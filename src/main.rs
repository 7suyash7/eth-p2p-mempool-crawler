// Declare modules
mod analysis;
mod config;
mod network;
mod types;
mod ui;

use anyhow::Result;
use futures_util::StreamExt; // Keep for event stream
use reth::chainspec::{ChainSpec, MAINNET};
use reth::network::transactions::NetworkTransactionEvent;
use reth::revm::revm::primitives::alloy_primitives::B512; // Keep for PeerId calc
use reth_discv4::{Discv4ConfigBuilder, NatResolver, NodeRecord}; // Keep for builders
use reth_network::{
    config::SecretKey as RethSecretKey, EthNetworkPrimitives, NetworkConfigBuilder,
    NetworkEventListenerProvider, NetworkManager, PeersConfig, PeersInfo, // Keep network types
};
use reth_network_api::PeerId; // Keep PeerId
use reth_primitives::{Head, TransactionSigned}; // Keep Head, TransactionSigned
use reth_provider::noop::NoopProvider; // Keep NoopProvider
use reth_tasks::TaskManager;
use secp256k1::Secp256k1; // Keep for key loading
use std::sync::Arc;
use tokio::{
    signal,
    sync::mpsc::{self, unbounded_channel}, // keep mpsc
};
use tracing::{debug, error, info, trace, warn}; // keep tracing

// Import items from our new modules
use crate::{
    analysis::TxAnalysisResult, // Only need this if main deals with it directly (it doesn't)
    config::{load_config, load_or_generate_key, parse_bootnodes, setup_logging, Config},
    network::EthP2PHandler,
    types::UiUpdate, // Import the UI message type
    ui::run_ui,      // Import the UI entry point
};

#[tokio::main]
async fn main() -> Result<()> {
    // --- Load Configuration & Logging ---
    // (Error handling will propagate via ?)
    let app_config = load_config()?;
    setup_logging(app_config.debug_logging);
    info!("🚀 Starting Ethereum P2P Crawler...");
    info!("Loaded configuration: {:?}", app_config);

    // --- Key Management ---
    let secret_key: RethSecretKey = load_or_generate_key(app_config.node_key_file.clone())?; // Clone Option<PathBuf>
    let secp = Secp256k1::new();
    let public_key = secret_key.public_key(&secp);
    let serialized_pk_bytes = public_key.serialize_uncompressed();
    let our_peer_id: PeerId = B512::from_slice(&serialized_pk_bytes[1..65]);
    info!("🔑 Our Peer ID: {}", our_peer_id);

    // --- Chain Specification ---
    let chain_spec: Arc<ChainSpec> = MAINNET.clone();
    info!("⛓️ Using Chain Spec: {}", chain_spec.chain);

    // --- Bootnodes ---
    let bootnodes: Vec<NodeRecord> = parse_bootnodes(app_config.bootnodes.clone())?; // Clone Option<Vec<String>>
    if bootnodes.is_empty() {
        warn!("No bootnodes specified or found! Peer discovery might fail.");
    } else {
        info!("🌳 Using {} bootnodes", bootnodes.len());
    }

    // --- Task Executor ---
    let tokio_handle = tokio::runtime::Handle::current();
    let task_manager = TaskManager::new(tokio_handle);
    let executor = task_manager.executor();
    info!("Task executor created.");

    // --- Build Network Components ---
    let mut discv4_builder = Discv4ConfigBuilder::default();
    discv4_builder.add_boot_nodes(bootnodes.clone()); // Clone Vec<NodeRecord>
    info!("🔍 Discv4 behaviour configured.");

    let peers_config = PeersConfig::default()
        .with_max_outbound(app_config.max_peers_outbound)
        .with_max_inbound(app_config.max_peers_inbound);

    let config_builder: NetworkConfigBuilder<EthNetworkPrimitives> =
        NetworkConfigBuilder::new(secret_key)
            .listener_addr(app_config.p2p_listen_addr)
            .discovery_addr(app_config.discv4_listen_addr)
            .discovery(discv4_builder)
            .boot_nodes(bootnodes) // Pass Vec<NodeRecord> again
            .add_nat(Some(NatResolver::Upnp))
            .peer_config(peers_config);

    let client = NoopProvider::<ChainSpec>::new(chain_spec.clone());
    let network_config = config_builder.build(client);
    info!(
        "🔧 Network configured. RLPx TCP listening on {}. Discovery UDP listening on {}. Attempting UPnP NAT.",
        app_config.p2p_listen_addr, app_config.discv4_listen_addr
    );

    // --- Create Communication Channels ---
    let (tx_event_sender, mut tx_event_receiver) =
        mpsc::unbounded_channel::<NetworkTransactionEvent>();
    let (decoded_tx_sender, mut decoded_tx_receiver) =
        mpsc::unbounded_channel::<Arc<TransactionSigned>>();
    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiUpdate>(); // Use UiUpdate from types module

    // --- Initialize Network Manager ---
    let mut network_manager = NetworkManager::new(network_config).await?;
    network_manager.set_transactions(tx_event_sender); // Setup the internal tx event forwarding
    let network_handle = network_manager.handle().clone();
    info!(
        "🌐 Network Manager created. Initial peer count: {}",
        network_handle.num_connected_peers()
    );
    // Note: Sending initial peer count to UI here is tricky timing-wise.
    // The handler will send updates as peers actually connect.

    // --- Initialize Custom P2P Handler ---
    let initial_head = Head::default(); // Keep using default head for now
    let event_handler = EthP2PHandler::new(
        chain_spec.clone(),
        network_handle.clone(),
        initial_head,
        decoded_tx_sender.clone(),
        ui_tx.clone(), // Pass the UI sender
    );
    let handler_arc = Arc::new(event_handler);

    // --- Spawn Core Tasks ---
    let task_executor = &executor; // Use a reference to avoid multiple moves/clones if not needed

    // 1. Event Handler Task (Handles Peer Events -> Sends PeerUpdates to UI)
    let handler_clone_for_events = Arc::clone(&handler_arc);
    task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::events", "EVENT HANDLER TASK STARTED");
        let mut events = handler_clone_for_events.network_handle().event_listener();
        loop {
            if let Some(event) = events.next().await {
                trace!(target: "crawler::events", ?event, "Received network event object.");
                if let Err(e) = handler_clone_for_events
                    .handle_network_event_wrapper(event)
                    .await
                {
                    error!(target: "crawler::events", "Error handling network event: {}", e);
                }
            } else {
                warn!(target: "crawler::events", "Network event stream finished unexpectedly!");
                break; // Exit loop if stream ends
            }
        }
    }));
    info!("Spawned Peer Event Handler task.");

    // 2. Transaction Event Handler Task (Handles Network Tx Events -> Sends Decoded Txs)
    let handler_clone_for_tx = Arc::clone(&handler_arc);
    task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::tx", "TX HANDLER TASK STARTED");
        loop {
            if let Some(event) = tx_event_receiver.recv().await {
                // Removed trace log from here for less noise
                if let Err(e) = handler_clone_for_tx.handle_transaction_event(event).await {
                    error!(target: "crawler::tx", "Error handling transaction event: {}", e);
                }
            } else {
                warn!(target: "crawler::tx", "Transaction event stream finished unexpectedly!");
                break; // Exit loop if stream ends
            }
        }
    }));
    info!("Spawned Transaction Event Handler task.");

    // 3. Decoded Transaction Processor Task (Analyzes Txs -> Sends TxAnalysisResult to UI)
    let processor_ui_tx = ui_tx.clone();
    task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::processor", "Starting decoded transaction processor task...");
        while let Some(tx_signed_arc) = decoded_tx_receiver.recv().await {
            let analysis_result = analysis::analyze_transaction(&tx_signed_arc);

            // Log analysis to file (optional)
            trace!(target: "crawler::processor", tx_hash = %analysis_result.hash, "Analyzed tx, sending to UI.");

            // Send analysis to UI task
            if let Err(e) = processor_ui_tx.send(UiUpdate::NewTx(Box::new(analysis_result))) {
                error!(target: "crawler::processor", "Failed to send tx update to UI: {}. Receiver likely dropped.", e);
                break; // Exit loop if UI is gone
            }
        }
        info!(target: "crawler::processor", "Decoded transaction processor task finished.");
    }));
    info!("Spawned Decoded Transaction Processor task.");

    // 4. Core Network Task (Runs the NetworkManager)
    let network_manager_handle = task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::netmgr", "Starting core network task...");
        network_manager.await; // Drive the NetworkManager event loop
        error!(target: "crawler::netmgr", "Core network task finished unexpectedly!");
    }));
    info!("Spawned Core Network task.");

    // 5. UI Task (Receives updates, Draws TUI)
    let ui_task_handle = task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::ui", "UI TASK STARTED");
        if let Err(e) = run_ui(ui_rx).await { // Pass the receiver end of UI channel
            error!(target: "crawler::ui", "UI task error: {}", e);
        }
        info!(target: "crawler::ui", "UI task finished.");
    }));
    info!("Spawned UI task.");


    // --- Keep Alive & Handle Shutdown ---
    info!("✅ Crawler is running! Press Ctrl+C to shut down.");
    signal::ctrl_c().await?; // Wait for Ctrl+C
    info!("🛑 Ctrl+C received, initiating shutdown...");

    // Send shutdown signal to UI task first
    if let Err(e) = ui_tx.send(UiUpdate::Shutdown) {
         error!(target: "crawler::main", "Failed to send shutdown signal to UI task: {}", e);
    }
    // Wait briefly for UI to potentially clean up terminal (optional but nice)
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    // TaskManager's Drop impl should signal tasks to shut down gracefully
    // Or use task_manager.shutdown() if available/needed
    drop(task_manager);

    // Optionally await tasks to ensure they finish (or timeout)
    // let _ = tokio::join!(ui_task_handle, network_manager_handle); // Example, might need more complex handling

    info!("Shutdown complete.");
    Ok(())
}