use crate::types::{PeerInfo, PeerUpdateData, UiUpdate};
use anyhow::Result;
use dashmap::DashMap;
use reth::chainspec::ChainSpec;
use reth::primitives::{Block, Head, PooledTransaction, TransactionSigned};
use reth::revm::revm::primitives::B256;
use reth::revm::revm::primitives::alloy_primitives::Sealable;
use reth::tasks::TaskExecutor;
use reth_eth_wire::{
GetBlockBodies, GetBlockHeaders, GetPooledTransactions,
    NewPooledTransactionHashes, PooledTransactions, Status,
};
use reth_network::p2p::error::RequestError;
use reth_network::p2p::headers::client::HeadersDirection;
use reth_network::transactions::NetworkTransactionEvent;
use reth_network::types::BlockHashOrNumber;
use reth_network::{NetworkHandle, PeerRequest};
use reth_network_api::{
    NetworkEvent, PeerId,
    events::{PeerEvent, SessionInfo},
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::spawn;
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tracing::{debug, error, info, trace, warn};

#[derive(Debug, Clone)]
pub struct PeerSessionInfo {
    #[allow(dead_code)]
    status: Arc<Status>,
    session_info: Arc<SessionInfo>,
}

#[derive(Debug)]
pub struct EthP2PHandler {
    chain_spec: Arc<ChainSpec>,
    network_handle: NetworkHandle,
    pub peers: Arc<DashMap<PeerId, PeerSessionInfo>>,
    current_head: Head,
    decoded_tx_sender: UnboundedSender<Arc<TransactionSigned>>,
    ui_tx: UnboundedSender<UiUpdate>,
}

impl EthP2PHandler {
    pub fn new(
        chain_spec: Arc<ChainSpec>,
        network_handle: NetworkHandle,
        peers: Arc<DashMap<PeerId, PeerSessionInfo>>,
        initial_head: Head,
        decoded_tx_sender: UnboundedSender<Arc<TransactionSigned>>,
        ui_tx: UnboundedSender<UiUpdate>,
    ) -> Self {
        info!(target: "crawler::network", "Initializing EthP2PHandler.");
        Self {
            chain_spec,
            network_handle,
            peers,
            current_head: initial_head,
            decoded_tx_sender,
            ui_tx,
        }
    }

    fn on_session_established(&self, session_info: Arc<SessionInfo>) {
        let peer_id = session_info.peer_id;
        info!(target: "crawler::network", %peer_id, client=%session_info.client_version, "Session established...");

        let peer_info_struct = PeerSessionInfo {
            status: session_info.status.clone(),
            session_info: Arc::clone(&session_info),
        };
        self.peers.insert(peer_id, peer_info_struct);

        println!("[DEBUG] EthP2PHandler: Peer added! New peer count: {}", self.peers.len());

        let connected_peers_info: Vec<PeerInfo> = self
            .peers
            .iter()
            .map(|entry| PeerInfo {
                id: *entry.key(),
                client_version: entry.value().session_info.client_version.to_string(),
            })
            .collect();
        
        info!(target: "crawler::network", %peer_id, total_peers = connected_peers_info.len(), "Validated peer added to active set.");

        let update_data = PeerUpdateData {
            connected_peers: connected_peers_info,
            timestamp: Instant::now(),
        };
        if self.ui_tx.send(UiUpdate::PeerUpdate(update_data)).is_err() {
            warn!(target: "crawler::network", "Failed to send peer update to UI.");
        }
    }

    pub async fn handle_network_event_wrapper(&self, event: NetworkEvent) -> Result<()> {
        trace!(target: "crawler::handler", "Received NetworkEvent: {:?}", event);

        match event {
            NetworkEvent::Peer(peer_event) => {
                self.handle_peer_event(peer_event).await?;
            }
            NetworkEvent::ActivePeerSession { info, .. } => {
                self.on_session_established(info.into());
            }
        }
        Ok(())
    }

    pub async fn handle_peer_event(&self, event: PeerEvent) -> Result<()> {
        match event {
            PeerEvent::SessionEstablished(session_info) => {
                self.on_session_established(Arc::new(session_info));
            }
            PeerEvent::SessionClosed { peer_id, reason } => {
                info!(target: "crawler::network", %peer_id, ?reason, "Session closed");
                self.peers.remove(&peer_id);
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn handle_transaction_event(&self, event: NetworkTransactionEvent) -> Result<()> {
        match event {
            NetworkTransactionEvent::IncomingTransactions { peer_id, msg } => {
                let signed_transactions = msg.0;
                info!(target: "crawler::mempool", %peer_id, count = signed_transactions.len(), "Received full SIGNED transactions broadcast");
                let sender_clone = self.decoded_tx_sender.clone();
                for tx_signed_arc in signed_transactions.into_iter() {
                    trace!(target: "crawler::tx", tx_hash=%tx_signed_arc.hash(), "Processing directly received TransactionSigned.");
                    let tx_to_send = tx_signed_arc.clone();
                    if let Err(e) = sender_clone.send(tx_to_send.into()) {
                        error!(target: "crawler::tx", %peer_id, "Failed to send DIRECTLY received TransactionSigned: {}. Receiver likely dropped.", e);
                    } else {
                        debug!(target: "crawler::tx", %peer_id, tx_hash=%tx_signed_arc.hash(), "Successfully forwarded DIRECTLY received TransactionSigned to processor task.");
                    }
                }
            }
            NetworkTransactionEvent::IncomingPooledTransactionHashes { peer_id, msg } => {
                let hashes: Vec<B256> = match msg {
                    NewPooledTransactionHashes::Eth66(h) => h.0,
                    NewPooledTransactionHashes::Eth68(h) => h.hashes,
                };
                info!(target: "crawler::mempool", %peer_id, count = hashes.len(), "Received transaction hashes broadcast");
                if !hashes.is_empty() {
                    let request_payload = GetPooledTransactions(hashes.clone());
                    let (response_tx, response_rx) =
                        oneshot::channel::<Result<PooledTransactions, RequestError>>();
                    let peer_request = PeerRequest::GetPooledTransactions {
                        request: request_payload,
                        response: response_tx,
                    };
                    self.network_handle.send_request(peer_id, peer_request);
                    let sender_clone = self.decoded_tx_sender.clone();
                    spawn(async move {
                        match response_rx.await {
                            Ok(Ok(response_msg)) => {
                                let received_pooled_txs = response_msg.0;
                                info!(target: "crawler::mempool", %peer_id, count = received_pooled_txs.len(), "Received PooledTransactions RESPONSE");
                                for pooled_tx_arc in received_pooled_txs.into_iter() {
                                    let received_hash = pooled_tx_arc.hash();
                                    let pooled_tx_ref: &PooledTransaction = &pooled_tx_arc;
                                    let pooled_tx: PooledTransaction = pooled_tx_ref.clone();
                                    let tx_signed: TransactionSigned = pooled_tx.into();
                                    if tx_signed.hash() != received_hash {
                                        warn!(target: "crawler::tx", received_hash=%received_hash, computed_hash=%tx_signed.hash(), "Hash mismatch on requested tx!");
                                    }
                                    let tx_signed_arc = Arc::new(tx_signed);
                                    if let Err(e) = sender_clone.send(tx_signed_arc) {
                                        error!(target: "crawler::tx", %peer_id, "Failed to send REQUESTED tx: {}. Receiver likely dropped.", e);
                                    } else {
                                        debug!(target: "crawler::tx", %peer_id, tx_hash=%received_hash, "Forwarded REQUESTED tx to processor.");
                                    }
                                }
                            }
                            Ok(Err(req_err)) => {
                                warn!(target: "crawler::network", %peer_id, ?req_err, "GetPooledTransactions request failed")
                            }
                            Err(recv_err) => {
                                warn!(target: "crawler::network", %peer_id, %recv_err, "Failed to receive GetPooledTransactions response")
                            }
                        }
                    });
                }
            }
            NetworkTransactionEvent::GetPooledTransactions {
                peer_id,
                request,
                response,
            } => {
                debug!(target: "crawler::network", %peer_id, count = request.0.len(), "Ignoring incoming GetPooledTransactions request");
                let _ = response.send(Ok(PooledTransactions(vec![])));
            }
            _ => {
                debug!(target: "crawler::network", "Unhandled NetworkTransactionEvent: {:?}", event);
            }
        }
        Ok(())
    }
}

pub fn spawn_block_poller(
    executor: &TaskExecutor,
    network_handle: NetworkHandle,
    peers: Arc<DashMap<PeerId, PeerSessionInfo>>,
    block_sender: UnboundedSender<Block>,
) {
    let poller_task = async move {
        println!("[INFO] crawler::block-poller: Starting P2P block poller task...");
        let mut interval = tokio::time::interval(Duration::from_secs(5));

        let mut last_seen_block_number: u64 = 23_290_350;

        loop {
            interval.tick().await;

            if let Some(peer_entry) = peers.iter().next() {
                let peer_id = *peer_entry.key();
                let target_block = last_seen_block_number + 1;

                println!("[INFO] crawler::block-poller: Requesting block #{} from peer {}", target_block, peer_id);

                let (header_tx, header_rx) = oneshot::channel();
                let get_headers_req = PeerRequest::GetBlockHeaders {
                    request: GetBlockHeaders {
                        start_block: BlockHashOrNumber::Number(target_block),
                        limit: 1,
                        skip: 0,
                        direction: HeadersDirection::Rising,
                    },
                    response: header_tx,
                };
                network_handle.send_request(peer_id, get_headers_req);

                if let Ok(Ok(Ok(headers_res))) = tokio::time::timeout(Duration::from_secs(5), header_rx).await {
                    if let Some(header) = headers_res.0.into_iter().next() {
                        let block_hash = header.clone().seal_slow().hash();
                        let (body_tx, body_rx) = oneshot::channel();
                        let get_bodies_req = PeerRequest::GetBlockBodies {
                            request: GetBlockBodies(vec![block_hash]),
                            response: body_tx,
                        };
                        network_handle.send_request(peer_id, get_bodies_req);

                        if let Ok(Ok(Ok(bodies_res))) = tokio::time::timeout(Duration::from_secs(5), body_rx).await {
                            if let Some(body) = bodies_res.0.into_iter().next() {
                                let full_block = Block { header, body };
                                println!("[INFO] crawler::block-poller: ✅ Successfully fetched block #{}", full_block.number);
                                last_seen_block_number = full_block.number;
                                if block_sender.send(full_block).is_err() {
                                    println!("[ERROR] crawler::block-poller: Block processor channel closed.");
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    println!("[WARN] crawler::block-poller: Request to peer {} for block #{} timed out or failed", peer_id, target_block);
                }
            } else {
                println!("[INFO] crawler::block-poller: Waiting for peers to connect...");
            }
        }
    };

    executor.spawn(Box::pin(poller_task));
}
