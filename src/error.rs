//! Custom error types for smtp-sink.

use std::fmt;
use std::io;

/// Main error type for SMTP sink operations.
#[derive(Debug)]
pub enum Error {
    /// I/O errors (network, file operations)
    Io(io::Error),
    /// TLS/certificate errors
    Tls(String),
    /// `SQLite` database errors
    Database(String),
    /// SMTP protocol errors
    Smtp(SmtpError),
    /// Email parsing errors
    Parse(String),
    /// Configuration errors
    Config(String),
    /// Email forwarding errors
    Forward(String),
}

/// SMTP protocol-specific errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmtpError {
    /// Authentication failed
    AuthFailed,
    /// Authentication required but not provided
    AuthRequired,
    /// Invalid command sequence
    BadSequence(String),
    /// Sender not in whitelist
    SenderNotAllowed(String),
    /// Invalid command syntax
    InvalidSyntax(String),
    /// Unknown command
    UnknownCommand(String),
    /// TLS not available
    TlsNotAvailable,
    /// TLS already active
    TlsAlreadyActive,
    /// Connection closed unexpectedly
    ConnectionClosed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Tls(msg) => write!(f, "TLS error: {msg}"),
            Self::Database(msg) => write!(f, "database error: {msg}"),
            Self::Smtp(e) => write!(f, "SMTP error: {e}"),
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
            Self::Config(msg) => write!(f, "configuration error: {msg}"),
            Self::Forward(msg) => write!(f, "forwarding error: {msg}"),
        }
    }
}

impl fmt::Display for SmtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthFailed => write!(f, "authentication failed"),
            Self::AuthRequired => write!(f, "authentication required"),
            Self::BadSequence(msg) => write!(f, "bad command sequence: {msg}"),
            Self::SenderNotAllowed(addr) => write!(f, "sender not allowed: {addr}"),
            Self::InvalidSyntax(msg) => write!(f, "invalid syntax: {msg}"),
            Self::UnknownCommand(cmd) => write!(f, "unknown command: {cmd}"),
            Self::TlsNotAvailable => write!(f, "TLS not available"),
            Self::TlsAlreadyActive => write!(f, "TLS already active"),
            Self::ConnectionClosed => write!(f, "connection closed unexpectedly"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl std::error::Error for SmtpError {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<SmtpError> for Error {
    fn from(e: SmtpError) -> Self {
        Self::Smtp(e)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e.to_string())
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(io_err) => io_err,
            other => Self::other(other.to_string()),
        }
    }
}

/// Result type alias for smtp-sink operations.
pub type Result<T> = std::result::Result<T, Error>;
