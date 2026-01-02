//! HTTP server with REST API and WebSocket support.

use crate::store::EmailStore;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{delete, get};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// Embedded HTML UI.
const INDEX_HTML: &str = include_str!("../public/index.html");

#[derive(Clone)]
struct AppState {
    store: Arc<EmailStore>,
    email_notify: broadcast::Sender<()>,
}

/// Run the HTTP server.
pub async fn run_http_server(
    listener: TcpListener,
    store: Arc<EmailStore>,
    email_notify: broadcast::Sender<()>,
    mut shutdown: broadcast::Receiver<()>,
) {
    let state = AppState {
        store,
        email_notify,
    };

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/index.html", get(serve_index))
        .route("/emails", get(get_emails).delete(delete_all_emails))
        .route("/emails/{id}", delete(delete_email))
        .route("/ws", get(ws_handler))
        .fallback(not_found)
        .with_state(state);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.recv().await;
        })
        .await
        .unwrap();
}

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn get_emails(State(state): State<AppState>) -> Json<Vec<crate::MailRecord>> {
    Json(state.store.get_all())
}

async fn delete_all_emails(State(state): State<AppState>) -> StatusCode {
    state.store.clear();
    StatusCode::NO_CONTENT
}

async fn delete_email(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if state.store.remove(&id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::NOT_FOUND, "Email not found").into_response()
    }
}

async fn not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "Not found")
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(|socket| handle_websocket(socket, state))
}

async fn handle_websocket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();

    // Send initial state
    let emails = state.store.get_all();
    let msg = json!({ "event": "emails", "data": emails });
    if sender
        .send(Message::Text(msg.to_string().into()))
        .await
        .is_err()
    {
        return;
    }

    let mut email_rx = state.email_notify.subscribe();

    loop {
        tokio::select! {
            // Wait for email updates
            result = email_rx.recv() => {
                if result.is_err() {
                    break;
                }
                let emails = state.store.get_all();
                let msg = json!({ "event": "emails", "data": emails });
                if sender.send(Message::Text(msg.to_string().into())).await.is_err() {
                    break;
                }
            }
            // Handle incoming messages (mainly for ping/pong and close)
            msg = receiver.next() => {
                match msg {
                    None | Some(Ok(Message::Close(_)) | Err(_)) => break,
                    Some(Ok(Message::Ping(data))) => {
                        if sender.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}
