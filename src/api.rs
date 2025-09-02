use axum::{
  Json, Router,
  extract::{Path, State},
  http::StatusCode,
  response::IntoResponse,
  routing::get,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use tracing::{info, warn};

#[derive(Serialize, sqlx::FromRow)]
pub struct ApiTransaction {
  hash: String,
  tx_type: i16,
  sender: Option<String>,
  receiver: Option<String>,
  value_wei: String,
  gas_limit: i64,
  gas_price_or_max_fee_wei: Option<String>,
  max_priority_fee_wei: Option<String>,
  input_len: i32,
  first_seen_at: DateTime<Utc>,
}

/// Handler to get a single transaction by its hash.
async fn get_transaction_by_hash(
  State(pool): State<PgPool>,
  Path(hash): Path<String>,
) -> impl IntoResponse {
  let query = "SELECT * FROM transactions WHERE hash = $1";
  match sqlx::query_as::<_, ApiTransaction>(query)
      .bind(&hash)
      .fetch_one(&pool)
      .await
  {
      Ok(tx) => (StatusCode::OK, Json(tx)).into_response(),
      Err(sqlx::Error::RowNotFound) => (
          StatusCode::NOT_FOUND,
          format!("Transaction not found: {}", hash),
      )
          .into_response(),
      Err(e) => {
          warn!(target: "crawler::api", "Database error fetching tx {}: {}", hash, e);
          (
              StatusCode::INTERNAL_SERVER_ERROR,
              "Database error".to_string(),
          )
              .into_response()
      }
  }
}

/// Handler to get the 10 most recently seen transactions.
async fn get_latest_transactions(State(pool): State<PgPool>) -> impl IntoResponse {
  let query = "SELECT * FROM transactions ORDER BY first_seen_at DESC LIMIT 10";
  match sqlx::query_as::<_, ApiTransaction>(query)
      .fetch_all(&pool)
      .await
  {
      Ok(txs) => (StatusCode::OK, Json(txs)).into_response(),
      Err(e) => {
          warn!(target: "crawler::api", "Database error fetching latest txs: {}", e);
          (
              StatusCode::INTERNAL_SERVER_ERROR,
              "Database error".to_string(),
          )
              .into_response()
      }
  }
}

pub fn create_router(pool: PgPool) -> Router {
  info!(target: "crawler::api", "Creating API router");
  Router::new()
      .route("/tx/:hash", get(get_transaction_by_hash))
      .route("/txs/latest", get(get_latest_transactions))
      .with_state(pool)
}
