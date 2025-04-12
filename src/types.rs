use crate::analysis::TxAnalysisResult;
use reth_network_api::PeerId;
use std::time::Instant;

// Info for a single connected peer to display in UI
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub id: PeerId,
    pub client_version: String,
    // Add more later: direction, address?
}

// Data sent when peer list changes
#[derive(Debug, Clone)]
pub struct PeerUpdateData {
    pub connected_peers: Vec<PeerInfo>, // The list of currently connected peers
    pub timestamp: Instant,
}

// Enum for all messages sent to the UI task
#[derive(Debug)]
pub enum UiUpdate {
    PeerUpdate(PeerUpdateData), // Use the new struct
    NewTx(Box<TxAnalysisResult>),
    Shutdown,
}