use reth::revm::revm::primitives::{Address, B256, U256};
use reth_primitives::{
    Transaction as RethTransactionEnum, // Renamed enum import
    TransactionSigned, TxType,
    transaction::SignedTransaction, // Trait for recover_signer
};
use alloy_consensus::Transaction as AlloyTransactionTrait; // Trait for other methods
use tracing::warn;
// Removed Arc as it's not used directly here anymore

// Make struct public
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
}

// Make function public
pub fn analyze_transaction(tx_signed: &TransactionSigned) -> TxAnalysisResult {
  let hash = tx_signed.hash(); // Gets &B256

  // recover_signer comes from SignedTransaction trait impl on TransactionSigned
  let sender = match tx_signed.recover_signer() {
      Ok(addr) => Some(addr),
      Err(e) => {
          warn!(tx_hash=%hash, "Failed to recover sender: {}", e); // Use calculated hash in log
          None
      }
  };

  // Use methods from AlloyTransactionTrait directly on tx_signed
  let receiver = tx_signed.to();
  let value = tx_signed.value();
  let gas_limit = tx_signed.gas_limit();
  let input_len = tx_signed.input().len();
  let tx_type = tx_signed.tx_type(); // This specific method might be inherent

  // Use inner enum ref just for matching logic
  let unsigned_tx_enum = &tx_signed.transaction();

  let (gas_price_or_max_fee, max_priority_fee) = match unsigned_tx_enum {
      RethTransactionEnum::Legacy(_) | RethTransactionEnum::Eip2930(_) => {
          (tx_signed.gas_price(), None) // Use AlloyTransactionTrait methods
      }
      RethTransactionEnum::Eip1559(_) | RethTransactionEnum::Eip4844(_) | RethTransactionEnum::Eip7702(_) => {
          (
              Some(tx_signed.max_fee_per_gas()), // Use AlloyTransactionTrait method
              tx_signed.max_priority_fee_per_gas(), // Use AlloyTransactionTrait method
          )
      }
  };

  TxAnalysisResult {
      hash: *hash, // Dereference &B256 to B256
      tx_type,
      sender,
      receiver,
      value,
      gas_limit,
      gas_price_or_max_fee,
      max_priority_fee,
      input_len,
  }
}