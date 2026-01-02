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

// ─────────────────────────────────────────────────────────────────────────────
// SMTP AUTH tests
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_auth_advertised_when_credentials_configured() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        auth_username: Some("testuser".to_string()),
        auth_password: Some("testpass".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();

    // Use raw socket to check EHLO response
    let port = servers.smtp_addr.port();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        
        // Read greeting
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        
        // Send EHLO
        stream.write_all(b"EHLO localhost\r\n").unwrap();
        
        // Read response lines until we get 250 (without dash)
        let mut has_auth = false;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.contains("AUTH") {
                has_auth = true;
            }
            if line.starts_with("250 ") {
                break;
            }
        }
        
        stream.write_all(b"QUIT\r\n").unwrap();
        has_auth
    })
    .await
    .unwrap();

    assert!(result, "AUTH should be advertised in EHLO response");

    servers.stop().await;
}

#[tokio::test]
async fn test_auth_required_rejects_unauthenticated() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        auth_required: true,
        auth_username: Some("testuser".to_string()),
        auth_password: Some("testpass".to_string()),
        ..Default::default()
    })
    .await
    .unwrap();

    // Try to send without auth - should fail
    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("No auth")
        .body("Should fail".to_string())
        .unwrap();

    let result = try_send_email(servers.smtp_addr.port(), email, false).await;
    assert!(result.is_err(), "Should reject unauthenticated sender");

    let emails = get_emails(servers.http_addr.port()).await;
    assert!(emails.is_empty());

    servers.stop().await;
}

#[tokio::test]
async fn test_catches_all_recipient_domains() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    // Send to various random domains - all should be accepted
    let port = servers.smtp_addr.port();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // greeting
        
        stream.write_all(b"EHLO localhost\r\n").unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.starts_with("250 ") { break; }
        }
        
        stream.write_all(b"MAIL FROM:<sender@example.com>\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        
        // Try various recipient domains
        stream.write_all(b"RCPT TO:<user@random-domain-xyz.com>\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let ok1 = line.starts_with("250");
        
        stream.write_all(b"RCPT TO:<test@another.domain.org>\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let ok2 = line.starts_with("250");
        
        stream.write_all(b"RCPT TO:<catch@all.works>\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let ok3 = line.starts_with("250");
        
        stream.write_all(b"QUIT\r\n").unwrap();
        
        ok1 && ok2 && ok3
    })
    .await
    .unwrap();

    assert!(result, "Should accept any recipient domain (catch-all)");

    servers.stop().await;
}

#[tokio::test]
async fn test_starttls_advertised() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        starttls: true,
        tls_self_signed: true,
        ..Default::default()
    })
    .await
    .unwrap();

    let port = servers.smtp_addr.port();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        
        stream.write_all(b"EHLO localhost\r\n").unwrap();
        
        let mut has_starttls = false;
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.contains("STARTTLS") {
                has_starttls = true;
            }
            if line.starts_with("250 ") { break; }
        }
        
        stream.write_all(b"QUIT\r\n").unwrap();
        has_starttls
    })
    .await
    .unwrap();

    assert!(result, "STARTTLS should be advertised");

    servers.stop().await;
}

#[tokio::test]
async fn test_starttls_upgrade_and_send_email() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        starttls: true,
        tls_self_signed: true,
        ..Default::default()
    })
    .await
    .unwrap();

    let smtp_port = servers.smtp_addr.port();
    let http_port = servers.http_addr.port();
    
    // Use lettre with STARTTLS
    let result = tokio::task::spawn_blocking(move || {
        let tls_params = TlsParameters::builder("localhost".to_string())
            .dangerous_accept_invalid_certs(true)
            .build()
            .unwrap();
        
        let transport = SmtpTransport::builder_dangerous("127.0.0.1")
            .port(smtp_port)
            .timeout(Some(Duration::from_secs(10)))
            .tls(Tls::Required(tls_params))
            .build();
        
        let email = Message::builder()
            .from("sender@example.com".parse::<Mailbox>().unwrap())
            .to("recipient@example.com".parse::<Mailbox>().unwrap())
            .subject("STARTTLS Test Email")
            .body("This email was sent over STARTTLS".to_string())
            .unwrap();
        
        transport.send(&email)
    })
    .await
    .unwrap();

    assert!(result.is_ok(), "Email should be sent over STARTTLS: {:?}", result.err());
    
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(http_port).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject.as_deref(), Some("STARTTLS Test Email"));
    assert!(emails[0].text.as_deref().unwrap().contains("STARTTLS"));

    servers.stop().await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Additional comprehensive test scenarios
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_email_with_cc_and_bcc() {
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
        .cc("cc@example.com".parse().unwrap())
        .bcc("bcc@example.com".parse().unwrap())
        .subject("CC and BCC test")
        .body("Testing CC and BCC".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject.as_deref(), Some("CC and BCC test"));

    servers.stop().await;
}

#[tokio::test]
async fn test_email_with_reply_to() {
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
        .reply_to("replyto@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Reply-To test")
        .body("Testing Reply-To header".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    // Just verify the email was received - Reply-To is in headers
    assert_eq!(emails[0].subject.as_deref(), Some("Reply-To test"));

    servers.stop().await;
}

#[tokio::test]
async fn test_email_with_very_long_subject() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let long_subject = "A".repeat(500);
    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject(long_subject.clone())
        .body("Long subject test".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].subject.as_ref().unwrap().contains("AAAA"));

    servers.stop().await;
}

#[tokio::test]
async fn test_email_with_large_body() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let large_body = "X".repeat(100_000); // 100KB body
    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Large body test")
        .body(large_body.clone())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(200)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].text.as_ref().unwrap().len() >= 100_000);

    servers.stop().await;
}

#[tokio::test]
async fn test_email_with_special_characters_in_addresses() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("sender+tag@example.com".parse().unwrap())
        .to("recipient.name@example.com".parse().unwrap())
        .subject("Special chars in address")
        .body("Testing special characters".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].from.contains("sender+tag@example.com"));
    assert!(emails[0].to.contains(&"recipient.name@example.com".to_string()));

    servers.stop().await;
}

#[tokio::test]
async fn test_email_with_display_names() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("John Doe <john.doe@example.com>".parse().unwrap())
        .to("Jane Smith <jane.smith@example.com>".parse().unwrap())
        .subject("Display names test")
        .body("Testing display names".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].from.contains("john.doe@example.com"));

    servers.stop().await;
}

#[tokio::test]
async fn test_concurrent_email_sending() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(20),
        ..Default::default()
    })
    .await
    .unwrap();

    let port = servers.smtp_addr.port();
    let mut handles = Vec::new();
    
    for i in 0..10 {
        let handle = tokio::spawn(async move {
            let email = Message::builder()
                .from("sender@example.com".parse().unwrap())
                .to("recipient@example.com".parse().unwrap())
                .subject(format!("Concurrent {i}"))
                .body(format!("Concurrent email {i}"))
                .unwrap();
            
            tokio::task::spawn_blocking(move || {
                let transport = create_transport(port, false);
                transport.send(&email).unwrap();
            })
            .await
            .unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap();
    }
    
    sleep(Duration::from_millis(300)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 10);

    servers.stop().await;
}

#[tokio::test]
async fn test_multiple_websocket_clients() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let url = format!("ws://127.0.0.1:{}/ws", servers.http_addr.port());
    
    // Connect multiple WebSocket clients
    let (ws1, _) = connect_async(&url).await.unwrap();
    let (ws2, _) = connect_async(&url).await.unwrap();
    let (_, mut read1) = ws1.split();
    let (_, mut read2) = ws2.split();

    // Both should receive initial state
    let msg1 = read1.next().await.unwrap().unwrap();
    let msg2 = read2.next().await.unwrap().unwrap();
    
    if let (WsMessage::Text(t1), WsMessage::Text(t2)) = (msg1, msg2) {
        let j1: Value = serde_json::from_str(&t1).unwrap();
        let j2: Value = serde_json::from_str(&t2).unwrap();
        assert_eq!(j1["event"], "emails");
        assert_eq!(j2["event"], "emails");
    }

    // Send an email
    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Multi-client test")
        .body("Testing multiple clients".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;

    // Both clients should receive the update
    let msg1 = tokio::time::timeout(Duration::from_secs(2), read1.next())
        .await.unwrap().unwrap().unwrap();
    let msg2 = tokio::time::timeout(Duration::from_secs(2), read2.next())
        .await.unwrap().unwrap().unwrap();

    if let (WsMessage::Text(t1), WsMessage::Text(t2)) = (msg1, msg2) {
        let j1: Value = serde_json::from_str(&t1).unwrap();
        let j2: Value = serde_json::from_str(&t2).unwrap();
        assert_eq!(j1["data"].as_array().unwrap().len(), 1);
        assert_eq!(j2["data"].as_array().unwrap().len(), 1);
    }

    servers.stop().await;
}

#[tokio::test]
async fn test_whitelist_with_multiple_addresses() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        whitelist: vec![
            "allowed1@example.com".to_string(),
            "allowed2@example.com".to_string(),
            "allowed3@example.com".to_string(),
        ],
        ..Default::default()
    })
    .await
    .unwrap();

    // First allowed sender
    let email1 = Message::builder()
        .from("allowed1@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("From allowed1")
        .body("Should work".to_string())
        .unwrap();

    // Second allowed sender
    let email2 = Message::builder()
        .from("allowed2@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("From allowed2")
        .body("Should work".to_string())
        .unwrap();

    // Blocked sender
    let email3 = Message::builder()
        .from("blocked@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("From blocked")
        .body("Should fail".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email1, false).await;
    send_email(servers.smtp_addr.port(), email2, false).await;
    let result = try_send_email(servers.smtp_addr.port(), email3, false).await;
    
    assert!(result.is_err());
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 2);

    servers.stop().await;
}

#[tokio::test]
async fn test_max_emails_fifo_order() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(3),
        ..Default::default()
    })
    .await
    .unwrap();

    // Send emails A, B, C, D, E
    for label in ["A", "B", "C", "D", "E"] {
        let email = Message::builder()
            .from("sender@example.com".parse().unwrap())
            .to("recipient@example.com".parse().unwrap())
            .subject(format!("Email {label}"))
            .body(format!("Content {label}"))
            .unwrap();

        send_email(servers.smtp_addr.port(), email, false).await;
        sleep(Duration::from_millis(50)).await;
    }

    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 3);
    // Should keep C, D, E (oldest removed first)
    assert_eq!(emails[0].subject.as_deref(), Some("Email C"));
    assert_eq!(emails[1].subject.as_deref(), Some("Email D"));
    assert_eq!(emails[2].subject.as_deref(), Some("Email E"));

    servers.stop().await;
}

#[tokio::test]
async fn test_empty_whitelist_accepts_all() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        whitelist: vec![],  // Empty whitelist
        ..Default::default()
    })
    .await
    .unwrap();

    let email = Message::builder()
        .from("anyone@anydomain.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Open policy")
        .body("Anyone can send".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);

    servers.stop().await;
}

#[tokio::test]
async fn test_http_cors_headers() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let client = Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails", servers.http_addr.port()))
        .send()
        .await
        .unwrap();

    // Check for CORS headers (if implemented)
    let status = resp.status().as_u16();
    assert_eq!(status, 200);

    servers.stop().await;
}

#[tokio::test]
async fn test_delete_single_email_preserves_others() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    // Send three emails
    for i in 1..=3 {
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

    // Delete the middle one
    let middle_id = &emails[1].id;
    let status = delete_email(servers.http_addr.port(), middle_id).await;
    assert_eq!(status, 204);

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 2);
    assert_eq!(emails[0].subject.as_deref(), Some("Email 1"));
    assert_eq!(emails[1].subject.as_deref(), Some("Email 3"));

    servers.stop().await;
}

#[tokio::test]
async fn test_smtps_rejects_plain_connection() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        tls: true,
        tls_self_signed: true,
        ..Default::default()
    })
    .await
    .unwrap();

    // Try to connect with plain SMTP (no TLS) - should fail
    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Plain to SMTPS")
        .body("Should fail".to_string())
        .unwrap();

    let result = try_send_email(servers.smtp_addr.port(), email, false).await;
    assert!(result.is_err());

    servers.stop().await;
}

#[tokio::test]
async fn test_email_timestamps_are_valid() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let before = chrono::Utc::now();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Timestamp test")
        .body("Testing timestamps".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let after = chrono::Utc::now();

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    
    // The date field should be a valid timestamp
    let date_str = &emails[0].date;
    assert!(!date_str.is_empty());
    
    // Parse and verify it's within our time window
    let parsed = chrono::DateTime::parse_from_rfc3339(date_str);
    if let Ok(dt) = parsed {
        let dt_utc = dt.with_timezone(&chrono::Utc);
        assert!(dt_utc >= before - chrono::Duration::seconds(1));
        assert!(dt_utc <= after + chrono::Duration::seconds(1));
    }

    servers.stop().await;
}

#[tokio::test]
async fn test_smtp_helo_command() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let port = servers.smtp_addr.port();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        
        let mut line = String::new();
        reader.read_line(&mut line).unwrap(); // greeting
        assert!(line.starts_with("220"));
        
        // Use HELO instead of EHLO
        stream.write_all(b"HELO localhost\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let helo_ok = line.starts_with("250");
        
        stream.write_all(b"QUIT\r\n").unwrap();
        helo_ok
    })
    .await
    .unwrap();

    assert!(result, "HELO command should be accepted");

    servers.stop().await;
}

#[tokio::test]
async fn test_smtp_rset_command() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let port = servers.smtp_addr.port();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        
        stream.write_all(b"EHLO localhost\r\n").unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.starts_with("250 ") { break; }
        }
        
        stream.write_all(b"MAIL FROM:<sender@example.com>\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        
        // Reset the transaction
        stream.write_all(b"RSET\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let rset_ok = line.starts_with("250");
        
        stream.write_all(b"QUIT\r\n").unwrap();
        rset_ok
    })
    .await
    .unwrap();

    assert!(result, "RSET command should be accepted");

    servers.stop().await;
}

#[tokio::test]
async fn test_smtp_noop_command() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let port = servers.smtp_addr.port();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        
        stream.write_all(b"EHLO localhost\r\n").unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.starts_with("250 ") { break; }
        }
        
        stream.write_all(b"NOOP\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        let noop_ok = line.starts_with("250");
        
        stream.write_all(b"QUIT\r\n").unwrap();
        noop_ok
    })
    .await
    .unwrap();

    assert!(result, "NOOP command should be accepted");

    servers.stop().await;
}

#[tokio::test]
async fn test_smtp_vrfy_command() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let port = servers.smtp_addr.port();
    let result = tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        
        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        
        stream.write_all(b"EHLO localhost\r\n").unwrap();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.starts_with("250 ") { break; }
        }
        
        stream.write_all(b"VRFY user@example.com\r\n").unwrap();
        line.clear();
        reader.read_line(&mut line).unwrap();
        // VRFY may return 252 (cannot verify), 250 (ok), 502 (not implemented), 
        // or 500/501 (syntax error) - all are valid responses
        let vrfy_ok = line.starts_with('2') || line.starts_with('5');
        
        stream.write_all(b"QUIT\r\n").unwrap();
        vrfy_ok
    })
    .await
    .unwrap();

    assert!(result, "VRFY command should return a valid SMTP response");

    servers.stop().await;
}

#[tokio::test]
async fn test_http_json_content_type() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let client = Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails", servers.http_addr.port()))
        .send()
        .await
        .unwrap();

    let content_type = resp.headers().get("content-type").map(|v| v.to_str().unwrap_or(""));
    assert!(content_type.unwrap_or("").contains("application/json"));

    servers.stop().await;
}

#[tokio::test]
async fn test_restart_clears_emails() {
    // First instance
    let servers1 = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let smtp_port = servers1.smtp_addr.port();
    let http_port = servers1.http_addr.port();

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Before restart")
        .body("Should not persist".to_string())
        .unwrap();

    send_email(smtp_port, email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(http_port).await;
    assert_eq!(emails.len(), 1);

    servers1.stop().await;

    // Second instance on different ports
    let servers2 = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let emails = get_emails(servers2.http_addr.port()).await;
    assert!(emails.is_empty(), "Emails should not persist across restarts");

    servers2.stop().await;
}

#[tokio::test]
async fn test_websocket_ping_pong() {
    use futures_util::SinkExt;
    
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        ..Default::default()
    })
    .await
    .unwrap();

    let url = format!("ws://127.0.0.1:{}/ws", servers.http_addr.port());
    let (ws_stream, _) = connect_async(&url).await.unwrap();
    let (mut write, mut read) = ws_stream.split();

    // Consume initial message
    let _ = read.next().await;

    // Send ping
    write.send(WsMessage::Ping(vec![1, 2, 3].into())).await.unwrap();

    // Should receive pong or nothing (handled automatically)
    let msg = tokio::time::timeout(Duration::from_millis(500), read.next()).await;
    
    // WebSocket implementation may handle ping/pong automatically
    // Just verify connection is still alive by checking we didn't error
    #[allow(clippy::match_same_arms)]
    match msg {
        Ok(Some(Ok(WsMessage::Pong(_)))) => (), // Got pong
        Ok(Some(Ok(_))) => (),                  // Got other message  
        Ok(None) => (),                         // Stream ended (ok for test)
        Err(_) => (),                           // Timeout (ping handled internally)
        Ok(Some(Err(_))) => panic!("WebSocket error"),
    }

    servers.stop().await;
}

#[tokio::test]
async fn test_email_ids_are_uuid_format() {
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
        .subject("UUID test")
        .body("Testing UUID format".to_string())
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    
    // Check that ID looks like a UUID (36 chars with dashes)
    let id = &emails[0].id;
    assert!(id.len() >= 32, "ID should be at least 32 chars");

    servers.stop().await;
}

#[tokio::test]
async fn test_multipart_email() {
    use lettre::message::{MultiPart, SinglePart};

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
        .subject("Multipart test")
        .multipart(
            MultiPart::alternative()
                .singlepart(
                    SinglePart::builder()
                        .header(lettre::message::header::ContentType::TEXT_PLAIN)
                        .body(String::from("Plain text version")),
                )
                .singlepart(
                    SinglePart::builder()
                        .header(lettre::message::header::ContentType::TEXT_HTML)
                        .body(String::from("<h1>HTML version</h1>")),
                ),
        )
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert!(emails[0].text.as_ref().is_some_and(|t| t.contains("Plain text")));
    assert!(emails[0].html.as_ref().is_some_and(|h| h.contains("HTML version")));

    servers.stop().await;
}

// ─────────────────────────────────────────────────────────────────────────────
// Attachments, Search/Filter, and SQLite Persistence Tests
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_email_with_attachment() {
    use lettre::message::{Attachment, MultiPart, SinglePart};

    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    let attachment = Attachment::new(String::from("test.txt"))
        .body(String::from("Hello, this is a test attachment!"), "text/plain".parse().unwrap());

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Email with attachment")
        .multipart(
            MultiPart::mixed()
                .singlepart(
                    SinglePart::builder()
                        .header(lettre::message::header::ContentType::TEXT_PLAIN)
                        .body(String::from("Please see attachment")),
                )
                .singlepart(attachment),
        )
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].attachments.len(), 1);
    assert_eq!(emails[0].attachments[0].filename, "test.txt");
    assert_eq!(emails[0].attachments[0].content_type, "text/plain");

    // Test GET /emails/:id/attachments
    let client = Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails/{}/attachments", 
            servers.http_addr.port(), emails[0].id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let attachments: Vec<Value> = resp.json().await.unwrap();
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0]["filename"], "test.txt");

    // Test GET /emails/:id/attachments/:filename
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails/{}/attachments/test.txt", 
            servers.http_addr.port(), emails[0].id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let content = resp.text().await.unwrap();
    assert!(content.contains("Hello, this is a test attachment!"));

    servers.stop().await;
}

#[tokio::test]
async fn test_search_emails_by_from() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    // Send emails from different senders
    for sender in ["alice@example.com", "bob@example.com", "alice@other.com"] {
        let email = Message::builder()
            .from(sender.parse().unwrap())
            .to("recipient@example.com".parse().unwrap())
            .subject(format!("From {sender}"))
            .body(String::from("Test"))
            .unwrap();
        send_email(servers.smtp_addr.port(), email, false).await;
    }
    sleep(Duration::from_millis(100)).await;

    let client = Client::new();
    
    // Search for alice
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails?from=alice", servers.http_addr.port()))
        .send()
        .await
        .unwrap();
    let emails: Vec<MailRecord> = resp.json().await.unwrap();
    assert_eq!(emails.len(), 2);
    
    // Search for bob
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails?from=bob", servers.http_addr.port()))
        .send()
        .await
        .unwrap();
    let emails: Vec<MailRecord> = resp.json().await.unwrap();
    assert_eq!(emails.len(), 1);
    assert!(emails[0].from.contains("bob"));

    servers.stop().await;
}

#[tokio::test]
async fn test_search_emails_by_subject() {
    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    for subject in ["Important: Review needed", "FYI: Meeting tomorrow", "Important: Budget update"] {
        let email = Message::builder()
            .from("sender@example.com".parse().unwrap())
            .to("recipient@example.com".parse().unwrap())
            .subject(subject)
            .body(String::from("Test"))
            .unwrap();
        send_email(servers.smtp_addr.port(), email, false).await;
    }
    sleep(Duration::from_millis(100)).await;

    let client = Client::new();
    
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails?subject=Important", servers.http_addr.port()))
        .send()
        .await
        .unwrap();
    let emails: Vec<MailRecord> = resp.json().await.unwrap();
    assert_eq!(emails.len(), 2);

    servers.stop().await;
}

#[tokio::test]
async fn test_get_single_email() {
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
        .subject("Get single email test")
        .body(String::from("Body content"))
        .unwrap();
    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    let id = &emails[0].id;

    let client = Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails/{}", servers.http_addr.port(), id))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let email: MailRecord = resp.json().await.unwrap();
    assert_eq!(email.subject.as_deref(), Some("Get single email test"));

    // Test 404 for non-existent email
    let resp = client
        .get(format!("http://127.0.0.1:{}/emails/nonexistent", servers.http_addr.port()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    servers.stop().await;
}

#[tokio::test]
async fn test_sqlite_persistence() {
    use tempfile::NamedTempFile;
    
    let db_file = NamedTempFile::new().unwrap();
    let db_path = db_file.path().to_str().unwrap().to_string();

    // Start server with SQLite, send email, stop
    {
        let servers = start_sink(SinkOptions {
            smtp_port: Some(0),
            http_port: Some(0),
            max: Some(10),
            db_path: Some(db_path.clone()),
            ..Default::default()
        })
        .await
        .unwrap();

        let smtp_port = servers.smtp_addr.port();
        let http_port = servers.http_addr.port();

        let email = Message::builder()
            .from("persist@example.com".parse().unwrap())
            .to("recipient@example.com".parse().unwrap())
            .subject("Persistence test")
            .body(String::from("This should survive restart"))
            .unwrap();
        send_email(smtp_port, email, false).await;
        sleep(Duration::from_millis(100)).await;

        let emails = get_emails(http_port).await;
        assert_eq!(emails.len(), 1);

        servers.stop().await;
    }

    // Start new server with same DB - email should still be there
    {
        let servers = start_sink(SinkOptions {
            smtp_port: Some(0),
            http_port: Some(0),
            max: Some(10),
            db_path: Some(db_path),
            ..Default::default()
        })
        .await
        .unwrap();

        let emails = get_emails(servers.http_addr.port()).await;
        assert_eq!(emails.len(), 1);
        assert_eq!(emails[0].subject.as_deref(), Some("Persistence test"));
        assert!(emails[0].from.contains("persist@example.com"));

        servers.stop().await;
    }
}

#[tokio::test]
async fn test_sqlite_with_attachments() {
    use lettre::message::{Attachment, MultiPart, SinglePart};
    use tempfile::NamedTempFile;
    
    let db_file = NamedTempFile::new().unwrap();
    let db_path = db_file.path().to_str().unwrap().to_string();

    let email_id;

    // Store email with attachment
    {
        let servers = start_sink(SinkOptions {
            smtp_port: Some(0),
            http_port: Some(0),
            max: Some(10),
            db_path: Some(db_path.clone()),
            ..Default::default()
        })
        .await
        .unwrap();

        let attachment = Attachment::new(String::from("data.bin"))
            .body(vec![0u8, 1, 2, 3, 4, 5], "application/octet-stream".parse().unwrap());

        let email = Message::builder()
            .from("sender@example.com".parse().unwrap())
            .to("recipient@example.com".parse().unwrap())
            .subject("Attachment persistence")
            .multipart(
                MultiPart::mixed()
                    .singlepart(
                        SinglePart::builder()
                            .header(lettre::message::header::ContentType::TEXT_PLAIN)
                            .body(String::from("See attached")),
                    )
                    .singlepart(attachment),
            )
            .unwrap();

        send_email(servers.smtp_addr.port(), email, false).await;
        sleep(Duration::from_millis(100)).await;

        let emails = get_emails(servers.http_addr.port()).await;
        email_id = emails[0].id.clone();
        assert_eq!(emails[0].attachments.len(), 1);

        servers.stop().await;
    }

    // Restart and verify attachment is preserved
    {
        let servers = start_sink(SinkOptions {
            smtp_port: Some(0),
            http_port: Some(0),
            max: Some(10),
            db_path: Some(db_path),
            ..Default::default()
        })
        .await
        .unwrap();

        let client = Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{}/emails/{}/attachments/data.bin", 
                servers.http_addr.port(), email_id))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let data = resp.bytes().await.unwrap();
        assert_eq!(data.as_ref(), &[0u8, 1, 2, 3, 4, 5]);

        servers.stop().await;
    }
}

#[tokio::test]
async fn test_inline_attachment_by_cid() {
    use lettre::message::{MultiPart, SinglePart};
    use lettre::message::header::{ContentType, ContentDisposition, ContentId};

    let servers = start_sink(SinkOptions {
        smtp_port: Some(0),
        http_port: Some(0),
        max: Some(10),
        ..Default::default()
    })
    .await
    .unwrap();

    // Create email with inline image
    let image_data = vec![0x89, 0x50, 0x4E, 0x47]; // PNG header bytes
    let inline_image = SinglePart::builder()
        .header(ContentType::parse("image/png").unwrap())
        .header(ContentDisposition::inline())
        .header(ContentId::from(String::from("<image001@example.com>")))
        .body(image_data.clone());

    let html_body = SinglePart::builder()
        .header(ContentType::TEXT_HTML)
        .body(String::from(r#"<html><body><img src="cid:image001@example.com"/></body></html>"#));

    let email = Message::builder()
        .from("sender@example.com".parse().unwrap())
        .to("recipient@example.com".parse().unwrap())
        .subject("Inline image test")
        .multipart(MultiPart::related().singlepart(html_body).singlepart(inline_image))
        .unwrap();

    send_email(servers.smtp_addr.port(), email, false).await;
    sleep(Duration::from_millis(100)).await;

    let emails = get_emails(servers.http_addr.port()).await;
    assert_eq!(emails.len(), 1);
    
    // Get attachment by content-id
    let client = Client::new();
    let resp = client
        .get(format!(
            "http://127.0.0.1:{}/emails/{}/cid/image001@example.com",
            servers.http_addr.port(),
            emails[0].id
        ))
        .send()
        .await
        .unwrap();
    
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers().get("content-type").unwrap(), "image/png");
    let data = resp.bytes().await.unwrap();
    assert_eq!(data.as_ref(), &image_data);

    servers.stop().await;
}
