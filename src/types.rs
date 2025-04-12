use crate::analysis::TxAnalysisResult;
use reth_network_api::PeerId;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub id: PeerId,
    pub client_version: String,
}

#[derive(Debug, Clone)]
pub struct PeerUpdateData {
    pub connected_peers: Vec<PeerInfo>,
    pub timestamp: Instant,
}

#[derive(Debug)]
pub enum UiUpdate {
    PeerUpdate(PeerUpdateData),
    NewTx(Box<TxAnalysisResult>),
    Shutdown,
}
