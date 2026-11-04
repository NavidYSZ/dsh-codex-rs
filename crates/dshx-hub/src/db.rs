//! SQLite storage: the schema, its migrations and every query the hub runs.
//!
//! One connection behind a mutex is plenty for the hub's load; every method
//! takes the lock for one short statement and never holds it across an await.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

/// Each entry is one schema version, applied in a transaction; the current
/// version lives in `PRAGMA user_version`.
const MIGRATIONS: &[&str] = &[
    // v1: accounts, sessions, tiers, enrol codes, servers.
    "CREATE TABLE tiers (
         id TEXT PRIMARY KEY,
         name TEXT NOT NULL,
         servers INTEGER,
         relay INTEGER NOT NULL DEFAULT 1,
         price_eur_monthly REAL,
         sort INTEGER NOT NULL DEFAULT 0
     );
     INSERT INTO tiers (id, name, servers, relay, price_eur_monthly, sort) VALUES
         ('free', 'Free', 2, 1, NULL, 0),
         ('pro', 'Pro', 10, 1, 9.99, 1),
         ('team', 'Team', NULL, 1, 29, 2);
     CREATE TABLE users (
         id TEXT PRIMARY KEY,
         email TEXT NOT NULL UNIQUE,
         name TEXT,
         password_hash TEXT NOT NULL,
         email_verified INTEGER NOT NULL DEFAULT 0,
         verification_code TEXT,
         verification_expires_at INTEGER,
         tier_id TEXT NOT NULL DEFAULT 'free' REFERENCES tiers(id),
         created_at INTEGER NOT NULL
     );
     CREATE TABLE sessions (
         token_sha256 TEXT PRIMARY KEY,
         user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
         created_at INTEGER NOT NULL,
         expires_at INTEGER NOT NULL,
         last_used_at INTEGER NOT NULL
     );
     CREATE INDEX sessions_user ON sessions(user_id);
     CREATE TABLE enroll_codes (
         code TEXT PRIMARY KEY,
         user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
         created_at INTEGER NOT NULL,
         expires_at INTEGER NOT NULL,
         used_at INTEGER
     );
     CREATE INDEX enroll_codes_user ON enroll_codes(user_id);
     CREATE TABLE servers (
         id TEXT PRIMARY KEY,
         user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
         name TEXT NOT NULL,
         hostname TEXT NOT NULL,
         platform TEXT NOT NULL,
         version TEXT NOT NULL,
         key_sha256 TEXT NOT NULL UNIQUE,
         created_at INTEGER NOT NULL,
         last_seen_at INTEGER
     );
     CREATE INDEX servers_user ON servers(user_id);",
];

#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub password_hash: String,
    pub email_verified: bool,
    pub verification_code: Option<String>,
    pub verification_expires_at: Option<i64>,
    pub tier_id: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub token_sha256: String,
    pub user_id: String,
    pub expires_at: i64,
    pub last_used_at: i64,
}

#[derive(Debug, Clone)]
pub struct TierRow {
    pub id: String,
    pub name: String,
    pub servers: Option<i64>,
    pub relay: bool,
    pub price_eur_monthly: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct EnrollCodeRow {
    pub code: String,
    pub user_id: String,
    pub expires_at: i64,
    pub used_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ServerRow {
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub hostname: String,
    pub platform: String,
    pub version: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
}

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    /// Opens (or creates) the database, switches it to WAL and applies the
    /// pending migrations.
    pub fn open(path: &Path) -> rusqlite::Result<Db> {
        let conn = Connection::open(path)?;
        // journal_mode answers with a row, so it cannot go through execute_batch.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000; PRAGMA synchronous = NORMAL;",
        )?;
        migrate(&conn)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // Users

    pub fn insert_user(&self, user: &UserRow) -> rusqlite::Result<()> {
        self.lock().execute(
            "INSERT INTO users (id, email, name, password_hash, email_verified, verification_code,
                                verification_expires_at, tier_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                user.id,
                user.email,
                user.name,
                user.password_hash,
                user.email_verified,
                user.verification_code,
                user.verification_expires_at,
                user.tier_id,
                user.created_at
            ],
        )?;
        Ok(())
    }

    pub fn user_by_email(&self, email: &str) -> rusqlite::Result<Option<UserRow>> {
        self.lock()
            .query_row(
                &format!("{USER_SELECT} WHERE email = ?1"),
                params![email],
                user_row,
            )
            .optional()
    }

    pub fn user_by_id(&self, id: &str) -> rusqlite::Result<Option<UserRow>> {
        self.lock()
            .query_row(
                &format!("{USER_SELECT} WHERE id = ?1"),
                params![id],
                user_row,
            )
            .optional()
    }

    pub fn set_user_name(&self, id: &str, name: Option<&str>) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE users SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    pub fn set_password_hash(&self, id: &str, hash: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE users SET password_hash = ?2 WHERE id = ?1",
            params![id, hash],
        )?;
        Ok(())
    }

    pub fn set_email_verified(&self, id: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE users SET email_verified = 1, verification_code = NULL,
                              verification_expires_at = NULL WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn set_tier(&self, id: &str, tier_id: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE users SET tier_id = ?2 WHERE id = ?1",
            params![id, tier_id],
        )?;
        Ok(())
    }

    // Sessions

    pub fn insert_session(
        &self,
        token_sha256: &str,
        user_id: &str,
        now: i64,
        expires_at: i64,
    ) -> rusqlite::Result<()> {
        self.lock().execute(
            "INSERT INTO sessions (token_sha256, user_id, created_at, expires_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?3)",
            params![token_sha256, user_id, now, expires_at],
        )?;
        Ok(())
    }

    pub fn session(&self, token_sha256: &str) -> rusqlite::Result<Option<SessionRow>> {
        self.lock()
            .query_row(
                "SELECT token_sha256, user_id, expires_at, last_used_at FROM sessions
                 WHERE token_sha256 = ?1",
                params![token_sha256],
                |r| {
                    Ok(SessionRow {
                        token_sha256: r.get(0)?,
                        user_id: r.get(1)?,
                        expires_at: r.get(2)?,
                        last_used_at: r.get(3)?,
                    })
                },
            )
            .optional()
    }

    pub fn refresh_session(
        &self,
        token_sha256: &str,
        now: i64,
        expires_at: i64,
    ) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE sessions SET last_used_at = ?2, expires_at = ?3 WHERE token_sha256 = ?1",
            params![token_sha256, now, expires_at],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, token_sha256: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "DELETE FROM sessions WHERE token_sha256 = ?1",
            params![token_sha256],
        )?;
        Ok(())
    }

    /// Ends every session of a user except `keep` (the one changing the password).
    pub fn delete_other_sessions(&self, user_id: &str, keep: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "DELETE FROM sessions WHERE user_id = ?1 AND token_sha256 <> ?2",
            params![user_id, keep],
        )?;
        Ok(())
    }

    pub fn purge_expired(&self, now: i64) -> rusqlite::Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])?;
        conn.execute(
            "DELETE FROM enroll_codes WHERE expires_at <= ?1 OR used_at IS NOT NULL",
            params![now - 24 * 3_600_000],
        )?;
        Ok(())
    }

    // Tiers

    pub fn tiers(&self) -> rusqlite::Result<Vec<TierRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!("{TIER_SELECT} ORDER BY sort, id"))?;
        let rows = stmt.query_map([], tier_row)?;
        rows.collect()
    }

    pub fn tier(&self, id: &str) -> rusqlite::Result<Option<TierRow>> {
        self.lock()
            .query_row(
                &format!("{TIER_SELECT} WHERE id = ?1"),
                params![id],
                tier_row,
            )
            .optional()
    }

    // Enrol codes

    /// Stores a fresh code for the user; earlier unused codes of the user
    /// are dropped so only the newest one counts.
    pub fn replace_enroll_code(
        &self,
        user_id: &str,
        code: &str,
        now: i64,
        expires_at: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM enroll_codes WHERE user_id = ?1 AND used_at IS NULL",
            params![user_id],
        )?;
        tx.execute(
            "INSERT INTO enroll_codes (code, user_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![code, user_id, now, expires_at],
        )?;
        tx.commit()
    }

    pub fn enroll_code(&self, code: &str) -> rusqlite::Result<Option<EnrollCodeRow>> {
        self.lock()
            .query_row(
                "SELECT code, user_id, expires_at, used_at FROM enroll_codes WHERE code = ?1",
                params![code],
                |r| {
                    Ok(EnrollCodeRow {
                        code: r.get(0)?,
                        user_id: r.get(1)?,
                        expires_at: r.get(2)?,
                        used_at: r.get(3)?,
                    })
                },
            )
            .optional()
    }

    /// Marks the code used; false when it was used meanwhile.
    pub fn use_enroll_code(&self, code: &str, now: i64) -> rusqlite::Result<bool> {
        let changed = self.lock().execute(
            "UPDATE enroll_codes SET used_at = ?2 WHERE code = ?1 AND used_at IS NULL",
            params![code, now],
        )?;
        Ok(changed == 1)
    }

    // Servers

    pub fn count_servers(&self, user_id: &str) -> rusqlite::Result<i64> {
        self.lock().query_row(
            "SELECT COUNT(*) FROM servers WHERE user_id = ?1",
            params![user_id],
            |r| r.get(0),
        )
    }

    pub fn insert_server(&self, server: &ServerRow, key_sha256: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "INSERT INTO servers (id, user_id, name, hostname, platform, version, key_sha256,
                                  created_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                server.id,
                server.user_id,
                server.name,
                server.hostname,
                server.platform,
                server.version,
                key_sha256,
                server.created_at,
                server.last_seen_at
            ],
        )?;
        Ok(())
    }

    /// Re-enrolment of a known server: new key, fresh description, same row.
    pub fn reenroll_server(&self, server: &ServerRow, key_sha256: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE servers SET name = ?2, hostname = ?3, platform = ?4, version = ?5,
                                key_sha256 = ?6
             WHERE id = ?1",
            params![
                server.id,
                server.name,
                server.hostname,
                server.platform,
                server.version,
                key_sha256
            ],
        )?;
        Ok(())
    }

    pub fn server(&self, id: &str) -> rusqlite::Result<Option<ServerRow>> {
        self.lock()
            .query_row(
                &format!("{SERVER_SELECT} WHERE id = ?1"),
                params![id],
                server_row,
            )
            .optional()
    }

    pub fn server_by_key(&self, key_sha256: &str) -> rusqlite::Result<Option<ServerRow>> {
        self.lock()
            .query_row(
                &format!("{SERVER_SELECT} WHERE key_sha256 = ?1"),
                params![key_sha256],
                server_row,
            )
            .optional()
    }

    pub fn servers_of(&self, user_id: &str) -> rusqlite::Result<Vec<ServerRow>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "{SERVER_SELECT} WHERE user_id = ?1 ORDER BY created_at, id"
        ))?;
        let rows = stmt.query_map(params![user_id], server_row)?;
        rows.collect()
    }

    pub fn rename_server(&self, id: &str, name: &str) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE servers SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    pub fn delete_server(&self, id: &str) -> rusqlite::Result<bool> {
        let changed = self
            .lock()
            .execute("DELETE FROM servers WHERE id = ?1", params![id])?;
        Ok(changed == 1)
    }

    pub fn touch_server(&self, id: &str, now: i64) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE servers SET last_seen_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    /// What the relay client reports about its machine on connect.
    pub fn update_server_meta(
        &self,
        id: &str,
        hostname: &str,
        platform: &str,
        version: &str,
    ) -> rusqlite::Result<()> {
        self.lock().execute(
            "UPDATE servers SET hostname = ?2, platform = ?3, version = ?4 WHERE id = ?1",
            params![id, hostname, platform, version],
        )?;
        Ok(())
    }
}

const USER_SELECT: &str =
    "SELECT id, email, name, password_hash, email_verified, verification_code,
                                  verification_expires_at, tier_id, created_at FROM users";
const TIER_SELECT: &str = "SELECT id, name, servers, relay, price_eur_monthly FROM tiers";
const SERVER_SELECT: &str =
    "SELECT id, user_id, name, hostname, platform, version, created_at, last_seen_at FROM servers";

fn user_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UserRow> {
    Ok(UserRow {
        id: r.get(0)?,
        email: r.get(1)?,
        name: r.get(2)?,
        password_hash: r.get(3)?,
        email_verified: r.get(4)?,
        verification_code: r.get(5)?,
        verification_expires_at: r.get(6)?,
        tier_id: r.get(7)?,
        created_at: r.get(8)?,
    })
}

fn tier_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TierRow> {
    Ok(TierRow {
        id: r.get(0)?,
        name: r.get(1)?,
        servers: r.get(2)?,
        relay: r.get(3)?,
        price_eur_monthly: r.get(4)?,
    })
}

fn server_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ServerRow> {
    Ok(ServerRow {
        id: r.get(0)?,
        user_id: r.get(1)?,
        name: r.get(2)?,
        hostname: r.get(3)?,
        platform: r.get(4)?,
        version: r.get(5)?,
        created_at: r.get(6)?,
        last_seen_at: r.get(7)?,
    })
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let applied = usize::try_from(version).unwrap_or(0);
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        conn.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {}; COMMIT;",
            index + 1
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_and_seeds_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("hub.db")).unwrap();
        let tiers = db.tiers().unwrap();
        let ids: Vec<&str> = tiers.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["free", "pro", "team"]);
        assert_eq!(tiers[0].servers, Some(2));
        assert_eq!(tiers[2].servers, None);
        // Opening again is a no-op (user_version is at the latest migration).
        drop(db);
        let db = Db::open(&dir.path().join("hub.db")).unwrap();
        assert_eq!(db.tiers().unwrap().len(), 3);
    }
}
