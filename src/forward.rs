//! Email forwarding to external SMTP servers.

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::email::MailRecord;

/// Configuration for email forwarding.
#[derive(Debug, Clone)]
pub struct ForwardConfig {
    /// SMTP host to forward to
    pub host: String,
    /// SMTP port (default: 25 or 587)
    pub port: u16,
    /// Use TLS (STARTTLS or implicit)
    pub tls: bool,
    /// Use implicit TLS (SMTPS)
    pub implicit_tls: bool,
    /// Username for SMTP AUTH
    pub username: Option<String>,
    /// Password for SMTP AUTH
    pub password: Option<String>,
    /// Rewrite all recipients to this address
    pub to_override: Option<String>,
    /// Rewrite sender to this address
    pub from_override: Option<String>,
}

impl ForwardConfig {
    /// Check if forwarding is configured.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        !self.host.is_empty()
    }
}

/// Email forwarder that sends emails to an external SMTP server.
pub struct Forwarder {
    config: ForwardConfig,
}

impl Forwarder {
    /// Create a new forwarder with the given config.
    /// Returns the forwarder and a handle for submitting emails.
    #[must_use]
    pub fn new(config: ForwardConfig) -> (Arc<Self>, ForwardHandle) {
        let (tx, rx) = mpsc::channel(100);
        let forwarder = Arc::new(Self { config });

        // Spawn the forwarding task
        let forwarder_clone = Arc::clone(&forwarder);
        tokio::spawn(async move {
            forwarder_clone.run(rx).await;
        });

        let handle = ForwardHandle { tx };
        (forwarder, handle)
    }

    async fn run(self: Arc<Self>, mut rx: mpsc::Receiver<MailRecord>) {
        info!(
            "Email forwarder started, forwarding to {}:{}",
            self.config.host, self.config.port
        );

        while let Some(email) = rx.recv().await {
            if let Err(e) = self.forward_email(&email).await {
                error!("Failed to forward email {}: {}", email.id, e);
            } else {
                debug!("Forwarded email {} to {}", email.id, self.config.host);
            }
        }
    }

    async fn forward_email(&self, email: &MailRecord) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Build the email message
        let from: Mailbox = self
            .config
            .from_override
            .as_ref()
            .unwrap_or(&email.from)
            .parse()
            .map_err(|_| format!("Invalid from address: {}", email.from))?;

        // Determine recipients
        let recipients: Vec<String> = self.config.to_override.as_ref().map_or_else(
            || email.to.clone(),
            |override_to| vec![override_to.clone()],
        );

        if recipients.is_empty() {
            return Err("No recipients".into());
        }

        // Build message
        let mut builder = Message::builder()
            .from(from)
            .subject(email.subject.as_deref().unwrap_or("(no subject)"));

        for recipient in &recipients {
            let mailbox: Mailbox = recipient
                .parse()
                .map_err(|_| format!("Invalid recipient: {recipient}"))?;
            builder = builder.to(mailbox);
        }

        // Add Date header
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&email.date) {
            builder = builder.date(std::time::SystemTime::from(dt));
        }

        // Create the message with body
        let message = if let Some(ref html) = email.html {
            if let Some(ref text) = email.text {
                builder.multipart(
                    lettre::message::MultiPart::alternative()
                        .singlepart(
                            lettre::message::SinglePart::builder()
                                .header(lettre::message::header::ContentType::TEXT_PLAIN)
                                .body(text.clone()),
                        )
                        .singlepart(
                            lettre::message::SinglePart::builder()
                                .header(lettre::message::header::ContentType::TEXT_HTML)
                                .body(html.clone()),
                        ),
                )?
            } else {
                builder.header(lettre::message::header::ContentType::TEXT_HTML).body(html.clone())?
            }
        } else if let Some(ref text) = email.text {
            builder.header(lettre::message::header::ContentType::TEXT_PLAIN).body(text.clone())?
        } else {
            builder.body(String::new())?
        };

        // Build SMTP transport
        let transport = self.build_transport()?;

        // Send the email
        transport.send(message).await?;

        Ok(())
    }

    fn build_transport(
        &self,
    ) -> Result<AsyncSmtpTransport<Tokio1Executor>, Box<dyn std::error::Error + Send + Sync>> {
        let mut builder = if self.config.implicit_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.config.host)?
        } else if self.config.tls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.config.host)?
        } else {
            let tls_params = TlsParameters::builder(self.config.host.clone())
                .dangerous_accept_invalid_certs(true)
                .build()?;
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&self.config.host)
                .tls(Tls::Opportunistic(tls_params))
        };

        builder = builder.port(self.config.port);

        if let (Some(ref user), Some(ref pass)) = (&self.config.username, &self.config.password) {
            builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
        }

        Ok(builder.build())
    }
}

/// Handle for submitting emails to be forwarded.
#[derive(Clone)]
pub struct ForwardHandle {
    tx: mpsc::Sender<MailRecord>,
}

impl ForwardHandle {
    /// Queue an email for forwarding.
    pub async fn forward(&self, email: MailRecord) {
        if let Err(e) = self.tx.send(email).await {
            error!("Failed to queue email for forwarding: {}", e);
        }
    }
}
