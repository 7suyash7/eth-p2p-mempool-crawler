use alloy_consensus::Transaction as AlloyTransactionTrait;
use alloy_consensus::transaction::SignerRecoverable;
use chrono::{DateTime, Utc};
use reth::revm::revm::primitives::{Address, B256, U256};
use reth_primitives::{
    Transaction as RethTransactionEnum, TransactionSigned, TxType,
};
use reth_primitives::transaction::SignedTransaction;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct TxAnalysisResult {
    pub hash: B256,
    pub tx_type: TxType,
    pub sender: Option<Address>,
    pub receiver: Option<Address>,
    pub value: U256,
    pub gas_limit: u64,
    pub gas_price_or_max_fee: Option<u128>,
    pub max_priority_fee: Option<u128>,
    pub input_len: usize,
    pub first_seen_at: DateTime<Utc>,
}

pub fn analyze_transaction(tx_signed: &TransactionSigned) -> TxAnalysisResult {
    let hash = tx_signed.hash();

    let sender = match tx_signed.recover_signer() {
        Ok(addr) => Some(addr),
        Err(e) => {
            warn!(tx_hash=%hash, "Failed to recover sender: {}", e);
            None
        }
    };

    let receiver = tx_signed.to();
    let value = tx_signed.value();
    let gas_limit = tx_signed.gas_limit();
    let input_len = tx_signed.input().len();
    let tx_type = tx_signed.tx_type();

    let unsigned_tx_enum = &tx_signed.clone().into_transaction().clone();

    let (gas_price_or_max_fee, max_priority_fee) = match unsigned_tx_enum {
        RethTransactionEnum::Legacy(_) | RethTransactionEnum::Eip2930(_) => {
            (tx_signed.gas_price(), None)
        }
        RethTransactionEnum::Eip1559(_)
        | RethTransactionEnum::Eip4844(_)
        | RethTransactionEnum::Eip7702(_) => (
            Some(tx_signed.max_fee_per_gas()),
            tx_signed.max_priority_fee_per_gas(),
        ),
    };

    TxAnalysisResult {
        hash: *hash,
        tx_type,
        sender,
        receiver,
        value,
        gas_limit,
        gas_price_or_max_fee,
        max_priority_fee,
        input_len,
        first_seen_at: Utc::now(),
    }
}
