mod analysis;
mod api;
mod config;
mod db;
mod network;
mod types;
mod ui;

use crate::{
    api::{create_router, AppState, ApiTransaction},
    config::{load_config, load_or_generate_key, parse_bootnodes, setup_logging},
    network::EthP2PHandler,
    types::UiUpdate,
    ui::run_ui,
};
use anyhow::Result;
use futures_util::StreamExt;
use reth::chainspec::{ChainSpec, MAINNET};
use reth::network::transactions::NetworkTransactionEvent;
use reth::revm::revm::primitives::alloy_primitives::B512;
use reth_discv4::{Discv4ConfigBuilder, NatResolver, NodeRecord};
use reth_network::{
    EthNetworkPrimitives, NetworkConfigBuilder, NetworkEventListenerProvider, NetworkManager,
    PeersConfig, PeersInfo, config::SecretKey as RethSecretKey,
};
use reth_network_api::PeerId;
use reth_primitives::{Head, TransactionSigned};
use reth_provider::noop::NoopProvider;
use reth_tasks::TaskManager;
use secp256k1::Secp256k1;
use std::sync::Arc;
use tokio::{
    signal,
    sync::{broadcast, mpsc::{self}},
};
use tracing::{error, info, trace, warn};

#[tokio::main]
async fn main() -> Result<()> {
    let app_config = load_config()?;
    setup_logging(app_config.debug_logging);
    info!("🚀 Starting Ethereum P2P Crawler...");
    println!("Loaded configuration: {:?}", app_config);

    let db_pool = db::create_pool(&app_config.database_url).await?;
    let (tx_broadcaster, _)  = broadcast::channel::<String>(100);
    let app_state = Arc::new(AppState {
        tx_broadcaster: tx_broadcaster.clone(),
        db_pool: db_pool.clone(),
    });

    let secret_key: RethSecretKey = load_or_generate_key(app_config.node_key_file.clone())?;
    let secp = Secp256k1::new();
    let public_key = secret_key.public_key(&secp);
    let serialized_pk_bytes = public_key.serialize_uncompressed();
    let our_peer_id: PeerId = B512::from_slice(&serialized_pk_bytes[1..65]);
    info!("🔑 Our Peer ID: {}", our_peer_id);

    let chain_spec: Arc<ChainSpec> = MAINNET.clone();
    info!("⛓️ Using Chain Spec: {}", chain_spec.chain);

    let bootnodes: Vec<NodeRecord> = parse_bootnodes(app_config.bootnodes.clone())?;
    if bootnodes.is_empty() {
        warn!("No bootnodes specified or found! Peer discovery might fail.");
    } else {
        info!("🌳 Using {} bootnodes", bootnodes.len());
    }

    let tokio_handle = tokio::runtime::Handle::current();
    let task_manager = TaskManager::new(tokio_handle);
    let executor = task_manager.executor();
    info!("Task executor created.");

    let mut discv4_builder = Discv4ConfigBuilder::default();
    discv4_builder.add_boot_nodes(bootnodes.clone());
    info!("🔍 Discv4 behaviour configured.");

    let peers_config = PeersConfig::default()
        .with_max_outbound(app_config.max_peers_outbound)
        .with_max_inbound(app_config.max_peers_inbound);

    let config_builder: NetworkConfigBuilder<EthNetworkPrimitives> =
        NetworkConfigBuilder::new(secret_key)
            .listener_addr(app_config.p2p_listen_addr)
            .discovery_addr(app_config.discv4_listen_addr)
            .discovery(discv4_builder)
            .boot_nodes(bootnodes)
            .add_nat(Some(NatResolver::Upnp))
            .peer_config(peers_config);

    let client = NoopProvider::<ChainSpec>::new(chain_spec.clone());
    let network_config = config_builder.build(client);
    info!(
        "🔧 Network configured. RLPx TCP listening on {}. Discovery UDP listening on {}. Attempting UPnP NAT.",
        app_config.p2p_listen_addr, app_config.discv4_listen_addr
    );

    let (tx_event_sender, mut tx_event_receiver) =
        mpsc::unbounded_channel::<NetworkTransactionEvent>();
    let (decoded_tx_sender, mut decoded_tx_receiver) =
        mpsc::unbounded_channel::<Arc<TransactionSigned>>();

    let (db_writer_tx, mut db_writer_rx) = mpsc::unbounded_channel::<analysis::TxAnalysisResult>();
    let (ui_tx, ui_rx) = mpsc::unbounded_channel::<UiUpdate>();

    let mut network_manager = NetworkManager::new(network_config).await?;
    network_manager.set_transactions(tx_event_sender);
    let network_handle = network_manager.handle().clone();
    info!(
        "🌐 Network Manager created. Initial peer count: {}",
        network_handle.num_connected_peers()
    );

    let initial_head = Head::default();
    let event_handler = EthP2PHandler::new(
        chain_spec.clone(),
        network_handle.clone(),
        initial_head,
        decoded_tx_sender.clone(),
        ui_tx.clone(),
    );
    let handler_arc = Arc::new(event_handler);

    let task_executor = &executor;

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
                break;
            }
        }
    }));
    info!("Spawned Peer Event Handler task.");

    let handler_clone_for_tx = Arc::clone(&handler_arc);
    task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::tx", "TX HANDLER TASK STARTED");
        loop {
            if let Some(event) = tx_event_receiver.recv().await {
                if let Err(e) = handler_clone_for_tx.handle_transaction_event(event).await {
                    error!(target: "crawler::tx", "Error handling transaction event: {}", e);
                }
            } else {
                warn!(target: "crawler::tx", "Transaction event stream finished unexpectedly!");
                break;
            }
        }
    }));
    info!("Spawned Transaction Event Handler task.");

    task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::processor", "Starting decoded transaction processor task...");
        while let Some(tx_signed_arc) = decoded_tx_receiver.recv().await {
            let analysis_result = analysis::analyze_transaction(&tx_signed_arc);
            trace!(target: "crawler::processor", tx_hash = %analysis_result.hash, "Analyzed tx, sending to DB writer.");

            if let Err(e) = db_writer_tx.send(analysis_result) {
                error!(target: "crawler::processor", "Failed to send tx to DB writer: {}. Receiver likely dropped.", e);
                break;
            }
        }
        info!(target: "crawler::processor", "Decoded transaction processor task finished.");
    }));
    info!("Spawned Decoded Transaction Processor task.");

    let writer_ui_tx = ui_tx.clone();
    let db_pool_clone = db_pool.clone();
    let broadcaster = tx_broadcaster.clone();
    task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::db-writer", "Starting database writer task...");
        while let Some(tx) = db_writer_rx.recv().await {
            let api_tx = ApiTransaction {
                hash: tx.hash.to_string(),
                tx_type: tx.tx_type as i16,
                sender: tx.sender.map(|s| s.to_string()),
                receiver: tx.receiver.map(|r| r.to_string()),
                value_wei: tx.value.to_string(),
                gas_limit: tx.gas_limit as i64,
                gas_price_or_max_fee_wei: tx.gas_price_or_max_fee.map(|p| p.to_string()),
                max_priority_fee_wei: tx.max_priority_fee.map(|p| p.to_string()),
                input_len: tx.input_len as i32,
                first_seen_at: tx.first_seen_at,
            };

            let query_result = sqlx::query!(
                r#"
                INSERT INTO transactions (hash, tx_type, sender, receiver, value_wei, gas_limit, gas_price_or_max_fee_wei, max_priority_fee_wei, input_len, first_seen_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (hash) DO NOTHING
                "#,
                api_tx.hash,
                api_tx.tx_type,
                api_tx.sender,
                api_tx.receiver,
                api_tx.value_wei,
                api_tx.gas_limit,
                api_tx.gas_price_or_max_fee_wei,
                api_tx.max_priority_fee_wei,
                api_tx.input_len,
                api_tx.first_seen_at
            )
            .execute(&db_pool_clone)
            .await;

            if let Err(e) = query_result {
                warn!(target: "crawler::db-writer", "Failed to write tx {} to DB: {}", api_tx.hash, e);
            }

            if let Ok(tx_json) = serde_json::to_string(&api_tx) {
                if broadcaster.send(tx_json).is_err() {
                    trace!(target: "crawler::broadcaster", "No active WebSocket clients to broadcast to");
                }
            }

            if writer_ui_tx.send(UiUpdate::NewTx(Box::new(tx))).is_err() {
                error!(target: "crawler::db-writer", "Failed to send tx update to UI: receiver dropped.");
            }
        }
    }));
    info!("Spawned Database Writer task.");

    let network_manager_handle = task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::netmgr", "Starting core network task...");
        network_manager.await;
        error!(target: "crawler::netmgr", "Core network task finished unexpectedly!");
    }));
    info!("Spawned Core Network task.");

    let ui_task_handle = task_executor.spawn(Box::pin(async move {
        info!(target: "crawler::ui", "UI TASK STARTED");
        if let Err(e) = run_ui(ui_rx).await {
            error!(target: "crawler::ui", "UI task error: {}", e);
        }
        info!(target: "crawler::ui", "UI task finished.");
    }));
    info!("Spawned UI task.");

    task_executor.spawn(Box::pin(async move {
        let app_router = create_router(app_state); 
        let listener = tokio::net::TcpListener::bind("0.0.0.0:8000").await.unwrap();
        info!(target: "crawler::api", "✅ API server listening on http://0.0.0.0:8000");
        axum::serve(listener, app_router).await.unwrap();
    }));
    info!("Spawned API Server task.");

    info!("✅ Crawler is running! Press Ctrl+C to shut down.");
    signal::ctrl_c().await?;
    info!("🛑 Ctrl+C received, initiating shutdown...");

    if let Err(e) = ui_tx.send(UiUpdate::Shutdown) {
        error!(target: "crawler::main", "Failed to send shutdown signal to UI task: {}", e);
    }

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    drop(db_pool);
    drop(task_manager);

    let _ = tokio::join!(ui_task_handle, network_manager_handle);

    info!("Shutdown complete.");
    Ok(())
}
