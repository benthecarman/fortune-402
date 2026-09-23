use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use rusqlite::{params, Connection};

/// Restart-durable record of consumed x402 payment proofs.
///
/// A Lightning preimage is a bearer proof with no public spent marker, so each
/// one may only buy a single fortune. Keys are only removed once the invoice
/// they came from can no longer pass validation.
#[derive(Clone)]
pub struct ReplayStore {
    conn: Arc<Mutex<Connection>>,
}

impl ReplayStore {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open replay store {}", path.display()))?;
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS consumed (
                key TEXT PRIMARY KEY NOT NULL,
                retain_until INTEGER NOT NULL
            ) STRICT",
        )?;
        Ok(ReplayStore {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Atomically records `key` as consumed until at least `retain_until`
    /// (unix seconds). Returns `false` if it was already consumed.
    pub fn consume(&self, key: &str, retain_until: u64) -> anyhow::Result<bool> {
        let inserted = self.lock().execute(
            "INSERT INTO consumed (key, retain_until) VALUES (?1, ?2) ON CONFLICT (key) DO NOTHING",
            params![key, to_sql_time(retain_until)],
        )?;
        Ok(inserted == 1)
    }

    /// Deletes keys whose retention ended before `now`, returning how many.
    pub fn prune(&self, now: u64) -> anyhow::Result<usize> {
        let deleted = self.lock().execute(
            "DELETE FROM consumed WHERE retain_until < ?1",
            params![to_sql_time(now)],
        )?;
        Ok(deleted)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("replay store lock poisoned")
    }
}

/// SQLite integers are signed; clamp so far-future times stay far-future.
fn to_sql_time(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> ReplayStore {
        ReplayStore::open(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn consume_once() {
        let store = memory();
        assert!(store.consume("net:aa", 100).unwrap());
        assert!(!store.consume("net:aa", 100).unwrap());
        assert!(store.consume("net:bb", 100).unwrap());
    }

    #[test]
    fn prune_keeps_unexpired_keys() {
        let store = memory();
        store.consume("old", 100).unwrap();
        store.consume("boundary", 200).unwrap();
        store.consume("new", 300).unwrap();

        assert_eq!(store.prune(200).unwrap(), 1);
        assert!(store.consume("old", 100).unwrap());
        assert!(!store.consume("boundary", 200).unwrap());
        assert!(!store.consume("new", 300).unwrap());
    }

    #[test]
    fn survives_reopen() {
        let path = std::env::temp_dir().join(format!(
            "fortune-402-replay-{}.db",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        {
            let store = ReplayStore::open(&path).unwrap();
            assert!(store.consume("net:aa", u64::MAX).unwrap());
        }
        let store = ReplayStore::open(&path).unwrap();
        assert!(!store.consume("net:aa", u64::MAX).unwrap());
        drop(store);

        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
