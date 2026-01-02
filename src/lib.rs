//! SMTP sink library for receiving and exposing emails via HTTP.

mod email;
mod http;
mod smtp;
mod store;
mod tls;

pub use email::MailRecord;
pub use store::EmailStore;

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;

/// Configuration options for the SMTP sink.
#[derive(Debug, Clone, Default)]
pub struct SinkOptions {
    pub smtp_port: Option<u16>,
    pub http_port: Option<u16>,
    pub whitelist: Vec<String>,
    pub max: Option<usize>,
    pub tls: bool,
    pub tls_key_path: Option<String>,
    pub tls_cert_path: Option<String>,
    pub tls_self_signed: bool,
}

/// Running server handles.
pub struct RunningServers {
    pub smtp_addr: SocketAddr,
    pub http_addr: SocketAddr,
    smtp_handle: tokio::task::JoinHandle<()>,
    http_handle: tokio::task::JoinHandle<()>,
    shutdown_tx: broadcast::Sender<()>,
}

impl RunningServers {
    /// Stop both servers gracefully.
    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.smtp_handle.await;
        let _ = self.http_handle.await;
    }
}

/// Start the SMTP sink with the given options.
pub async fn start_sink(opts: SinkOptions) -> std::io::Result<RunningServers> {
    let smtp_port = opts.smtp_port.unwrap_or(1025);
    let http_port = opts.http_port.unwrap_or(1080);
    let max = opts.max.unwrap_or(10);

    let whitelist: Vec<String> = opts
        .whitelist
        .iter()
        .map(|s| s.to_lowercase().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let (email_tx, _) = broadcast::channel::<()>(16);

    let store = Arc::new(EmailStore::new(max, email_tx.clone()));

    // Prepare TLS acceptor if requested
    let tls_acceptor: Option<TlsAcceptor> = if opts.tls {
        Some(tls::create_acceptor(&opts).await?)
    } else {
        None
    };

    // Bind SMTP listener
    let smtp_listener = TcpListener::bind(("0.0.0.0", smtp_port)).await?;
    let smtp_addr = smtp_listener.local_addr()?;

    // Bind HTTP listener
    let http_listener = TcpListener::bind(("0.0.0.0", http_port)).await?;
    let http_addr = http_listener.local_addr()?;

    println!("SMTP server listening on port {}", smtp_addr.port());
    println!(
        "HTTP server listening on port {}, emails at /emails",
        http_addr.port()
    );

    // Start SMTP server task
    let smtp_store = Arc::clone(&store);
    let smtp_shutdown = shutdown_tx.subscribe();
    let smtp_whitelist = whitelist.clone();
    let smtp_handle = tokio::spawn(async move {
        smtp::run_smtp_server(
            smtp_listener,
            smtp_store,
            smtp_whitelist,
            tls_acceptor,
            smtp_shutdown,
        )
        .await;
    });

    // Start HTTP server task
    let http_store = Arc::clone(&store);
    let http_shutdown = shutdown_tx.subscribe();
    let http_handle = tokio::spawn(async move {
        http::run_http_server(http_listener, http_store, email_tx, http_shutdown).await;
    });

    Ok(RunningServers {
        smtp_addr,
        http_addr,
        smtp_handle,
        http_handle,
        shutdown_tx,
    })
}
