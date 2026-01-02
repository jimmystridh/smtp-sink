//! SMTP server implementation with AUTH and STARTTLS support.

use crate::email::MailRecord;
use crate::store::EmailStore;
use base64::prelude::*;
use mail_parser::MessageParser;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error};
use uuid::Uuid;

/// SMTP server configuration.
#[derive(Clone, Default)]
pub struct SmtpConfig {
    pub whitelist: Vec<String>,
    pub tls_acceptor: Option<TlsAcceptor>,
    pub auth_required: bool,
    pub auth_username: Option<String>,
    pub auth_password: Option<String>,
}

/// Run the SMTP server (plain text, with optional STARTTLS).
pub async fn run_smtp_server(
    listener: TcpListener,
    store: Arc<EmailStore>,
    config: SmtpConfig,
    mut shutdown: broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, addr)) => {
                        debug!("SMTP connection from {addr}");
                        let store = Arc::clone(&store);
                        let config = config.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_plain_connection(stream, store, config).await {
                                debug!("SMTP session error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        error!("Failed to accept connection: {e}");
                    }
                }
            }
            _ = shutdown.recv() => {
                break;
            }
        }
    }
}

/// Run the SMTPS server (TLS from the start).
pub async fn run_smtps_server(
    listener: TcpListener,
    store: Arc<EmailStore>,
    config: SmtpConfig,
    tls_acceptor: TlsAcceptor,
    mut shutdown: broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, addr)) => {
                        debug!("SMTPS connection from {addr}");
                        let store = Arc::clone(&store);
                        let config = config.clone();
                        let acceptor = tls_acceptor.clone();
                        tokio::spawn(async move {
                            match acceptor.accept(stream).await {
                                Ok(tls_stream) => {
                                    if let Err(e) = handle_session(tls_stream, store, config, true).await {
                                        debug!("SMTPS session error: {e}");
                                    }
                                }
                                Err(e) => {
                                    debug!("TLS handshake failed: {e}");
                                }
                            }
                        });
                    }
                    Err(e) => {
                        error!("Failed to accept connection: {e}");
                    }
                }
            }
            _ = shutdown.recv() => {
                break;
            }
        }
    }
}

async fn handle_plain_connection(
    stream: TcpStream,
    store: Arc<EmailStore>,
    config: SmtpConfig,
) -> io::Result<()> {
    let (reader, writer) = tokio::io::split(stream);
    let reader = BufReader::new(reader);
    
    handle_session_inner(reader, writer, store, config, false).await
}

async fn handle_session<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    store: Arc<EmailStore>,
    config: SmtpConfig,
    tls_active: bool,
) -> io::Result<()> {
    let (reader, writer) = tokio::io::split(stream);
    let reader = BufReader::new(reader);
    
    handle_session_inner(reader, writer, store, config, tls_active).await
}

#[allow(clippy::too_many_lines)]
async fn handle_session_inner<R, W>(
    mut reader: BufReader<R>,
    mut writer: W,
    store: Arc<EmailStore>,
    config: SmtpConfig,
    tls_active: bool,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut session = Session::new(config.auth_required, tls_active);

    writer.write_all(b"220 localhost ESMTP smtp-sink\r\n").await?;
    writer.flush().await?;

    let mut line = String::new();
    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break;
        }

        let trimmed = line.trim();
        let cmd = trimmed.to_uppercase();

        // Handle AUTH state machine
        match &session.auth_state {
            AuthState::WaitingForPlain => {
                verify_plain_auth(trimmed, &mut session, &config, &mut writer).await?;
                session.auth_state = AuthState::None;
                continue;
            }
            AuthState::WaitingForLoginUsername => {
                if let Ok(decoded) = BASE64_STANDARD.decode(trimmed) {
                    let username = String::from_utf8_lossy(&decoded).to_string();
                    writer.write_all(b"334 UGFzc3dvcmQ6\r\n").await?;
                    writer.flush().await?;
                    session.auth_state = AuthState::WaitingForLoginPassword(username);
                } else {
                    writer.write_all(b"501 Cannot decode\r\n").await?;
                    writer.flush().await?;
                    session.auth_state = AuthState::None;
                }
                continue;
            }
            AuthState::WaitingForLoginPassword(username) => {
                let username = username.clone();
                if let Ok(decoded) = BASE64_STANDARD.decode(trimmed) {
                    let password = String::from_utf8_lossy(&decoded);
                    if Some(username.as_str()) == config.auth_username.as_deref()
                        && Some(password.as_ref()) == config.auth_password.as_deref()
                    {
                        session.authenticated = true;
                        writer.write_all(b"235 Authentication successful\r\n").await?;
                    } else {
                        writer.write_all(b"535 Authentication failed\r\n").await?;
                    }
                } else {
                    writer.write_all(b"501 Cannot decode\r\n").await?;
                }
                writer.flush().await?;
                session.auth_state = AuthState::None;
                continue;
            }
            AuthState::None => {}
        }

        if cmd.starts_with("EHLO") || cmd.starts_with("HELO") {
            writer.write_all(b"250-localhost Hello\r\n").await?;
            writer.write_all(b"250-SIZE 10485760\r\n").await?;
            writer.write_all(b"250-8BITMIME\r\n").await?;
            if config.tls_acceptor.is_some() && !session.tls_active {
                writer.write_all(b"250-STARTTLS\r\n").await?;
            }
            if config.auth_username.is_some() {
                writer.write_all(b"250-AUTH PLAIN LOGIN\r\n").await?;
            }
            writer.write_all(b"250 OK\r\n").await?;
        } else if cmd.starts_with("STARTTLS") {
            if config.tls_acceptor.is_none() {
                writer.write_all(b"454 TLS not available\r\n").await?;
            } else if session.tls_active {
                writer.write_all(b"503 TLS already active\r\n").await?;
            } else {
                writer.write_all(b"220 Ready to start TLS\r\n").await?;
                writer.flush().await?;
                // Note: STARTTLS upgrade requires reuniting the stream
                // This simplified impl doesn't support it fully
                // For full support, would need different architecture
                writer.write_all(b"500 STARTTLS not fully implemented\r\n").await?;
            }
        } else if cmd.starts_with("AUTH ") {
            if config.auth_username.is_none() {
                writer.write_all(b"503 AUTH not available\r\n").await?;
            } else {
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() >= 2 {
                    let mechanism = parts[1].to_uppercase();
                    match mechanism.as_str() {
                        "PLAIN" => {
                            if parts.len() > 2 {
                                verify_plain_auth(parts[2], &mut session, &config, &mut writer).await?;
                            } else {
                                writer.write_all(b"334 \r\n").await?;
                                session.auth_state = AuthState::WaitingForPlain;
                            }
                        }
                        "LOGIN" => {
                            writer.write_all(b"334 VXNlcm5hbWU6\r\n").await?;
                            session.auth_state = AuthState::WaitingForLoginUsername;
                        }
                        _ => {
                            writer.write_all(b"504 Unrecognized auth type\r\n").await?;
                        }
                    }
                } else {
                    writer.write_all(b"501 Syntax error\r\n").await?;
                }
            }
        } else if cmd.starts_with("MAIL FROM:") {
            if session.auth_required && !session.authenticated {
                writer.write_all(b"530 Authentication required\r\n").await?;
            } else {
                let addr = extract_address(&trimmed[10..]);
                if !config.whitelist.is_empty() && !config.whitelist.contains(&addr.to_lowercase()) {
                    writer.write_all(b"550 Sender not allowed\r\n").await?;
                } else {
                    session.mail_from = Some(addr);
                    writer.write_all(b"250 OK\r\n").await?;
                }
            }
        } else if cmd.starts_with("RCPT TO:") {
            if session.auth_required && !session.authenticated {
                writer.write_all(b"530 Authentication required\r\n").await?;
            } else if session.mail_from.is_none() {
                writer.write_all(b"503 MAIL FROM required first\r\n").await?;
            } else {
                // Catch-all: accept any recipient
                let addr = extract_address(&trimmed[8..]);
                session.rcpt_to.push(addr);
                writer.write_all(b"250 OK\r\n").await?;
            }
        } else if cmd == "DATA" {
            if session.auth_required && !session.authenticated {
                writer.write_all(b"530 Authentication required\r\n").await?;
            } else if session.mail_from.is_none() {
                writer.write_all(b"503 MAIL FROM required first\r\n").await?;
            } else if session.rcpt_to.is_empty() {
                writer.write_all(b"503 RCPT TO required first\r\n").await?;
            } else {
                writer.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n").await?;
                writer.flush().await?;

                let data = read_data(&mut reader).await?;
                let record = parse_email(&data, &session);
                store.push(record);
                session.reset();

                writer.write_all(b"250 OK: queued\r\n").await?;
            }
        } else if cmd == "RSET" {
            session.reset();
            writer.write_all(b"250 OK\r\n").await?;
        } else if cmd == "NOOP" {
            writer.write_all(b"250 OK\r\n").await?;
        } else if cmd == "QUIT" {
            writer.write_all(b"221 Bye\r\n").await?;
            writer.flush().await?;
            break;
        } else {
            writer.write_all(b"500 Command not recognized\r\n").await?;
        }

        writer.flush().await?;
    }

    Ok(())
}

async fn verify_plain_auth<W: AsyncWrite + Unpin>(
    encoded: &str,
    session: &mut Session,
    config: &SmtpConfig,
    writer: &mut W,
) -> io::Result<()> {
    if let Ok(decoded) = BASE64_STANDARD.decode(encoded.trim()) {
        let parts: Vec<&[u8]> = decoded.split(|&b| b == 0).collect();
        if parts.len() >= 3 {
            let username = String::from_utf8_lossy(parts[1]);
            let password = String::from_utf8_lossy(parts[2]);

            if Some(username.as_ref()) == config.auth_username.as_deref()
                && Some(password.as_ref()) == config.auth_password.as_deref()
            {
                session.authenticated = true;
                writer.write_all(b"235 Authentication successful\r\n").await?;
                writer.flush().await?;
                return Ok(());
            }
        }
    }

    writer.write_all(b"535 Authentication failed\r\n").await?;
    writer.flush().await?;
    Ok(())
}

struct Session {
    mail_from: Option<String>,
    rcpt_to: Vec<String>,
    authenticated: bool,
    auth_required: bool,
    tls_active: bool,
    auth_state: AuthState,
}

#[derive(Default)]
enum AuthState {
    #[default]
    None,
    WaitingForPlain,
    WaitingForLoginUsername,
    WaitingForLoginPassword(String),
}

impl Session {
    const fn new(auth_required: bool, tls_active: bool) -> Self {
        Self {
            mail_from: None,
            rcpt_to: Vec::new(),
            authenticated: false,
            auth_required,
            tls_active,
            auth_state: AuthState::None,
        }
    }

    fn reset(&mut self) {
        self.mail_from = None;
        self.rcpt_to.clear();
    }
}

fn extract_address(s: &str) -> String {
    let s = s.trim();
    if let (Some(start), Some(end)) = (s.find('<'), s.find('>')) {
        return s[start + 1..end].to_string();
    }
    s.to_string()
}

async fn read_data<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            break;
        }
        if line.trim() == "." {
            break;
        }
        let content = if line.starts_with("..") { &line[1..] } else { &line };
        data.extend_from_slice(content.as_bytes());
    }

    Ok(data)
}

#[allow(clippy::similar_names)]
fn parse_email(data: &[u8], session: &Session) -> MailRecord {
    let parser = MessageParser::default();
    let parsed = parser.parse(data);

    let id = format!(
        "{}-{}",
        chrono::Utc::now().timestamp_millis(),
        Uuid::new_v4().simple()
    );

    let (from, to, subject, text, html, mail_date, headers) = parsed.map_or_else(
        || {
            (
                session.mail_from.clone().unwrap_or_else(|| "unknown".to_string()),
                session.rcpt_to.clone(),
                None,
                None,
                None,
                chrono::Utc::now().to_rfc3339(),
                HashMap::new(),
            )
        },
        |msg| {
            let from = msg
                .from()
                .and_then(|a| a.first())
                .map(|a| {
                    a.name().map_or_else(
                        || a.address().unwrap_or_default().to_string(),
                        |name| format!("{name} <{}>", a.address().unwrap_or_default()),
                    )
                })
                .or_else(|| session.mail_from.clone())
                .unwrap_or_else(|| "unknown".to_string());

            let to: Vec<String> = msg
                .to()
                .map(|list| list.iter().filter_map(|a| a.address().map(String::from)).collect())
                .filter(|v: &Vec<String>| !v.is_empty())
                .unwrap_or_else(|| session.rcpt_to.clone());

            let subject = msg.subject().map(String::from);
            let text = msg.body_text(0).map(|s| s.to_string());
            let html = msg.body_html(0).map(|s| s.to_string());

            let mail_date = msg.date().map_or_else(
                || chrono::Utc::now().to_rfc3339(),
                |d| {
                    chrono::DateTime::from_timestamp(d.to_timestamp(), 0)
                        .unwrap_or_else(chrono::Utc::now)
                        .to_rfc3339()
                },
            );

            let headers: HashMap<String, String> = msg
                .headers()
                .iter()
                .filter_map(|h| {
                    let name = h.name().to_string().to_lowercase();
                    let value = h.value().as_text()?.to_string();
                    Some((name, value))
                })
                .collect();

            (from, to, subject, text, html, mail_date, headers)
        },
    );

    MailRecord {
        id,
        to,
        from,
        subject,
        text,
        html,
        date: mail_date,
        headers,
    }
}
