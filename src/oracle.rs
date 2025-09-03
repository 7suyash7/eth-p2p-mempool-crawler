use crate::api::ApiTransaction;
use serde::Serialize;
use std::collections::VecDeque;
use tokio::sync::{RwLock, broadcast};
use tracing::info;

const MAX_SAMPLES: usize = 1000;

#[derive(Debug, Serialize, Clone, Copy, Default)]
pub struct GasEstimates {
    pub slow: u128,
    pub standard: u128,
    pub fast: u128,
}

#[derive(Debug)]
pub struct GasOracle {
    recent_priority_fees: RwLock<VecDeque<u128>>,
    estimates: RwLock<GasEstimates>,
}

impl GasOracle {
    pub fn new() -> Self {
        Self {
            recent_priority_fees: RwLock::new(VecDeque::with_capacity(MAX_SAMPLES)),
            estimates: RwLock::new(GasEstimates::default()),
        }
    }

    pub async fn run_collector(&self, mut rx: broadcast::Receiver<String>) {
        info!(target: "crawler::oracle", "Gas Oracle collector task started");
        loop {
            if let Ok(tx_json) = rx.recv().await {
                if let Ok(tx) = serde_json::from_str::<ApiTransaction>(&tx_json) {
                    if let Some(prio_fee_str) = tx.max_priority_fee_wei {
                        if let Ok(prio_fee) = prio_fee_str.parse::<u128>() {
                            if prio_fee > 0 {
                                let mut fees = self.recent_priority_fees.write().await;
                                if fees.len() == MAX_SAMPLES {
                                    fees.pop_front();
                                }
                                fees.push_back(prio_fee);
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn run_calculator(&self) {
        info!(target: "crawler::oracle", "Gas Oracle calculator task started");
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            interval.tick().await;

            let fees = self.recent_priority_fees.read().await;
            if fees.len() < 20 {
                continue;
            }

            let mut samples: Vec<f64> = fees.iter().map(|&fee| fee as f64).collect();
            samples.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());

            let new_estimates = GasEstimates {
                slow: calculate_percentile(&samples, 0.2) as u128,
                standard: calculate_percentile(&samples, 0.5) as u128,
                fast: calculate_percentile(&samples, 0.8) as u128,
            };

            let mut estimates_writer = self.estimates.write().await;
            *estimates_writer = new_estimates;

            info!(target: "crawler::oracle", "Updated gas estimates: {:?}", *estimates_writer);
        }
    }

    pub async fn get_estimates(&self) -> GasEstimates {
        self.estimates.read().await.clone()
    }
}

fn calculate_percentile(sorted_data: &[f64], percentile: f64) -> f64 {
    if sorted_data.is_empty() {
        return 0.0;
    }
    let n = sorted_data.len();
    let index = percentile * (n - 1) as f64;
    let lower_index = index.floor() as usize;
    let upper_index = index.ceil() as usize;

    if lower_index == upper_index {
        return sorted_data[lower_index];
    }

    let weight = index - lower_index as f64;
    sorted_data[lower_index] * (1.0 - weight) + sorted_data[upper_index] * weight
}
