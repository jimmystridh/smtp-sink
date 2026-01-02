//! HTTP server with REST API and WebSocket support.

use crate::store::{EmailQuery, EmailStorage};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// Embedded HTML UI.
const INDEX_HTML: &str = include_str!("../public/index.html");

#[derive(Clone)]
struct AppState {
    store: Arc<dyn EmailStorage>,
    email_notify: broadcast::Sender<()>,
}

/// Query parameters for email search.
#[derive(Debug, Deserialize, Default)]
struct EmailQueryParams {
    from: Option<String>,
    to: Option<String>,
    subject: Option<String>,
    since: Option<String>,
    until: Option<String>,
}

impl From<EmailQueryParams> for EmailQuery {
    fn from(p: EmailQueryParams) -> Self {
        Self {
            from: p.from,
            to: p.to,
            subject: p.subject,
            since: p.since,
            until: p.until,
        }
    }
}

/// Run the HTTP server.
pub async fn run_http_server(
    listener: TcpListener,
    store: Arc<dyn EmailStorage>,
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
        .route("/health", get(health_check))
        .route("/emails", get(get_emails).delete(delete_all_emails))
        .route("/emails/{id}", get(get_email).delete(delete_email))
        .route("/emails/{id}/attachments", get(list_attachments))
        .route("/emails/{id}/attachments/{filename}", get(get_attachment))
        .route("/emails/{id}/cid/{cid}", get(get_attachment_by_cid))
        .route("/ws", get(ws_handler))
        .fallback(not_found)
        .with_state(state);

    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.recv().await;
        })
        .await
    {
        tracing::error!("HTTP server error: {e}");
    }
}

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health_check(State(state): State<AppState>) -> Json<serde_json::Value> {
    let email_count = state.store.get_all().len();
    Json(json!({
        "status": "ok",
        "email_count": email_count
    }))
}

async fn get_emails(
    State(state): State<AppState>,
    Query(params): Query<EmailQueryParams>,
) -> Json<Vec<crate::MailRecord>> {
    let has_query = params.from.is_some()
        || params.to.is_some()
        || params.subject.is_some()
        || params.since.is_some()
        || params.until.is_some();

    if has_query {
        Json(state.store.query(&params.into()))
    } else {
        Json(state.store.get_all())
    }
}

async fn get_email(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    state.store.get_by_id(&id).map_or_else(
        || (StatusCode::NOT_FOUND, "Email not found").into_response(),
        |email| Json(email).into_response(),
    )
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

async fn list_attachments(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_by_id(&id) {
        Some(email) => {
            let attachments: Vec<_> = email
                .attachments
                .iter()
                .map(|a| {
                    json!({
                        "filename": a.filename,
                        "content_type": a.content_type,
                        "size": a.size,
                        "content_id": a.content_id,
                    })
                })
                .collect();
            Json(attachments).into_response()
        }
        None => (StatusCode::NOT_FOUND, "Email not found").into_response(),
    }
}

async fn get_attachment(
    State(state): State<AppState>,
    Path((id, filename)): Path<(String, String)>,
) -> Response {
    match state.store.get_attachment(&id, &filename) {
        Some(att) => {
            let content_type = att.content_type.clone();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, content_type),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{}\"", att.filename),
                    ),
                ],
                att.data,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "Attachment not found").into_response(),
    }
}

async fn get_attachment_by_cid(
    State(state): State<AppState>,
    Path((id, cid)): Path<(String, String)>,
) -> Response {
    match state.store.get_attachment_by_cid(&id, &cid) {
        Some(att) => {
            let content_type = att.content_type.clone();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, content_type)],
                att.data,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "Attachment not found").into_response(),
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
