//! Email record type.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents a stored email attachment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    /// Content ID for inline attachments (e.g., cid:image001)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_id: Option<String>,
    /// Base64-encoded content (not serialized to API by default)
    #[serde(skip)]
    pub data: Vec<u8>,
}

/// Represents a stored email.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailRecord {
    pub id: String,
    pub to: Vec<String>,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    pub date: String,
    pub headers: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    /// Raw email data for attachment retrieval
    #[serde(skip)]
    pub raw: Vec<u8>,
}
