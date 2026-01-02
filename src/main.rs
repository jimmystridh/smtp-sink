//! CLI entry point for smtp-sink.

use clap::Parser;
use smtp_sink::{start_sink, SinkOptions};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "smtp-sink")]
#[command(about = "Receive emails via SMTP and expose them via HTTP for testing")]
#[allow(clippy::struct_excessive_bools)]
struct Cli {
    /// SMTP port to listen on
    #[arg(short = 's', long, default_value = "1025")]
    smtp_port: u16,

    /// HTTP port to listen on
    #[arg(short = 'p', long, default_value = "1080")]
    http_port: u16,

    /// Comma-separated list of allowed sender addresses
    #[arg(short = 'w', long)]
    whitelist: Option<String>,

    /// Max number of emails to keep
    #[arg(short = 'm', long, default_value = "10")]
    max: usize,

    /// Enable implicit TLS (SMTPS) - TLS from connection start
    #[arg(long)]
    tls: bool,

    /// Enable STARTTLS - upgrade plain connection to TLS
    #[arg(long)]
    starttls: bool,

    /// Path to PEM private key for TLS
    #[arg(long)]
    tls_key: Option<String>,

    /// Path to PEM certificate for TLS
    #[arg(long)]
    tls_cert: Option<String>,

    /// Generate a self-signed certificate
    #[arg(long)]
    tls_self_signed: bool,

    /// Require SMTP AUTH before sending
    #[arg(long)]
    auth_required: bool,

    /// Username for SMTP AUTH
    #[arg(long)]
    auth_username: Option<String>,

    /// Password for SMTP AUTH
    #[arg(long)]
    auth_password: Option<String>,

    /// `SQLite` database path for persistence (e.g., ./emails.db)
    #[arg(long)]
    db: Option<String>,

    /// Forward emails to this SMTP host
    #[arg(long)]
    forward_host: Option<String>,

    /// Forward SMTP port (default: 587 for STARTTLS, 465 for implicit TLS)
    #[arg(long)]
    forward_port: Option<u16>,

    /// Use STARTTLS when forwarding
    #[arg(long)]
    forward_tls: bool,

    /// Use implicit TLS (SMTPS) when forwarding
    #[arg(long)]
    forward_implicit_tls: bool,

    /// SMTP username for forwarding
    #[arg(long)]
    forward_username: Option<String>,

    /// SMTP password for forwarding
    #[arg(long)]
    forward_password: Option<String>,

    /// Override all recipients when forwarding (catch-all redirect)
    #[arg(long)]
    forward_to: Option<String>,

    /// Override sender when forwarding
    #[arg(long)]
    forward_from: Option<String>,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();

    let whitelist: Vec<String> = cli
        .whitelist
        .map(|s| s.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    let opts = SinkOptions {
        smtp_port: Some(cli.smtp_port),
        http_port: Some(cli.http_port),
        whitelist,
        max: Some(cli.max),
        tls: cli.tls,
        starttls: cli.starttls,
        tls_key_path: cli.tls_key,
        tls_cert_path: cli.tls_cert,
        tls_self_signed: cli.tls_self_signed,
        auth_required: cli.auth_required,
        auth_username: cli.auth_username,
        auth_password: cli.auth_password,
        db_path: cli.db,
        forward_host: cli.forward_host,
        forward_port: cli.forward_port,
        forward_tls: cli.forward_tls,
        forward_implicit_tls: cli.forward_implicit_tls,
        forward_username: cli.forward_username,
        forward_password: cli.forward_password,
        forward_to: cli.forward_to,
        forward_from: cli.forward_from,
    };

    let servers = start_sink(opts).await?;

    // Wait for shutdown signal (Ctrl+C or SIGTERM)
    let shutdown = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
            let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT");
            tokio::select! {
                _ = sigterm.recv() => info!("Received SIGTERM"),
                _ = sigint.recv() => info!("Received SIGINT"),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.expect("failed to listen for Ctrl+C");
            info!("Received Ctrl+C");
        }
    };

    shutdown.await;
    info!("Initiating graceful shutdown...");
    servers.stop().await;
    info!("Shutdown complete");

    Ok(())
}
