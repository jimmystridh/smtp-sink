//! SMTP server implementation with AUTH and STARTTLS support.

use crate::email::MailRecord;
use crate::forward::ForwardHandle;
use crate::store::EmailStorage;
use base64::prelude::*;
use mail_parser::{MessageParser, MimeHeaders};
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info};
use uuid::Uuid;

/// SMTP server configuration.
#[derive(Clone, Default)]
pub struct SmtpConfig {
    pub whitelist: Vec<String>,
    pub tls_acceptor: Option<TlsAcceptor>,
    pub auth_required: bool,
    pub auth_username: Option<String>,
    pub auth_password: Option<String>,
    pub forward_handle: Option<ForwardHandle>,
}

/// Run the SMTP server (plain text, with optional STARTTLS).
pub async fn run_smtp_server(
    listener: TcpListener,
    store: Arc<dyn EmailStorage>,
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
                            if let Err(e) = handle_connection(stream, store, config).await {
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
    store: Arc<dyn EmailStorage>,
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
                                    if let Err(e) = handle_tls_connection(tls_stream, store, config).await {
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

/// Result of processing a single SMTP command.
enum CommandResult {
    Continue,
    Quit,
    StartTls,
}

/// Handle a plain TCP connection with optional STARTTLS upgrade.
async fn handle_connection(
    stream: TcpStream,
    store: Arc<dyn EmailStorage>,
    config: SmtpConfig,
) -> io::Result<()> {
    let mut session = Session::new(config.auth_required, false);
    
    // Use buffered I/O over the raw stream
    let mut stream = BufStream::new(stream);
    
    stream.write_all(b"220 localhost ESMTP smtp-sink\r\n").await?;
    stream.flush().await?;

    loop {
        match process_command(&mut stream, &mut session, &config, &store).await? {
            CommandResult::Continue => {}
            CommandResult::Quit => break,
            CommandResult::StartTls => {
                // Upgrade to TLS
                if let Some(ref acceptor) = config.tls_acceptor {
                    info!("Upgrading connection to TLS");
                    let inner = stream.into_inner();
                    match acceptor.clone().accept(inner).await {
                        Ok(tls_stream) => {
                            // Continue session on TLS stream
                            session.tls_active = true;
                            session.reset();
                            return handle_tls_session(tls_stream, session, config, store).await;
                        }
                        Err(e) => {
                            debug!("STARTTLS handshake failed: {e}");
                            return Err(io::Error::other(format!("TLS handshake failed: {e}")));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Handle an already-TLS connection (SMTPS).
async fn handle_tls_connection<S>(
    stream: S,
    store: Arc<dyn EmailStorage>,
    config: SmtpConfig,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let session = Session::new(config.auth_required, true);
    handle_tls_session(stream, session, config, store).await
}

/// Continue an SMTP session over a TLS stream.
async fn handle_tls_session<S>(
    stream: S,
    mut session: Session,
    config: SmtpConfig,
    store: Arc<dyn EmailStorage>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = BufStream::new(stream);
    
    // Send greeting if this is a fresh SMTPS connection
    if session.mail_from.is_none() && session.rcpt_to.is_empty() {
        stream.write_all(b"220 localhost ESMTP smtp-sink\r\n").await?;
        stream.flush().await?;
    }

    loop {
        match process_command(&mut stream, &mut session, &config, &store).await? {
            CommandResult::Continue => {}
            CommandResult::Quit => break,
            CommandResult::StartTls => {
                // Already on TLS, this shouldn't happen but handle gracefully
                stream.write_all(b"503 TLS already active\r\n").await?;
                stream.flush().await?;
            }
        }
    }

    Ok(())
}

/// A simple buffered stream wrapper that supports both reading and writing.
struct BufStream<S> {
    inner: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> BufStream<S> {
    fn new(stream: S) -> Self {
        Self {
            inner: BufReader::new(stream),
        }
    }

    fn into_inner(self) -> S {
        self.inner.into_inner()
    }
}

impl<S: AsyncRead + Unpin> AsyncBufRead for BufStream<S> {
    fn poll_fill_buf(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<&[u8]>> {
        Pin::new(&mut self.get_mut().inner).poll_fill_buf(cx)
    }

    fn consume(self: Pin<&mut Self>, amt: usize) {
        Pin::new(&mut self.get_mut().inner).consume(amt);
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for BufStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for BufStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        Pin::new(self.get_mut().inner.get_mut()).poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        Pin::new(self.get_mut().inner.get_mut()).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        Pin::new(self.get_mut().inner.get_mut()).poll_shutdown(cx)
    }
}

/// Process a single SMTP command.
#[allow(clippy::too_many_lines)]
async fn process_command<S>(
    stream: &mut BufStream<S>,
    session: &mut Session,
    config: &SmtpConfig,
    store: &Arc<dyn EmailStorage>,
) -> io::Result<CommandResult>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut line = String::new();
    let bytes_read = stream.inner.read_line(&mut line).await?;
    if bytes_read == 0 {
        return Ok(CommandResult::Quit);
    }

    let trimmed = line.trim();
    let cmd = trimmed.to_uppercase();

    // Handle AUTH state machine
    match &session.auth_state {
        AuthState::WaitingForPlain => {
            verify_plain_auth(trimmed, session, config, stream).await?;
            session.auth_state = AuthState::None;
            return Ok(CommandResult::Continue);
        }
        AuthState::WaitingForLoginUsername => {
            if let Ok(decoded) = BASE64_STANDARD.decode(trimmed) {
                let username = String::from_utf8_lossy(&decoded).to_string();
                stream.write_all(b"334 UGFzc3dvcmQ6\r\n").await?;
                stream.flush().await?;
                session.auth_state = AuthState::WaitingForLoginPassword(username);
            } else {
                stream.write_all(b"501 Cannot decode\r\n").await?;
                stream.flush().await?;
                session.auth_state = AuthState::None;
            }
            return Ok(CommandResult::Continue);
        }
        AuthState::WaitingForLoginPassword(username) => {
            let username = username.clone();
            if let Ok(decoded) = BASE64_STANDARD.decode(trimmed) {
                let password = String::from_utf8_lossy(&decoded);
                if Some(username.as_str()) == config.auth_username.as_deref()
                    && Some(password.as_ref()) == config.auth_password.as_deref()
                {
                    session.authenticated = true;
                    stream.write_all(b"235 Authentication successful\r\n").await?;
                } else {
                    stream.write_all(b"535 Authentication failed\r\n").await?;
                }
            } else {
                stream.write_all(b"501 Cannot decode\r\n").await?;
            }
            stream.flush().await?;
            session.auth_state = AuthState::None;
            return Ok(CommandResult::Continue);
        }
        AuthState::None => {}
    }

    if cmd.starts_with("EHLO") || cmd.starts_with("HELO") {
        stream.write_all(b"250-localhost Hello\r\n").await?;
        stream.write_all(b"250-SIZE 10485760\r\n").await?;
        stream.write_all(b"250-8BITMIME\r\n").await?;
        if config.tls_acceptor.is_some() && !session.tls_active {
            stream.write_all(b"250-STARTTLS\r\n").await?;
        }
        if config.auth_username.is_some() {
            stream.write_all(b"250-AUTH PLAIN LOGIN\r\n").await?;
        }
        stream.write_all(b"250 OK\r\n").await?;
    } else if cmd.starts_with("STARTTLS") {
        if config.tls_acceptor.is_none() {
            stream.write_all(b"454 TLS not available\r\n").await?;
        } else if session.tls_active {
            stream.write_all(b"503 TLS already active\r\n").await?;
        } else {
            stream.write_all(b"220 Ready to start TLS\r\n").await?;
            stream.flush().await?;
            return Ok(CommandResult::StartTls);
        }
    } else if cmd.starts_with("AUTH ") {
        if config.auth_username.is_none() {
            stream.write_all(b"503 AUTH not available\r\n").await?;
        } else {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                let mechanism = parts[1].to_uppercase();
                match mechanism.as_str() {
                    "PLAIN" => {
                        if parts.len() > 2 {
                            verify_plain_auth(parts[2], session, config, stream).await?;
                        } else {
                            stream.write_all(b"334 \r\n").await?;
                            session.auth_state = AuthState::WaitingForPlain;
                        }
                    }
                    "LOGIN" => {
                        stream.write_all(b"334 VXNlcm5hbWU6\r\n").await?;
                        session.auth_state = AuthState::WaitingForLoginUsername;
                    }
                    _ => {
                        stream.write_all(b"504 Unrecognized auth type\r\n").await?;
                    }
                }
            } else {
                stream.write_all(b"501 Syntax error\r\n").await?;
            }
        }
    } else if cmd.starts_with("MAIL FROM:") {
        if session.auth_required && !session.authenticated {
            stream.write_all(b"530 Authentication required\r\n").await?;
        } else {
            let addr = extract_address(&trimmed[10..]);
            if !config.whitelist.is_empty() && !config.whitelist.contains(&addr.to_lowercase()) {
                stream.write_all(b"550 Sender not allowed\r\n").await?;
            } else {
                session.mail_from = Some(addr);
                stream.write_all(b"250 OK\r\n").await?;
            }
        }
    } else if cmd.starts_with("RCPT TO:") {
        if session.auth_required && !session.authenticated {
            stream.write_all(b"530 Authentication required\r\n").await?;
        } else if session.mail_from.is_none() {
            stream.write_all(b"503 MAIL FROM required first\r\n").await?;
        } else {
            // Catch-all: accept any recipient
            let addr = extract_address(&trimmed[8..]);
            session.rcpt_to.push(addr);
            stream.write_all(b"250 OK\r\n").await?;
        }
    } else if cmd == "DATA" {
        if session.auth_required && !session.authenticated {
            stream.write_all(b"530 Authentication required\r\n").await?;
        } else if session.mail_from.is_none() {
            stream.write_all(b"503 MAIL FROM required first\r\n").await?;
        } else if session.rcpt_to.is_empty() {
            stream.write_all(b"503 RCPT TO required first\r\n").await?;
        } else {
            stream.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n").await?;
            stream.flush().await?;

            let data = read_data(&mut stream.inner).await?;
            let record = parse_email(&data, session);
            
            // Forward email if configured
            if let Some(ref fwd) = config.forward_handle {
                fwd.forward(record.clone()).await;
            }
            
            store.push(record);
            session.reset();

            stream.write_all(b"250 OK: queued\r\n").await?;
        }
    } else if cmd == "RSET" {
        session.reset();
        stream.write_all(b"250 OK\r\n").await?;
    } else if cmd == "NOOP" {
        stream.write_all(b"250 OK\r\n").await?;
    } else if cmd == "QUIT" {
        stream.write_all(b"221 Bye\r\n").await?;
        stream.flush().await?;
        return Ok(CommandResult::Quit);
    } else {
        stream.write_all(b"500 Command not recognized\r\n").await?;
    }

    stream.flush().await?;
    Ok(CommandResult::Continue)
}

async fn verify_plain_auth<S: AsyncRead + AsyncWrite + Unpin>(
    encoded: &str,
    session: &mut Session,
    config: &SmtpConfig,
    stream: &mut BufStream<S>,
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
                stream.write_all(b"235 Authentication successful\r\n").await?;
                stream.flush().await?;
                return Ok(());
            }
        }
    }

    stream.write_all(b"535 Authentication failed\r\n").await?;
    stream.flush().await?;
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

#[allow(clippy::similar_names, clippy::too_many_lines)]
fn parse_email(data: &[u8], session: &Session) -> MailRecord {
    use crate::email::Attachment;
    
    let parser = MessageParser::default();
    let parsed = parser.parse(data);

    let id = format!(
        "{}-{}",
        chrono::Utc::now().timestamp_millis(),
        Uuid::new_v4().simple()
    );

    let (from, to, subject, text, html, mail_date, headers, attachments) = parsed.map_or_else(
        || {
            (
                session.mail_from.clone().unwrap_or_else(|| "unknown".to_string()),
                session.rcpt_to.clone(),
                None,
                None,
                None,
                chrono::Utc::now().to_rfc3339(),
                HashMap::new(),
                Vec::new(),
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

            // Extract attachments
            let attachments: Vec<Attachment> = msg
                .attachments()
                .map(|part| {
                    let filename = part.attachment_name().map_or_else(
                        || format!("attachment_{}", uuid::Uuid::new_v4().simple()),
                        String::from,
                    );
                    
                    let content_type = part.content_type().map_or_else(
                        || "application/octet-stream".to_string(),
                        |ct| ct.subtype().map_or_else(
                            || ct.ctype().to_string(),
                            |subtype| format!("{}/{}", ct.ctype(), subtype),
                        ),
                    );
                    
                    let content_id = part.content_id().map(|s| s.trim_matches(['<', '>']).to_string());
                    let data = part.contents().to_vec();
                    let size = data.len();
                    
                    Attachment {
                        filename,
                        content_type,
                        size,
                        content_id,
                        data,
                    }
                })
                .collect();

            (from, to, subject, text, html, mail_date, headers, attachments)
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
        attachments,
        raw: data.to_vec(),
    }
}
