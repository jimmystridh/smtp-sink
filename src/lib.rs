//! SMTP sink library for receiving and exposing emails via HTTP.

mod email;
pub mod error;
mod forward;
mod http;
mod smtp;
mod sqlite_store;
mod store;
mod tls;

pub use email::{Attachment, MailRecord};
pub use error::{Error, Result, SmtpError};
pub use forward::{ForwardConfig, ForwardHandle, Forwarder};
pub use smtp::SmtpConfig;
pub use sqlite_store::SqliteStore;
pub use store::{EmailQuery, EmailStorage, EmailStore, MemoryStore};

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;

/// Configuration options for the SMTP sink.
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct SinkOptions {
    pub smtp_port: Option<u16>,
    pub http_port: Option<u16>,
    pub whitelist: Vec<String>,
    pub max: Option<usize>,
    /// Use implicit TLS (SMTPS) - TLS from connection start
    pub tls: bool,
    pub tls_key_path: Option<String>,
    pub tls_cert_path: Option<String>,
    pub tls_self_signed: bool,
    /// Enable STARTTLS (upgrade plain connection to TLS)
    pub starttls: bool,
    /// Require SMTP AUTH before sending
    pub auth_required: bool,
    /// Username for SMTP AUTH
    pub auth_username: Option<String>,
    /// Password for SMTP AUTH
    pub auth_password: Option<String>,
    /// `SQLite` database path for persistence
    pub db_path: Option<String>,
    /// Forward emails to external SMTP server
    pub forward_host: Option<String>,
    /// Forward SMTP port
    pub forward_port: Option<u16>,
    /// Use TLS for forwarding
    pub forward_tls: bool,
    /// Use implicit TLS for forwarding
    pub forward_implicit_tls: bool,
    /// Forward SMTP username
    pub forward_username: Option<String>,
    /// Forward SMTP password
    pub forward_password: Option<String>,
    /// Override all recipients when forwarding
    pub forward_to: Option<String>,
    /// Override sender when forwarding
    pub forward_from: Option<String>,
}

/// Running server handles.
pub struct RunningServers {
    pub smtp_addr: SocketAddr,
    pub http_addr: SocketAddr,
    smtp_handle: tokio::task::JoinHandle<()>,
    http_handle: tokio::task::JoinHandle<()>,
    shutdown_tx: broadcast::Sender<()>,
    store: Arc<dyn store::EmailStorage>,
}

impl RunningServers {
    /// Stop both servers gracefully.
    pub async fn stop(self) {
        let _ = self.shutdown_tx.send(());
        let _ = self.smtp_handle.await;
        let _ = self.http_handle.await;
        // Close the store (flushes SQLite WAL if applicable)
        self.store.close();
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

    // Create store - SQLite if db_path provided, otherwise in-memory
    let store: Arc<dyn EmailStorage> = if let Some(ref db_path) = opts.db_path {
        Arc::new(
            SqliteStore::open(db_path, max, email_tx.clone())
                .map_err(|e| std::io::Error::other(format!("Failed to open database: {e}")))?,
        )
    } else {
        Arc::new(MemoryStore::new(max, email_tx.clone()))
    };

    // Prepare TLS acceptor if TLS or STARTTLS is requested
    let tls_acceptor: Option<TlsAcceptor> = if opts.tls || opts.starttls {
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

    let mode = if opts.tls {
        "SMTPS"
    } else if opts.starttls {
        "SMTP+STARTTLS"
    } else {
        "SMTP"
    };
    let storage_mode = if opts.db_path.is_some() { "SQLite" } else { "memory" };
    println!("{mode} server listening on port {} ({storage_mode})", smtp_addr.port());
    println!(
        "HTTP server listening on port {}, emails at /emails",
        http_addr.port()
    );

    // Set up email forwarding if configured
    let forward_handle: Option<ForwardHandle> = if let Some(ref host) = opts.forward_host {
        let forward_config = ForwardConfig {
            host: host.clone(),
            port: opts.forward_port.unwrap_or(if opts.forward_implicit_tls { 465 } else { 587 }),
            tls: opts.forward_tls,
            implicit_tls: opts.forward_implicit_tls,
            username: opts.forward_username.clone(),
            password: opts.forward_password.clone(),
            to_override: opts.forward_to.clone(),
            from_override: opts.forward_from.clone(),
        };
        let (_, handle) = Forwarder::new(forward_config);
        println!("Forwarding emails to {}:{}", host, opts.forward_port.unwrap_or(587));
        Some(handle)
    } else {
        None
    };

    // Build SMTP config
    let smtp_config = SmtpConfig {
        whitelist: whitelist.clone(),
        tls_acceptor: if opts.starttls && !opts.tls {
            tls_acceptor.clone()
        } else {
            None
        },
        auth_required: opts.auth_required,
        auth_username: opts.auth_username.clone(),
        auth_password: opts.auth_password.clone(),
        forward_handle,
    };

    // Start SMTP server task
    let smtp_store = Arc::clone(&store);
    let smtp_shutdown = shutdown_tx.subscribe();
    let smtp_handle = if opts.tls {
        let acceptor = tls_acceptor.clone().expect("TLS acceptor required when tls=true");
        tokio::spawn(async move {
            smtp::run_smtps_server(smtp_listener, smtp_store, smtp_config, acceptor, smtp_shutdown)
                .await;
        })
    } else {
        tokio::spawn(async move {
            smtp::run_smtp_server(smtp_listener, smtp_store, smtp_config, smtp_shutdown).await;
        })
    };

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
        store,
    })
}
