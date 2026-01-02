//! Thread-safe email stores with in-memory and `SQLite` backends.

use crate::email::{Attachment, MailRecord};
use std::sync::RwLock;
use tokio::sync::broadcast;

/// Query parameters for filtering emails.
#[derive(Debug, Default, Clone)]
pub struct EmailQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
}

/// Trait for email storage backends.
pub trait EmailStorage: Send + Sync {
    fn push(&self, email: MailRecord);
    fn get_all(&self) -> Vec<MailRecord>;
    fn get_by_id(&self, id: &str) -> Option<MailRecord>;
    fn query(&self, query: &EmailQuery) -> Vec<MailRecord>;
    fn get_attachment(&self, email_id: &str, filename: &str) -> Option<Attachment>;
    fn get_attachment_by_cid(&self, email_id: &str, cid: &str) -> Option<Attachment>;
    fn clear(&self);
    fn remove(&self, id: &str) -> bool;
    /// Perform any cleanup needed before shutdown (e.g., flush `SQLite` WAL)
    fn close(&self) {}
}

/// Thread-safe in-memory email store with a maximum capacity (ring buffer).
pub struct MemoryStore {
    max: usize,
    emails: RwLock<Vec<MailRecord>>,
    notify: broadcast::Sender<()>,
}

impl MemoryStore {
    /// Create a new email store with the given maximum capacity.
    #[must_use]
    pub const fn new(max: usize, notify: broadcast::Sender<()>) -> Self {
        Self {
            max,
            emails: RwLock::new(Vec::new()),
            notify,
        }
    }
}

impl EmailStorage for MemoryStore {
    fn push(&self, email: MailRecord) {
        let mut emails = self.emails.write().expect("lock poisoned");
        emails.push(email);
        while emails.len() > self.max {
            emails.remove(0);
        }
        drop(emails);
        let _ = self.notify.send(());
    }

    fn get_all(&self) -> Vec<MailRecord> {
        self.emails.read().expect("lock poisoned").clone()
    }

    fn get_by_id(&self, id: &str) -> Option<MailRecord> {
        self.emails.read().expect("lock poisoned").iter().find(|e| e.id == id).cloned()
    }

    fn query(&self, query: &EmailQuery) -> Vec<MailRecord> {
        let emails = self.emails.read().expect("lock poisoned");
        emails
            .iter()
            .filter(|e| matches_query(e, query))
            .cloned()
            .collect()
    }

    #[allow(clippy::significant_drop_tightening)]
    fn get_attachment(&self, email_id: &str, filename: &str) -> Option<Attachment> {
        let emails = self.emails.read().expect("lock poisoned");
        let email = emails.iter().find(|e| e.id == email_id)?;
        email.attachments.iter().find(|a| a.filename == filename).cloned()
    }

    #[allow(clippy::significant_drop_tightening)]
    fn get_attachment_by_cid(&self, email_id: &str, cid: &str) -> Option<Attachment> {
        let emails = self.emails.read().expect("lock poisoned");
        let email = emails.iter().find(|e| e.id == email_id)?;
        email.attachments.iter().find(|a| a.content_id.as_deref() == Some(cid)).cloned()
    }

    fn clear(&self) {
        self.emails.write().expect("lock poisoned").clear();
        let _ = self.notify.send(());
    }

    fn remove(&self, id: &str) -> bool {
        let mut emails = self.emails.write().expect("lock poisoned");
        let Some(pos) = emails.iter().position(|e| e.id == id) else {
            return false;
        };
        emails.remove(pos);
        drop(emails);
        let _ = self.notify.send(());
        true
    }
}

fn matches_query(email: &MailRecord, query: &EmailQuery) -> bool {
    if let Some(ref from) = query.from {
        if !email.from.to_lowercase().contains(&from.to_lowercase()) {
            return false;
        }
    }
    if let Some(ref to) = query.to {
        let to_lower = to.to_lowercase();
        if !email.to.iter().any(|t| t.to_lowercase().contains(&to_lower)) {
            return false;
        }
    }
    if let Some(ref subject) = query.subject {
        let subj = email.subject.as_deref().unwrap_or("");
        if !subj.to_lowercase().contains(&subject.to_lowercase()) {
            return false;
        }
    }
    if let Some(ref since) = query.since {
        if email.date < *since {
            return false;
        }
    }
    if let Some(ref until) = query.until {
        if email.date > *until {
            return false;
        }
    }
    true
}

/// Backward-compatible type alias.
pub type EmailStore = MemoryStore;
