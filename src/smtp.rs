//! SMTP server implementation.

use crate::email::MailRecord;
use crate::store::EmailStore;
use mail_parser::MessageParser;
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error};
use uuid::Uuid;

/// Run the SMTP server.
pub async fn run_smtp_server(
    listener: TcpListener,
    store: Arc<EmailStore>,
    whitelist: Vec<String>,
    tls_acceptor: Option<TlsAcceptor>,
    mut shutdown: broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, addr)) => {
                        debug!("SMTP connection from {addr}");
                        let store = Arc::clone(&store);
                        let whitelist = whitelist.clone();
                        let acceptor = tls_acceptor.clone();
                        tokio::spawn(async move {
                            if let Some(acceptor) = acceptor {
                                match acceptor.accept(stream).await {
                                    Ok(tls_stream) => {
                                        if let Err(e) = handle_connection(tls_stream, &store, &whitelist).await {
                                            debug!("SMTP session error: {e}");
                                        }
                                    }
                                    Err(e) => {
                                        debug!("TLS handshake failed: {e}");
                                    }
                                }
                            } else if let Err(e) = handle_connection(stream, &store, &whitelist).await {
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

/// SMTP session state.
struct Session {
    mail_from: Option<String>,
    rcpt_to: Vec<String>,
}

impl Session {
    const fn new() -> Self {
        Self {
            mail_from: None,
            rcpt_to: Vec::new(),
        }
    }

    fn reset(&mut self) {
        self.mail_from = None;
        self.rcpt_to.clear();
    }
}

async fn handle_connection<S>(stream: S, store: &EmailStore, whitelist: &[String]) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut session = Session::new();

    // Send greeting
    writer.write_all(b"220 localhost ESMTP\r\n").await?;
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

        if cmd.starts_with("HELO") || cmd.starts_with("EHLO") {
            writer.write_all(b"250 Hello localhost\r\n").await?;
        } else if cmd.starts_with("MAIL FROM:") {
            let addr = extract_address(&trimmed[10..]);
            if !whitelist.is_empty() {
                let lower = addr.to_lowercase();
                if !whitelist.contains(&lower) {
                    writer
                        .write_all(b"550 Sender not allowed\r\n")
                        .await?;
                    writer.flush().await?;
                    continue;
                }
            }
            session.mail_from = Some(addr);
            writer.write_all(b"250 OK\r\n").await?;
        } else if cmd.starts_with("RCPT TO:") {
            let addr = extract_address(&trimmed[8..]);
            session.rcpt_to.push(addr);
            writer.write_all(b"250 OK\r\n").await?;
        } else if cmd == "DATA" {
            writer
                .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                .await?;
            writer.flush().await?;

            let data = read_data(&mut reader).await?;
            let record = parse_email(&data, &session);
            store.push(record);
            session.reset();

            writer.write_all(b"250 OK: queued\r\n").await?;
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
        // Handle dot-stuffing
        let content = if line.starts_with("..") {
            &line[1..]
        } else {
            &line
        };
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
                session
                    .mail_from
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
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
                .map(|list| {
                    list.iter()
                        .filter_map(|a| a.address().map(String::from))
                        .collect()
                })
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
