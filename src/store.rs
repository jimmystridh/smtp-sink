//! Thread-safe ring buffer email store.

use crate::email::MailRecord;
use std::sync::RwLock;
use tokio::sync::broadcast;

/// Thread-safe email store with a maximum capacity (ring buffer).
pub struct EmailStore {
    max: usize,
    emails: RwLock<Vec<MailRecord>>,
    notify: broadcast::Sender<()>,
}

impl EmailStore {
    /// Create a new email store with the given maximum capacity.
    #[must_use]
    pub const fn new(max: usize, notify: broadcast::Sender<()>) -> Self {
        Self {
            max,
            emails: RwLock::new(Vec::new()),
            notify,
        }
    }

    /// Add an email to the store.
    pub fn push(&self, email: MailRecord) {
        let mut emails = self.emails.write().unwrap();
        emails.push(email);
        while emails.len() > self.max {
            emails.remove(0);
        }
        drop(emails);
        let _ = self.notify.send(());
    }

    /// Get all stored emails.
    #[must_use]
    pub fn get_all(&self) -> Vec<MailRecord> {
        self.emails.read().unwrap().clone()
    }

    /// Clear all stored emails.
    pub fn clear(&self) {
        self.emails.write().unwrap().clear();
        let _ = self.notify.send(());
    }

    /// Remove an email by ID. Returns true if found and removed.
    pub fn remove(&self, id: &str) -> bool {
        let mut emails = self.emails.write().unwrap();
        let Some(pos) = emails.iter().position(|e| e.id == id) else {
            return false;
        };
        emails.remove(pos);
        drop(emails);
        let _ = self.notify.send(());
        true
    }
}
