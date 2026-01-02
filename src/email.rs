//! Email record type.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
}
