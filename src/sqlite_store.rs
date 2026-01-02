//! `SQLite`-backed email storage for persistence.

use crate::email::{Attachment, MailRecord};
use crate::store::{EmailQuery, EmailStorage};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

/// `SQLite`-backed email store with persistence.
pub struct SqliteStore {
    conn: Mutex<Connection>,
    max: usize,
    notify: broadcast::Sender<()>,
}

impl SqliteStore {
    /// Create or open a `SQLite` database at the given path.
    pub fn open(path: &str, max: usize, notify: broadcast::Sender<()>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS emails (
                id TEXT PRIMARY KEY,
                from_addr TEXT NOT NULL,
                to_addrs TEXT NOT NULL,
                subject TEXT,
                text_body TEXT,
                html_body TEXT,
                date TEXT NOT NULL,
                headers TEXT NOT NULL,
                raw BLOB NOT NULL,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            );
            
            CREATE TABLE IF NOT EXISTS attachments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                email_id TEXT NOT NULL,
                filename TEXT NOT NULL,
                content_type TEXT NOT NULL,
                content_id TEXT,
                data BLOB NOT NULL,
                size INTEGER NOT NULL,
                FOREIGN KEY (email_id) REFERENCES emails(id) ON DELETE CASCADE
            );
            
            CREATE INDEX IF NOT EXISTS idx_emails_date ON emails(date);
            CREATE INDEX IF NOT EXISTS idx_emails_from ON emails(from_addr);
            CREATE INDEX IF NOT EXISTS idx_attachments_email ON attachments(email_id);
            ",
        )?;
        
        Ok(Self {
            conn: Mutex::new(conn),
            max,
            notify,
        })
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn enforce_max(&self, conn: &Connection) {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM emails", [], |row| row.get(0))
            .unwrap_or(0);
        
        if count as usize > self.max {
            let to_delete = count as usize - self.max;
            let _ = conn.execute(
                "DELETE FROM emails WHERE id IN (SELECT id FROM emails ORDER BY created_at ASC LIMIT ?)",
                params![to_delete],
            );
        }
    }

    fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailRecord> {
        let id: String = row.get(0)?;
        let from: String = row.get(1)?;
        let to_json: String = row.get(2)?;
        let subject: Option<String> = row.get(3)?;
        let text: Option<String> = row.get(4)?;
        let html: Option<String> = row.get(5)?;
        let date: String = row.get(6)?;
        let headers_json: String = row.get(7)?;
        let raw: Vec<u8> = row.get(8)?;
        
        let to: Vec<String> = serde_json::from_str(&to_json).unwrap_or_default();
        let headers: HashMap<String, String> = serde_json::from_str(&headers_json).unwrap_or_default();
        
        Ok(MailRecord {
            id,
            from,
            to,
            subject,
            text,
            html,
            date,
            headers,
            attachments: Vec::new(), // Loaded separately
            raw,
        })
    }

    fn load_attachments(conn: &Connection, email_id: &str) -> Vec<Attachment> {
        let Ok(mut stmt) = conn
            .prepare("SELECT filename, content_type, content_id, data, size FROM attachments WHERE email_id = ?")
        else {
            return Vec::new();
        };
        
        stmt.query_map(params![email_id], |row| {
            Ok(Attachment {
                filename: row.get(0)?,
                content_type: row.get(1)?,
                content_id: row.get(2)?,
                data: row.get(3)?,
                size: row.get(4)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }
}

#[allow(clippy::significant_drop_tightening)]
impl EmailStorage for SqliteStore {
    fn push(&self, email: MailRecord) {
        let conn = self.conn.lock().unwrap();
        
        let to_json = serde_json::to_string(&email.to).unwrap_or_default();
        let headers_json = serde_json::to_string(&email.headers).unwrap_or_default();
        
        let result = conn.execute(
            "INSERT INTO emails (id, from_addr, to_addrs, subject, text_body, html_body, date, headers, raw)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                email.id,
                email.from,
                to_json,
                email.subject,
                email.text,
                email.html,
                email.date,
                headers_json,
                email.raw,
            ],
        );
        
        if result.is_ok() {
            for att in &email.attachments {
                let _ = conn.execute(
                    "INSERT INTO attachments (email_id, filename, content_type, content_id, data, size)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![
                        email.id,
                        att.filename,
                        att.content_type,
                        att.content_id,
                        att.data,
                        att.size,
                    ],
                );
            }
            
            self.enforce_max(&conn);
        }
        
        drop(conn);
        let _ = self.notify.send(());
    }

    fn get_all(&self) -> Vec<MailRecord> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, from_addr, to_addrs, subject, text_body, html_body, date, headers, raw FROM emails ORDER BY created_at DESC")
            .unwrap();
        
        let mut records: Vec<MailRecord> = stmt
            .query_map([], Self::row_to_record)
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default();
        
        for record in &mut records {
            record.attachments = Self::load_attachments(&conn, &record.id);
        }
        
        records
    }

    fn get_by_id(&self, id: &str) -> Option<MailRecord> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT id, from_addr, to_addrs, subject, text_body, html_body, date, headers, raw FROM emails WHERE id = ?")
            .ok()?;
        
        let mut record = stmt.query_row(params![id], Self::row_to_record).ok()?;
        record.attachments = Self::load_attachments(&conn, id);
        Some(record)
    }

    fn query(&self, query: &EmailQuery) -> Vec<MailRecord> {
        let conn = self.conn.lock().unwrap();
        
        let mut sql = String::from(
            "SELECT id, from_addr, to_addrs, subject, text_body, html_body, date, headers, raw FROM emails WHERE 1=1"
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        
        if let Some(ref from) = query.from {
            sql.push_str(" AND from_addr LIKE ?");
            params_vec.push(Box::new(format!("%{from}%")));
        }
        if let Some(ref to) = query.to {
            sql.push_str(" AND to_addrs LIKE ?");
            params_vec.push(Box::new(format!("%{to}%")));
        }
        if let Some(ref subject) = query.subject {
            sql.push_str(" AND subject LIKE ?");
            params_vec.push(Box::new(format!("%{subject}%")));
        }
        if let Some(ref since) = query.since {
            sql.push_str(" AND date >= ?");
            params_vec.push(Box::new(since.clone()));
        }
        if let Some(ref until) = query.until {
            sql.push_str(" AND date <= ?");
            params_vec.push(Box::new(until.clone()));
        }
        
        sql.push_str(" ORDER BY created_at DESC");
        
        let Ok(mut stmt) = conn.prepare(&sql) else {
            return Vec::new();
        };
        
        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(AsRef::as_ref).collect();
        
        let mut records: Vec<MailRecord> = stmt
            .query_map(params_refs.as_slice(), Self::row_to_record)
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default();
        
        for record in &mut records {
            record.attachments = Self::load_attachments(&conn, &record.id);
        }
        
        records
    }

    fn get_attachment(&self, email_id: &str, filename: &str) -> Option<Attachment> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT filename, content_type, content_id, data, size FROM attachments WHERE email_id = ? AND filename = ?")
            .ok()?;
        
        stmt.query_row(params![email_id, filename], |row| {
            Ok(Attachment {
                filename: row.get(0)?,
                content_type: row.get(1)?,
                content_id: row.get(2)?,
                data: row.get(3)?,
                size: row.get(4)?,
            })
        })
        .ok()
    }

    fn get_attachment_by_cid(&self, email_id: &str, cid: &str) -> Option<Attachment> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT filename, content_type, content_id, data, size FROM attachments WHERE email_id = ? AND content_id = ?")
            .ok()?;
        
        stmt.query_row(params![email_id, cid], |row| {
            Ok(Attachment {
                filename: row.get(0)?,
                content_type: row.get(1)?,
                content_id: row.get(2)?,
                data: row.get(3)?,
                size: row.get(4)?,
            })
        })
        .ok()
    }

    fn clear(&self) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("DELETE FROM emails", []);
        drop(conn);
        let _ = self.notify.send(());
    }

    fn remove(&self, id: &str) -> bool {
        let conn = self.conn.lock().unwrap();
        let result = conn.execute("DELETE FROM emails WHERE id = ?", params![id]);
        let removed = result.map(|n| n > 0).unwrap_or(false);
        drop(conn);
        if removed {
            let _ = self.notify.send(());
        }
        removed
    }

    fn close(&self) {
        let conn = self.conn.lock().unwrap();
        // Checkpoint WAL to ensure all data is written to the main database file
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        tracing::info!("SQLite database checkpointed and ready for shutdown");
    }
}
