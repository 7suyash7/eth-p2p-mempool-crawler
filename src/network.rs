use crate::types::{PeerInfo, PeerUpdateData, UiUpdate};
use anyhow::Result;
use dashmap::DashMap;
use reth::chainspec::ChainSpec;
use reth::primitives::{Head, PooledTransaction, TransactionSigned};
use reth::revm::revm::primitives::B256;
use reth_eth_wire::{
    GetPooledTransactions, NewPooledTransactionHashes, PooledTransactions, Status,
};
use reth_network::p2p::error::RequestError;
use reth_network::transactions::NetworkTransactionEvent;
use reth_network::{NetworkHandle, PeerRequest};
use reth_network_api::{
    NetworkEvent, PeerId,
    events::{PeerEvent, SessionInfo},
};
use std::sync::Arc;
use std::time::Instant;
use tokio::spawn;
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tracing::{debug, error, info, trace, warn};

#[derive(Debug, Clone)]
struct PeerSessionInfo {
    #[allow(dead_code)]
    status: Arc<Status>,
    session_info: Arc<SessionInfo>,
}

#[derive(Debug)]
pub struct EthP2PHandler {
    chain_spec: Arc<ChainSpec>,
    network_handle: NetworkHandle,
    peers: Arc<DashMap<PeerId, PeerSessionInfo>>,
    current_head: Head,
    decoded_tx_sender: UnboundedSender<Arc<TransactionSigned>>,
    ui_tx: UnboundedSender<UiUpdate>,
}

impl EthP2PHandler {
    pub fn new(
        chain_spec: Arc<ChainSpec>,
        network_handle: NetworkHandle,
        initial_head: Head,
        decoded_tx_sender: UnboundedSender<Arc<TransactionSigned>>,
        ui_tx: UnboundedSender<UiUpdate>,
    ) -> Self {
        info!(target: "crawler::network", "Initializing EthP2PHandler.");
        Self {
            chain_spec,
            network_handle,
            peers: Arc::new(DashMap::new()),
            current_head: initial_head,
            decoded_tx_sender,
            ui_tx,
        }
    }

    pub async fn handle_network_event_wrapper(&self, event: NetworkEvent) -> Result<()> {
        trace!(target: "crawler::handler", "**** Received NetworkEvent in Handler: {:?}", event);
        match event {
            NetworkEvent::Peer(peer_event) => {
                self.handle_peer_event(peer_event).await?;
            }
            NetworkEvent::ActivePeerSession { info, .. } => {
                debug!(target: "crawler::network", peer_id=%info.peer_id, "Session confirmed active.");
            }
        }
        Ok(())
    }

    pub async fn handle_peer_event(&self, event: PeerEvent) -> Result<()> {
        match event {
            PeerEvent::SessionEstablished(session_info) => {
                let peer_id = session_info.peer_id;
                info!(target: "crawler::network", %peer_id, client=%session_info.client_version, version=%session_info.status.version, "Session established...");

                let peer_info_struct = PeerSessionInfo {
                    status: session_info.status.clone(),
                    session_info: Arc::new(session_info),
                };
                self.peers.insert(peer_id, peer_info_struct);
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
                debug!(target: "crawler::sender", "Sending PeerUpdate (Established): {} peers", update_data.connected_peers.len());
                if let Err(e) = self.ui_tx.send(UiUpdate::PeerUpdate(update_data)) {
                    warn!(target: "crawler::network", "Failed to send peer update to UI: {}", e);
                }
            }
            PeerEvent::SessionClosed { peer_id, reason } => {
                info!(target: "crawler::network", %peer_id, ?reason, "Session closed");
                let mut removed = false;
                if self.peers.remove(&peer_id).is_some() {
                    removed = true;
                }
                let connected_peers_info: Vec<PeerInfo> = self
                    .peers
                    .iter()
                    .map(|entry| PeerInfo {
                        id: *entry.key(),
                        client_version: entry.value().session_info.client_version.to_string(),
                    })
                    .collect();
                if removed {
                    info!(target: "crawler::network", %peer_id, total_peers = connected_peers_info.len(), "Peer removed from active set.");
                }
                let update_data = PeerUpdateData {
                    connected_peers: connected_peers_info,
                    timestamp: Instant::now(),
                };
                debug!(target: "crawler::sender", "Sending PeerUpdate (Closed): {} peers", update_data.connected_peers.len());
                if let Err(e) = self.ui_tx.send(UiUpdate::PeerUpdate(update_data)) {
                    warn!(target: "crawler::network", "Failed to send peer update to UI: {}", e);
                }
            }
            PeerEvent::PeerAdded(peer_id) => {
                debug!(target: "crawler::network::discovery", %peer_id, "Peer added to address book");
            }
            PeerEvent::PeerRemoved(peer_id) => {
                debug!(target: "crawler::network::discovery", %peer_id, "Peer removed from address book");
            }
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

    pub fn network_handle(&self) -> NetworkHandle {
        self.network_handle.clone()
    }
}
