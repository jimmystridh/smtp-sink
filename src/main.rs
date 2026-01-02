//! CLI entry point for smtp-sink.

use clap::Parser;
use smtp_sink::{start_sink, SinkOptions};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "smtp-sink")]
#[command(about = "Receive emails via SMTP and expose them via HTTP for testing")]
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

    /// Max number of emails to keep in memory
    #[arg(short = 'm', long, default_value = "10")]
    max: usize,

    /// Enable TLS for SMTP (SMTPS)
    #[arg(long)]
    tls: bool,

    /// Path to PEM private key for SMTPS
    #[arg(long)]
    tls_key: Option<String>,

    /// Path to PEM certificate for SMTPS
    #[arg(long)]
    tls_cert: Option<String>,

    /// Generate a self-signed cert when TLS is enabled
    #[arg(long)]
    tls_self_signed: bool,
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
        tls_key_path: cli.tls_key,
        tls_cert_path: cli.tls_cert,
        tls_self_signed: cli.tls_self_signed,
    };

    let servers = start_sink(opts).await?;

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;
    servers.stop().await;

    Ok(())
}
