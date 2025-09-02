use axum::{
  extract::{
      ws::{Message, WebSocket, WebSocketUpgrade},
      Path, State,
  },
  http::StatusCode,
  response::IntoResponse,
  routing::get,
  Json, Router,
};
use chrono::{DateTime, Utc};
use tokio::sync::broadcast;
use serde::Serialize;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Clone)]
pub struct AppState {
  pub tx_broadcaster: broadcast::Sender<String>,
  pub db_pool: PgPool,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct ApiTransaction {
  pub hash: String,
  pub tx_type: i16,
  pub sender: Option<String>,
  pub receiver: Option<String>,
  pub value_wei: String,
  pub gas_limit: i64,
  pub gas_price_or_max_fee_wei: Option<String>,
  pub max_priority_fee_wei: Option<String>,
  pub input_len: i32,
  pub first_seen_at: DateTime<Utc>,
}

async fn websocket_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
  ws.on_upgrade(|socket| websocket(socket, state))
}

async fn websocket(mut socket: WebSocket, state: Arc<AppState>) {
  let mut rx = state.tx_broadcaster.subscribe();
  info!("New WebSocket client connected");

  loop {
      match rx.recv().await {
          Ok(tx_json) => {
              if socket.send(Message::Text(tx_json)).await.is_err() {
                  info!("WebSocket client disconnected");
                  break;
              }
          }
          Err(e) => {
              warn!("Error receiving from broadcast channel: {}", e);
              break;
          }
      }
  }
}

/// Handler to get a single transaction by its hash.
async fn get_transaction_by_hash(
  State(state): State<Arc<AppState>>,
  Path(hash): Path<String>,
) -> impl IntoResponse {
  let query = "SELECT * FROM transactions WHERE hash = $1";
  match sqlx::query_as::<_, ApiTransaction>(query)
      .bind(&hash)
      .fetch_one(&state.db_pool)
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
async fn get_latest_transactions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
  let query = "SELECT * FROM transactions ORDER BY first_seen_at DESC LIMIT 10";
  match sqlx::query_as::<_, ApiTransaction>(query)
      .fetch_all(&state.db_pool)
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

pub fn create_router(app_state: Arc<AppState>) -> Router {
  info!(target: "crawler::api", "Creating API router");
  Router::new()
      .route("/ws", get(websocket_handler))
      .route("/tx/:hash", get(get_transaction_by_hash))
      .route("/txs/latest", get(get_latest_transactions))
      .with_state(app_state)
}
