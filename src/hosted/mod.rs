//! Hosting many PACs on one server. A control database lists the PACs and the
//! people. Each PAC keeps its records in its own folder, laid out exactly like a
//! single-PAC data folder, so no query can see another PAC's records and a PAC
//! can leave with its folder.

pub mod mail;
pub mod stats;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::Context;
use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};
use tokio::sync::Mutex;

use crate::db::{self, Settings};

#[derive(Debug, Clone, FromRow)]
pub struct Person {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub password_hash: Option<String>,
    pub email_verified_at: Option<String>,
    pub is_operator: bool,
    pub session_version: i64,
    #[allow(dead_code)]
    pub created_at: String,
}

impl Person {
    pub fn is_verified(&self) -> bool {
        self.email_verified_at.is_some()
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Pac {
    pub id: i64,
    pub folder: String,
    pub name: String,
    pub school: String,
    pub district: String,
    pub status: String,
    pub requested_by: Option<i64>,
    pub message: String,
    pub created_at: String,
    pub decided_at: Option<String>,
}

impl Pac {
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

/// The Stripe account that takes a PAC's hot lunch payments.
#[derive(Debug, Clone, FromRow)]
pub struct PacStripe {
    pub pac_id: i64,
    pub account_id: String,
    /// "standard" (the PAC's own account) or "express" (under the platform).
    pub kind: String,
    pub charges_enabled: bool,
    pub updated_at: String,
}

impl PacStripe {
    pub fn destination(&self) -> crate::lunch::stripe::Destination {
        let account = self.account_id.clone();
        if self.kind == "standard" {
            crate::lunch::stripe::Destination::Standard { account }
        } else {
            crate::lunch::stripe::Destination::Express { account }
        }
    }

    pub fn is_express(&self) -> bool {
        self.kind == "express"
    }
}

/// A PAC with the person's role in it.
#[derive(Debug, Clone, FromRow)]
pub struct Membership {
    #[sqlx(flatten)]
    pub pac: Pac,
    pub role: String,
}

/// A row from the tokens table, after its link was followed.
#[derive(Debug, Clone, FromRow)]
pub struct Token {
    pub person_id: i64,
    pub pac_id: Option<i64>,
    pub role: Option<String>,
}

pub struct Hosted {
    pub control: SqlitePool,
    pub root: PathBuf,
    /// The public address, such as https://app.packeeper.ca. Used in emails.
    pub base_url: String,
    pub mailer: mail::Mailer,
    /// Most bytes of receipts one PAC may store.
    pub quota_bytes: i64,
    /// Where the operator's visit counts come from; see `stats`.
    pub stats: stats::Sources,
    pools: Mutex<HashMap<i64, SqlitePool>>,
    limiter: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Hosted {
    pub async fn open(root: &Path, base_url: String, mailer: mail::Mailer, quota_bytes: i64) -> anyhow::Result<Hosted> {
        std::fs::create_dir_all(root.join("pacs"))?;
        let path = root.join("control.db");
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(10));
        let control = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .with_context(|| format!("open {}", path.display()))?;
        sqlx::migrate!("./migrations_control").run(&control).await.context("migrate the control database")?;
        Ok(Hosted {
            control,
            root: root.to_path_buf(),
            base_url: base_url.trim_end_matches('/').to_string(),
            mailer,
            quota_bytes,
            stats: stats::Sources::default(),
            pools: Mutex::new(HashMap::new()),
            limiter: Mutex::new(HashMap::new()),
        })
    }

    pub fn secure_cookies(&self) -> bool {
        self.base_url.starts_with("https://")
    }

    pub fn pac_dir(&self, pac: &Pac) -> PathBuf {
        self.root.join("pacs").join(&pac.folder)
    }

    /// The PAC's own database. Opened on first use and kept open.
    pub async fn pac_pool(&self, pac: &Pac) -> anyhow::Result<SqlitePool> {
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get(&pac.id) {
            return Ok(pool.clone());
        }
        let dir = self.pac_dir(pac);
        std::fs::create_dir_all(&dir)?;
        let pool = db::open(&dir).await?;
        db::seed(&pool).await?;
        crate::fiscal::current(&pool).await?;
        pools.insert(pac.id, pool.clone());
        Ok(pool)
    }

    pub async fn person(&self, id: i64) -> anyhow::Result<Option<Person>> {
        Ok(sqlx::query_as("SELECT * FROM people WHERE id = ?").bind(id).fetch_optional(&self.control).await?)
    }

    pub async fn person_by_email(&self, email: &str) -> anyhow::Result<Option<Person>> {
        Ok(sqlx::query_as("SELECT * FROM people WHERE email = ?")
            .bind(email.trim())
            .fetch_optional(&self.control)
            .await?)
    }

    pub async fn pac(&self, id: i64) -> anyhow::Result<Option<Pac>> {
        Ok(sqlx::query_as("SELECT * FROM pacs WHERE id = ?").bind(id).fetch_optional(&self.control).await?)
    }

    /// Every PAC the person belongs to, active ones first.
    pub async fn memberships(&self, person_id: i64) -> anyhow::Result<Vec<Membership>> {
        Ok(sqlx::query_as(
            "SELECT p.*, m.role FROM memberships m JOIN pacs p ON p.id = m.pac_id
             WHERE m.person_id = ? ORDER BY p.status = 'active' DESC, p.name",
        )
        .bind(person_id)
        .fetch_all(&self.control)
        .await?)
    }

    pub async fn membership(&self, person_id: i64, pac_id: i64) -> anyhow::Result<Option<Membership>> {
        Ok(sqlx::query_as(
            "SELECT p.*, m.role FROM memberships m JOIN pacs p ON p.id = m.pac_id WHERE m.person_id = ? AND m.pac_id = ?",
        )
        .bind(person_id)
        .bind(pac_id)
        .fetch_optional(&self.control)
        .await?)
    }

    /// Adds a person, or returns the existing one with that email.
    pub async fn ensure_person(&self, email: &str, name: &str) -> anyhow::Result<Person> {
        if let Some(p) = self.person_by_email(email).await? {
            return Ok(p);
        }
        let id = sqlx::query("INSERT INTO people (email, name, created_at) VALUES (?, ?, ?)")
            .bind(email.trim())
            .bind(name.trim())
            .bind(db::now())
            .execute(&self.control)
            .await?
            .last_insert_rowid();
        Ok(self.person(id).await?.expect("just inserted"))
    }

    pub async fn set_password(&self, person_id: i64, password: &str) -> anyhow::Result<()> {
        let hash = crate::web::hash_password(password)?;
        // A new password signs the person out of every other session.
        sqlx::query("UPDATE people SET password_hash = ?, session_version = session_version + 1 WHERE id = ?")
            .bind(hash)
            .bind(person_id)
            .execute(&self.control)
            .await?;
        Ok(())
    }

    pub async fn mark_verified(&self, person_id: i64) -> anyhow::Result<()> {
        sqlx::query("UPDATE people SET email_verified_at = COALESCE(email_verified_at, ?) WHERE id = ?")
            .bind(db::now())
            .bind(person_id)
            .execute(&self.control)
            .await?;
        Ok(())
    }

    /// Makes a one-time link token. Returns the token; only its hash is stored.
    pub async fn create_token(
        &self,
        purpose: &str,
        person_id: i64,
        pac_id: Option<i64>,
        role: Option<&str>,
        valid_for: chrono::Duration,
    ) -> anyhow::Result<String> {
        let mut bytes = [0u8; 24];
        rand::rng().fill_bytes(&mut bytes);
        let token = hex::encode(bytes);
        sqlx::query(
            "INSERT INTO tokens (token_hash, purpose, person_id, pac_id, role, expires_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(hash_token(&token))
        .bind(purpose)
        .bind(person_id)
        .bind(pac_id)
        .bind(role)
        .bind((chrono::Utc::now() + valid_for).to_rfc3339())
        .execute(&self.control)
        .await?;
        Ok(token)
    }

    /// The token's row if it is unused and not expired. Does not use it up.
    pub async fn peek_token(&self, token: &str, purpose: &str) -> anyhow::Result<Option<Token>> {
        Ok(sqlx::query_as(
            "SELECT person_id, pac_id, role FROM tokens
             WHERE token_hash = ? AND purpose = ? AND used_at IS NULL AND expires_at > ?",
        )
        .bind(hash_token(token))
        .bind(purpose)
        .bind(chrono::Utc::now().to_rfc3339())
        .fetch_optional(&self.control)
        .await?)
    }

    /// Uses up a token. Returns its row, or None if it was not valid.
    pub async fn take_token(&self, token: &str, purpose: &str) -> anyhow::Result<Option<Token>> {
        let row = self.peek_token(token, purpose).await?;
        if row.is_some() {
            sqlx::query("UPDATE tokens SET used_at = ? WHERE token_hash = ?")
                .bind(db::now())
                .bind(hash_token(token))
                .execute(&self.control)
                .await?;
        }
        Ok(row)
    }

    /// Records a PAC's request to join. The requester becomes its first treasurer.
    pub async fn request_pac(
        &self,
        person_id: i64,
        name: &str,
        school: &str,
        district: &str,
        message: &str,
    ) -> anyhow::Result<i64> {
        let mut folder_bytes = [0u8; 8];
        rand::rng().fill_bytes(&mut folder_bytes);
        let mut tx = self.control.begin().await?;
        let id = sqlx::query(
            "INSERT INTO pacs (folder, name, school, district, status, requested_by, message, created_at)
             VALUES (?, ?, ?, ?, 'requested', ?, ?, ?)",
        )
        .bind(hex::encode(folder_bytes))
        .bind(name.trim())
        .bind(school.trim())
        .bind(district.trim())
        .bind(person_id)
        .bind(message.trim())
        .bind(db::now())
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
        sqlx::query("INSERT INTO memberships (pac_id, person_id, role, created_at) VALUES (?, ?, 'treasurer', ?)")
            .bind(id)
            .bind(person_id)
            .bind(db::now())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Approves a PAC: creates its database with its name, and opens it for use.
    pub async fn activate(&self, pac_id: i64) -> anyhow::Result<Pac> {
        let pac = self.pac(pac_id).await?.context("no such PAC")?;
        let pool = self.pac_pool(&pac).await?;
        let mut settings = Settings::load(&pool).await?;
        if settings.pac_name == Settings::default().pac_name {
            settings.pac_name = pac.name.clone();
            settings.school_name = pac.school.clone();
            settings.save(&pool).await?;
        }
        self.set_status(pac_id, "active").await?;
        Ok(self.pac(pac_id).await?.expect("exists"))
    }

    pub async fn set_status(&self, pac_id: i64, status: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE pacs SET status = ?, decided_at = ? WHERE id = ?")
            .bind(status)
            .bind(db::now())
            .bind(pac_id)
            .execute(&self.control)
            .await?;
        Ok(())
    }

    /// Keeps the control database's PAC name in step with the PAC's own settings.
    pub async fn rename(&self, pac_id: i64, name: &str, school: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE pacs SET name = ?, school = ? WHERE id = ?")
            .bind(name)
            .bind(school)
            .bind(pac_id)
            .execute(&self.control)
            .await?;
        Ok(())
    }

    /// True if `key` has been used fewer than `max` times in the current `window`.
    pub async fn allow(&self, key: &str, max: u32, window: Duration) -> bool {
        let mut map = self.limiter.lock().await;
        let now = Instant::now();
        if map.len() > 50_000 {
            map.retain(|_, (start, _)| now.duration_since(*start) < window);
        }
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= max
    }

    // ----- Hot lunch families and payments -----

    /// The active PAC whose hot lunch the person orders from, if they are a parent there.
    pub async fn family_pac(&self, person_id: i64, pac_id: i64) -> anyhow::Result<Option<Pac>> {
        Ok(sqlx::query_as(
            "SELECT p.* FROM families f JOIN pacs p ON p.id = f.pac_id WHERE f.person_id = ? AND f.pac_id = ?",
        )
        .bind(person_id)
        .bind(pac_id)
        .fetch_optional(&self.control)
        .await?)
    }

    /// The PACs where the person orders hot lunch.
    pub async fn family_pacs(&self, person_id: i64) -> anyhow::Result<Vec<Pac>> {
        Ok(sqlx::query_as(
            "SELECT p.* FROM families f JOIN pacs p ON p.id = f.pac_id WHERE f.person_id = ? ORDER BY p.name",
        )
        .bind(person_id)
        .fetch_all(&self.control)
        .await?)
    }

    pub async fn add_family(&self, person_id: i64, pac_id: i64) -> anyhow::Result<()> {
        sqlx::query("INSERT OR IGNORE INTO families (pac_id, person_id, created_at) VALUES (?, ?, ?)")
            .bind(pac_id)
            .bind(person_id)
            .bind(crate::db::now())
            .execute(&self.control)
            .await?;
        Ok(())
    }

    pub async fn remove_family(&self, person_id: i64, pac_id: i64) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM families WHERE pac_id = ? AND person_id = ?")
            .bind(pac_id)
            .bind(person_id)
            .execute(&self.control)
            .await?;
        Ok(())
    }

    /// The parents of a PAC, by name.
    pub async fn families(&self, pac_id: i64) -> anyhow::Result<Vec<Person>> {
        Ok(sqlx::query_as(
            "SELECT p.* FROM families f JOIN people p ON p.id = f.person_id WHERE f.pac_id = ? ORDER BY p.name, p.email",
        )
        .bind(pac_id)
        .fetch_all(&self.control)
        .await?)
    }

    /// People by id, for lists of orders.
    pub async fn people(&self, ids: &[i64]) -> anyhow::Result<HashMap<i64, Person>> {
        let mut out = HashMap::new();
        for id in ids {
            if !out.contains_key(id)
                && let Some(p) = self.person(*id).await?
            {
                out.insert(*id, p);
            }
        }
        Ok(out)
    }

    /// The PAC's sign-up code for families, made on first use.
    pub async fn family_code(&self, pac_id: i64) -> anyhow::Result<String> {
        let code: Option<String> = sqlx::query_scalar("SELECT code FROM family_codes WHERE pac_id = ?")
            .bind(pac_id)
            .fetch_optional(&self.control)
            .await?;
        match code {
            Some(code) => Ok(code),
            None => self.new_family_code(pac_id).await,
        }
    }

    /// Replaces the PAC's sign-up code. The old link stops working.
    pub async fn new_family_code(&self, pac_id: i64) -> anyhow::Result<String> {
        let mut bytes = [0u8; 9];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);
        let code = hex::encode(bytes);
        sqlx::query("INSERT OR REPLACE INTO family_codes (code, pac_id, created_at) VALUES (?, ?, ?)")
            .bind(&code)
            .bind(pac_id)
            .bind(crate::db::now())
            .execute(&self.control)
            .await?;
        Ok(code)
    }

    pub async fn pac_by_family_code(&self, code: &str) -> anyhow::Result<Option<Pac>> {
        Ok(sqlx::query_as("SELECT p.* FROM family_codes c JOIN pacs p ON p.id = c.pac_id WHERE c.code = ?")
            .bind(code)
            .fetch_optional(&self.control)
            .await?)
    }

    pub async fn pac_stripe(&self, pac_id: i64) -> anyhow::Result<Option<PacStripe>> {
        Ok(sqlx::query_as("SELECT * FROM pac_stripe WHERE pac_id = ?")
            .bind(pac_id)
            .fetch_optional(&self.control)
            .await?)
    }

    pub async fn set_pac_stripe(
        &self,
        pac_id: i64,
        account_id: &str,
        kind: &str,
        charges_enabled: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO pac_stripe (pac_id, account_id, kind, charges_enabled, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(pac_id)
        .bind(account_id)
        .bind(kind)
        .bind(charges_enabled)
        .bind(crate::db::now())
        .execute(&self.control)
        .await?;
        Ok(())
    }

    /// The PAC's sealed Helcim API token; see `lunch::helcim`.
    pub async fn pac_helcim(&self, pac_id: i64) -> anyhow::Result<Option<String>> {
        Ok(sqlx::query_scalar("SELECT token_sealed FROM pac_helcim WHERE pac_id = ?")
            .bind(pac_id)
            .fetch_optional(&self.control)
            .await?)
    }

    pub async fn set_pac_helcim(&self, pac_id: i64, token_sealed: &str) -> anyhow::Result<()> {
        sqlx::query("INSERT OR REPLACE INTO pac_helcim (pac_id, token_sealed, updated_at) VALUES (?, ?, ?)")
            .bind(pac_id)
            .bind(token_sealed)
            .bind(db::now())
            .execute(&self.control)
            .await?;
        Ok(())
    }

    pub async fn remove_pac_helcim(&self, pac_id: i64) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM pac_helcim WHERE pac_id = ?").bind(pac_id).execute(&self.control).await?;
        Ok(())
    }

    /// The active PACs that take cards through Helcim.
    pub async fn helcim_pacs(&self) -> anyhow::Result<Vec<Pac>> {
        Ok(sqlx::query_as(
            "SELECT pacs.* FROM pacs JOIN pac_helcim ON pac_helcim.pac_id = pacs.id WHERE pacs.status = 'active'",
        )
        .fetch_all(&self.control)
        .await?)
    }

    pub async fn remove_pac_stripe(&self, pac_id: i64) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM pac_stripe WHERE pac_id = ?").bind(pac_id).execute(&self.control).await?;
        Ok(())
    }

    /// Bytes of receipts the PAC stores.
    pub async fn storage_used(pool: &SqlitePool) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar("SELECT COALESCE(SUM(size_bytes), 0) FROM attachments").fetch_one(pool).await?)
    }
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// True for a plausible email address. Delivery is the real test.
pub fn looks_like_email(s: &str) -> bool {
    let s = s.trim();
    let Some((local, domain)) = s.split_once('@') else { return false };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !s.contains(char::is_whitespace)
}

/// A consistent copy of every database, receipt, and bank file under `root`,
/// into `to/<timestamp>/`. Works on a hosted root or a single-PAC folder.
pub async fn backup(root: &Path, to: &Path) -> anyhow::Result<PathBuf> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let dest = to.join(&stamp);
    std::fs::create_dir_all(&dest)?;
    let mut databases = Vec::new();
    for name in ["control.db", "pac.db"] {
        if root.join(name).exists() {
            databases.push(PathBuf::from(name));
        }
    }
    if let Ok(entries) = std::fs::read_dir(root.join("pacs")) {
        for e in entries.flatten() {
            if e.path().join("pac.db").exists() {
                databases.push(Path::new("pacs").join(e.file_name()).join("pac.db"));
            }
        }
    }
    for rel in &databases {
        let target = dest.join(rel);
        std::fs::create_dir_all(target.parent().expect("parent"))?;
        let options =
            SqliteConnectOptions::from_str(&format!("sqlite://{}", root.join(rel).display()))?.read_only(true);
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(options).await?;
        // VACUUM INTO makes a consistent copy while the server keeps running.
        sqlx::query("VACUUM INTO ?").bind(target.display().to_string()).execute(&pool).await?;
        pool.close().await;
        // Receipts and bank files sit beside each database.
        let folder = rel.parent().unwrap_or(Path::new(""));
        for sub in ["receipts", "imports"] {
            let src = root.join(folder).join(sub);
            if src.exists() {
                copy_dir(&src, &dest.join(folder).join(sub))?;
            }
        }
    }
    Ok(dest)
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn hosted() -> (Hosted, PathBuf) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pac-hosted-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let mailer = mail::Mailer::Outbox(dir.join("outbox"));
        (Hosted::open(&dir, "http://localhost".into(), mailer, 1 << 30).await.unwrap(), dir)
    }

    #[tokio::test]
    async fn each_pac_has_its_own_database() {
        let (h, dir) = hosted().await;
        let alice = h.ensure_person("alice@example.org", "Alice").await.unwrap();
        let bob = h.ensure_person("bob@example.org", "Bob").await.unwrap();
        let a = h.request_pac(alice.id, "Alpha PAC", "Alpha School", "", "").await.unwrap();
        let b = h.request_pac(bob.id, "Beta PAC", "Beta School", "", "").await.unwrap();
        let a = h.activate(a).await.unwrap();
        let b = h.activate(b).await.unwrap();
        assert_ne!(h.pac_dir(&a), h.pac_dir(&b));

        let pool_a = h.pac_pool(&a).await.unwrap();
        let pool_b = h.pac_pool(&b).await.unwrap();
        sqlx::query(
            "INSERT INTO accounts (name, kind, opening_date) VALUES ('Alpha chequing', 'general', '2026-07-01')",
        )
        .execute(&pool_a)
        .await
        .unwrap();
        let in_b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts").fetch_one(&pool_b).await.unwrap();
        assert_eq!(in_b, 0);
        assert_eq!(Settings::load(&pool_a).await.unwrap().pac_name, "Alpha PAC");
        assert_eq!(Settings::load(&pool_b).await.unwrap().pac_name, "Beta PAC");

        // Membership is per PAC.
        assert!(h.membership(alice.id, a.id).await.unwrap().is_some());
        assert!(h.membership(alice.id, b.id).await.unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn tokens_are_one_time_and_expire() {
        let (h, dir) = hosted().await;
        let p = h.ensure_person("carol@example.org", "Carol").await.unwrap();
        let t = h.create_token("verify", p.id, None, None, chrono::Duration::hours(1)).await.unwrap();
        assert!(h.take_token(&t, "reset").await.unwrap().is_none(), "wrong purpose");
        assert!(h.take_token(&t, "verify").await.unwrap().is_some());
        assert!(h.take_token(&t, "verify").await.unwrap().is_none(), "used twice");
        let old = h.create_token("verify", p.id, None, None, chrono::Duration::hours(-1)).await.unwrap();
        assert!(h.take_token(&old, "verify").await.unwrap().is_none(), "expired");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn limiter_blocks_after_max() {
        let (h, dir) = hosted().await;
        for _ in 0..3 {
            assert!(h.allow("k", 3, Duration::from_secs(60)).await);
        }
        assert!(!h.allow("k", 3, Duration::from_secs(60)).await);
        assert!(h.allow("other", 3, Duration::from_secs(60)).await);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn email_shape() {
        assert!(looks_like_email("a@b.ca"));
        assert!(!looks_like_email("a@b"));
        assert!(!looks_like_email("a b@c.ca"));
        assert!(!looks_like_email("@c.ca"));
    }
}
