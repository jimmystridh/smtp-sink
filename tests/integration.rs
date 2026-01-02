//! Integration tests for smtp-sink.

#![allow(clippy::similar_names)]

use futures_util::StreamExt;
use lettre::message::Mailbox;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{Message, SmtpTransport, Transport};
use reqwest::Client;
use serde_json::Value;
use smtp_sink::{start_sink, MailRecord, SinkOptions};
use std::time::Duration;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message as WsMessage};

fn create_transport(port: u16, secure: bool) -> SmtpTransport {
    let builder = SmtpTransport::builder_dangerous("127.0.0.1")
        .port(port)
        .timeout(Some(Duration::from_secs(10)));

    if secure {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let tls_params = TlsParameters::builder("localhost".to_string())
            .dangerous_accept_invalid_certs(true)
            .build()
            .unwrap();
        builder.tls(Tls::Wrapper(tls_params)).build()
    } else {
        builder.build()
    }
}

async fn send_email(port: u16, email: Message, secure: bool) {
    tokio::task::spawn_blocking(move || {
        let transport = create_transport(port, secure);
        transport.send(&email).unwrap();
    })
    .await
    .unwrap();
}

async fn try_send_email(port: u16, email: Message, secure: bool) -> Result<(), ()> {
    tokio::task::spawn_blocking(move || {
        let transport = create_transport(port, secure);
        transport.send(&email).map(|_| ()).map_err(|_| ())
    })
    .await
    .unwrap()
}

async fn get_emails(http_port: u16) -> Vec<MailRecord> {
    let client = Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{http_port}/emails"))
        .send()
        .await
        .unwrap();
    resp.json().await.unwrap()
}

async fn delete_all_emails(http_port: u16) -> u16 {
    let client = Client::new();
    let resp = client
        .delete(format!("http://127.0.0.1:{http_port}/emails"))
        .send()
        .await
        .unwrap();
    resp.status().as_u16()
}

async fn delete_email(http_port: u16, id: &str) -> u16 {
    let client = Client::new();
    let resp = client
        .delete(format!(
            "http://127.0.0.1:{}/emails/{}",
            http_port,
            urlencoding::encode(id)
        ))
        .send()
        .await
        .unwrap();
    resp.status().as_u16()
}

async fn get_html(http_port: u16, path: &str) -> (u16, String) {
    let client = Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{http_port}{path}"))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap();
    (status, body)
}

#[tokio::test]
async fn test_returns_empty_array_initially() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(5),
        ..Default::default()
    })
    .await
    .unwrap();

    let emails = get_emails(servers.http_addr.port()).await;
    assert!(emails.is_empty());

    servers.stop().await;
}

#[tokio::test]
async fn test_accepts_message_and_exposes_via_http() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(5),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("Alice <alice@example.com>".parse().unwrap())
        .to("Bob <bob@example.com>".parse().unwrap())
        .subject("Hello")
        .body("Hi Bob!".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject.as_deref(), Some("Hello"));
    assert!(emails[0].text.as_ref().unwrap().contains("Hi Bob!"));
    assert!(emails[0].from.contains("alice@example.com"));
    assert!(emails[0].to.contains(&"bob@example.com".to_string()));
    assert!(!emails[0].id.is_empty());
    assert!(!emails[0].date.is_empty());

    servers.stop().await;
}

#[tokio::test]
async fn test_delete_emails_clears_stored_emails() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(5),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("test@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("To be deleted")
        .body("This will be deleted".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);

    let status = delete_all_emails(servers.http_addr.port()).await;
    assert_eq!(status, 204);

    let emails = get_emails(servers.http_addr.port()).await;
    assert!(emails.is_empty());

    servers.stop().await;
}

#[tokio::test]
async fn test_serves_web_ui_on_root() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let (status, body) = get_html(servers.http_addr.port(), "/").await;
    assert_eq!(status, 200);
    assert!(body.to_lowercase().contains("<!doctype html>"));
    assert!(body.contains("smtp-sink"));

    servers.stop().await;
}

#[tokio::test]
async fn test_serves_web_ui_on_index_html() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let (status, body) = get_html(servers.http_addr.port(), "/index.html").await;
    assert_eq!(status, 200);
    assert!(body.to_lowercase().contains("<!doctype html>"));

    servers.stop().await;
}

#[tokio::test]
async fn test_returns_404_for_unknown_paths() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let (status, _) = get_html(servers.http_addr.port(), "/unknown").await;
    assert_eq!(status, 404);

    servers.stop().await;
}

#[tokio::test]
async fn test_handles_multiple_recipients() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(5),
        ..Default::default()
    })
    .await
    .unwrap();

    let alice: Mailbox = "alice@example.com".parse().unwrap();
    let bob: Mailbox = "bob@example.com".parse().unwrap();
    let charlie: Mailbox = "charlie@example.com".parse().unwrap();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to(alice)
        .to(bob)
        .to(charlie)
        .subject("Group email")
        .body("Hello everyone!".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].to.contains(&"alice@example.com".to_string()));
    assert!(emails[0].to.contains(&"bob@example.com".to_string()));
    assert!(emails[0].to.contains(&"charlie@example.com".to_string()));

    servers.stop().await;
}

#[tokio::test]
async fn test_handles_html_emails() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(5),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("HTML Email")
        .header(lettre::message::header::ContentType::TEXT_HTML)
        .body("<h1>Hello</h1><p>This is <strong>HTML</strong> content.</p>".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].html.as_ref().unwrap().contains("<h1>Hello</h1>"));

    servers.stop().await;
}

#[tokio::test]
async fn test_includes_headers_in_email_record() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(5),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Headers test")
        .body("Testing headers".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(!emails[0].headers.is_empty());

    servers.stop().await;
}

#[tokio::test]
async fn test_limits_stored_emails_to_max() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(3),
        ..Default::default()
    })
    .await
    .unwrap();

    for i in 1..=5 {
        let email = Message::builder()
            .from("sender@example.com".parse().unwrap())
            .to("recipient@example.com".parse().unwrap())
            .subject(format!("Email {i}"))
            .body(format!("Content {i}"))
            .unwrap();

        send_email(servers.smtp_addr.port(), email, false).await;
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 3);
    assert_eq!(emails[0].subject.as_deref(), Some("Email 3"));
    assert_eq!(emails[1].subject.as_deref(), Some("Email 4"));
    assert_eq!(emails[2].subject.as_deref(), Some("Email 5"));

    servers.stop().await;
}

#[tokio::test]
async fn test_accepts_emails_from_whitelisted_senders() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        whitelist: vec!["allowed@example.com".to_string()],
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("allowed@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("From allowed sender")
        .body("Should be accepted".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);

    servers.stop().await;
}

#[tokio::test]
async fn test_accepts_emails_from_whitelisted_senders_case_insensitive() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        whitelist: vec!["ALSO.ALLOWED@EXAMPLE.COM".to_string()],
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("also.allowed@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Case insensitive test")
        .body("Should be accepted".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);

    servers.stop().await;
}

#[tokio::test]
async fn test_rejects_emails_from_non_whitelisted_senders() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        whitelist: vec!["allowed@example.com".to_string()],
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("notallowed@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("From blocked sender")
        .body("Should be rejected".to_string())
        .unwrap();

    let result = try_send_email(servers.smtp_addr.port(), email, false).await;
    assert!(result.is_err());

    let emails = get_emails(servers.http_addr.port()).await;
    assert!(emails.is_empty());

    servers.stop().await;
}

#[tokio::test]
async fn test_accepts_emails_over_tls_with_self_signed_cert() {
    // Install crypto provider before starting TLS server
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        tls: true,
        tls_self_signed: true,
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("secure@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Secure email")
        .body("Sent over TLS".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, true).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject.as_deref(), Some("Secure email"));

    servers.stop().await;
}

#[tokio::test]
async fn test_handles_email_with_no_subject() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .body("No subject line".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].subject.is_none());

    servers.stop().await;
}

#[tokio::test]
async fn test_handles_email_with_unicode_content() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Hello World")
        .body("Héllo Wörld! 🎉".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].text.as_ref().unwrap().contains("Héllo Wörld!"));
    assert!(emails[0].text.as_ref().unwrap().contains("🎉"));

    servers.stop().await;
}

#[tokio::test]
async fn test_handles_rapid_sequential_emails() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    for i in 0..5 {
        let email = Message::builder()
            .from("sender@example.com".parse().unwrap())
            .to("recipient@example.com".parse().unwrap())
            .subject(format!("Rapid {i}"))
            .body(format!("Content {i}"))
            .unwrap();

        send_email(servers.smtp_addr.port(), email, false).await;
    }

    sleep(Duration::from_millis(200)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 5);

    servers.stop().await;
}

#[tokio::test]
async fn test_generates_unique_ids_for_each_email() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let email1 = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("First")
        .body("First email".to_string())
        .unwrap();

    let email2 = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Second")
        .body("Second email".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email1, false).await;
    send_email(servers.smtp_addr.port(), email2, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 2);
    assert_ne!(emails[0].id, emails[1].id);

    servers.stop().await;
}

#[tokio::test]
async fn test_delete_single_email() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let email1 = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("First")
        .body("First email".to_string())
        .unwrap();

    let email2 = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Second")
        .body("Second email".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email1, false).await;
    send_email(servers.smtp_addr.port(), email2, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 2);

    let id_to_delete = &emails[0].id;
    let status = delete_email(servers.http_addr.port(), id_to_delete).await;
    assert_eq!(status, 204);

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject.as_deref(), Some("Second"));

    servers.stop().await;
}

#[tokio::test]
async fn test_delete_nonexistent_email_returns_404() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let status = delete_email(servers.http_addr.port(), "nonexistent-id").await;
    assert_eq!(status, 404);

    servers.stop().await;
}

#[tokio::test]
async fn test_websocket_receives_initial_emails() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let url = format!("ws://127.0.0.1:{}/ws", servers.http_addr.port());
    let (ws_stream, _) = connect_async(&url).await.unwrap();
    let (_, mut read) = ws_stream.split();

    let msg = read.next().await.unwrap().unwrap();
    if let WsMessage::Text(text) = msg {
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["event"], "emails");
        assert!(json["data"].is_array());
    } else {
        panic!("Expected text message");
    }

    servers.stop().await;
}

#[tokio::test]
async fn test_websocket_receives_updates_on_new_email() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let url = format!("ws://127.0.0.1:{}/ws", servers.http_addr.port());
    let (ws_stream, _) = connect_async(&url).await.unwrap();
    let (_, mut read) = ws_stream.split();

    let _ = read.next().await.unwrap().unwrap();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("WebSocket test")
        .body("Testing WebSocket".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;

    let msg = tokio::time::timeout(Duration::from_secs(2), read.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    if let WsMessage::Text(text) = msg {
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["event"], "emails");
        let emails = json["data"].as_array().unwrap();
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0]["subject"], "WebSocket test");
    } else {
        panic!("Expected text message");
    }

    servers.stop().await;
}

#[tokio::test]
async fn test_websocket_receives_updates_on_delete() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("To be deleted")
        .body("Testing WebSocket delete".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let url = format!("ws://127.0.0.1:{}/ws", servers.http_addr.port());
    let (ws_stream, _) = connect_async(&url).await.unwrap();
    let (_, mut read) = ws_stream.split();

    let msg = read.next().await.unwrap().unwrap();
    if let WsMessage::Text(text) = msg {
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["data"].as_array().unwrap().len(), 1);
    }

    delete_all_emails(servers.http_addr.port()).await;

    let msg = tokio::time::timeout(Duration::from_secs(2), read.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    if let WsMessage::Text(text) = msg {
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["event"], "emails");
        assert!(json["data"].as_array().unwrap().is_empty());
    } else {
        panic!("Expected text message");
    }

    servers.stop().await;
}
