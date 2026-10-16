//! A PAC's records as one zip, and back again. The zip holds `pac.db`,
//! `receipts/`, and `imports/`: Settings → "Download all records".
//!
//! A restore replaces the PAC's records with the backup's. A hosted server
//! takes zips from people on the internet, so the backup must pass these
//! checks first:
//!
//! - The zip holds only `pac.db` and files under `receipts/` and `imports/`
//!   with plain names, and stays under the size limits.
//! - `pac.db` is a PAC Keeper database: it passes SQLite's integrity check,
//!   has no triggers or views, and comes from this version of PAC Keeper or
//!   an older one. After its migrations it has exactly the tables of this
//!   version, and its foreign keys hold.
//! - Every receipt and bank file it names has a plain name, so no record
//!   can point outside the PAC's folder.
//!
//! The records then go into the open database in one transaction, so the
//! running server keeps its connections. Sign-in users (`users`) stay as they
//! are: they belong to this install, not to the PAC. A copy of the records
//! from before is kept in `backups/before-restore.zip`.
//!
//! On a hosted server with PAC_DATA_KEY, the zip is still plain, so that a
//! PAC can take its records to the desktop app. A restore seals the files and
//! puts the records into the encrypted database.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Connection, SqlitePool};

use crate::crypt::Files;

/// The tables of a PAC's records, children first, for the foreign keys. A
/// new table goes here; a test checks that none is missing.
pub const TABLES: [&str; 23] = [
    "lunch_order_lines",
    "lunch_orders",
    "lunch_children",
    "lunch_day_items",
    "lunch_days",
    "lunch_periods",
    "lunch_items",
    "lunch_divisions",
    "allocations",
    "attachments",
    "rules",
    "budget_amounts",
    "fundraiser_goals",
    "meetings",
    "transactions",
    "import_batches",
    "gaming_events",
    "fundraisers",
    "line_items",
    "fiscal_years",
    "accounts",
    "settings",
    "users",
];

/// The PAC's tables that a restore replaces: all but `users`.
fn restored_tables() -> impl DoubleEndedIterator<Item = &'static str> {
    TABLES.into_iter().filter(|t| *t != "users")
}

/// The most that `pac.db` in a backup may unpack to.
const MAX_DB_BYTES: u64 = 512 * 1024 * 1024;
/// The most that the receipts and bank files may unpack to, without a quota.
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 200_000;

/// A reason to refuse a backup, in words for the person who sent it.
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

fn refuse(message: impl Into<String>) -> anyhow::Error {
    Refused(message.into()).into()
}

const NOT_OURS: &str = "This is not a PAC Keeper backup. Choose the .zip from \"Download all records\" in Settings.";

/// A zip of the records now: a consistent copy of the database, and the
/// receipts and imported bank files, all plain.
pub async fn snapshot(pool: &SqlitePool, data_dir: &Path, files: &Files) -> anyhow::Result<Vec<u8>> {
    let dir = data_dir.join("backups");
    tokio::fs::create_dir_all(&dir).await?;
    let copy = dir.join(format!("pac-{}.db", unique()));
    let copied = plain_copy(pool, &copy, files.is_sealed()).await;
    let bytes = match copied {
        Ok(()) => {
            let (data_dir, file, files) = (data_dir.to_path_buf(), copy.clone(), files.clone());
            tokio::task::spawn_blocking(move || build_zip(&data_dir, &file, &files)).await?
        }
        Err(e) => Err(e),
    };
    let _ = tokio::fs::remove_file(&copy).await;
    bytes
}

/// Writes a consistent, plain copy of the database to `to` while the app
/// keeps running.
async fn plain_copy(pool: &SqlitePool, to: &Path, encrypted: bool) -> anyhow::Result<()> {
    if !encrypted {
        sqlx::query("VACUUM INTO ?").bind(to.display().to_string()).execute(pool).await?;
        return Ok(());
    }
    // SQLCipher's VACUUM INTO would encrypt the copy, so export it instead.
    let mut conn = pool.acquire().await?;
    sqlx::query("ATTACH DATABASE ? AS plain KEY ''").bind(to.display().to_string()).execute(&mut *conn).await?;
    let result = sqlx::query("SELECT sqlcipher_export('plain')").execute(&mut *conn).await;
    if sqlx::query("DETACH DATABASE plain").execute(&mut *conn).await.is_err() {
        let _ = conn.detach().close().await;
    }
    result?;
    Ok(())
}

fn build_zip(data_dir: &Path, db: &Path, files: &Files) -> anyhow::Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("pac.db", opts)?;
        zip.write_all(&std::fs::read(db)?)?;
        for sub in ["receipts", "imports"] {
            for file in walk(&data_dir.join(sub)) {
                let rel = file.strip_prefix(data_dir)?.to_string_lossy().replace('\\', "/");
                zip.start_file(rel.as_str(), opts)?;
                zip.write_all(&files.open(std::fs::read(&file)?).with_context(|| format!("read {rel}"))?)?;
            }
        }
        zip.finish()?;
    }
    Ok(buf.into_inner())
}

pub(crate) fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// What a restore may keep, beyond the checks that always apply.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// The most bytes of receipts and bank files (a hosted PAC's quota).
    pub max_file_bytes: Option<u64>,
    /// On a hosted server: the parents of this PAC. Hot lunch children and
    /// orders of anyone else are left out, since their person numbers belong
    /// to another server.
    pub families: Option<HashSet<i64>>,
}

#[derive(Debug, Default)]
pub struct Restored {
    pub transactions: i64,
    pub receipts: usize,
    /// Hot lunch records of people who are not parents here were left out.
    pub left_out_lunch: bool,
}

/// The most bytes an upload of a backup may have under `options`.
pub fn upload_limit(options: &Options) -> u64 {
    MAX_DB_BYTES + options.max_file_bytes.unwrap_or(MAX_FILE_BYTES)
}

/// Replaces the records of the database `pool`, whose files live in
/// `data_dir`, with the backup at `zip`. An error that is a `Refused` says
/// what is wrong with the backup.
pub async fn restore(
    pool: &SqlitePool,
    data_dir: &Path,
    files: &Files,
    zip: &Path,
    options: &Options,
) -> anyhow::Result<Restored> {
    let staging = data_dir.join(format!(".restore-{}", unique()));
    let result = restore_from(pool, data_dir, files, zip, &staging, options).await;
    let _ = tokio::fs::remove_dir_all(&staging).await;
    result
}

async fn restore_from(
    pool: &SqlitePool,
    data_dir: &Path,
    files: &Files,
    zip: &Path,
    staging: &Path,
    options: &Options,
) -> anyhow::Result<Restored> {
    let (zip, unpack_to) = (zip.to_path_buf(), staging.to_path_buf());
    let limit = options.max_file_bytes.unwrap_or(MAX_FILE_BYTES);
    let receipts = tokio::task::spawn_blocking(move || unpack(&zip, &unpack_to, limit)).await??;
    let db = staging.join("pac.db");
    check_database(pool, &db).await?;
    let (sealer, unpacked) = (files.clone(), staging.to_path_buf());
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        for sub in ["receipts", "imports"] {
            sealer.seal_folder(&unpacked.join(sub))?;
        }
        Ok(())
    })
    .await??;

    // Keep the records from before, in case the wrong backup went in.
    let before = snapshot(pool, data_dir, files).await?;
    files.write(&data_dir.join("backups").join("before-restore.zip"), &before).await?;

    let left_out_lunch = copy_records(pool, &db, files.is_sealed(), options.families.as_ref()).await?;
    for sub in ["receipts", "imports"] {
        let (live, new, old) = (data_dir.join(sub), staging.join(sub), staging.join(format!("old-{sub}")));
        if live.exists() {
            tokio::fs::rename(&live, &old).await?;
        }
        if new.exists() {
            tokio::fs::rename(&new, &live).await?;
        }
    }
    let transactions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions").fetch_one(pool).await?;
    Ok(Restored { transactions, receipts, left_out_lunch })
}

/// Unpacks a backup into `staging` and returns the number of receipts.
fn unpack(zip: &Path, staging: &Path, file_limit: u64) -> anyhow::Result<usize> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?).map_err(|_| refuse(NOT_OURS))?;
    if archive.len() > MAX_ENTRIES {
        return Err(refuse(NOT_OURS));
    }
    std::fs::create_dir_all(staging)?;
    let (mut has_db, mut file_bytes, mut receipts) = (false, 0u64, 0usize);
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|_| refuse("The zip is damaged."))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(|_| refuse("The zip is damaged."))?.to_string();
        let (target, limit) = if name == "pac.db" {
            has_db = true;
            (staging.join("pac.db"), MAX_DB_BYTES)
        } else if let Some(rest) = name.strip_prefix("receipts/").filter(|r| plain_name(r, 2)) {
            receipts += 1;
            (staging.join("receipts").join(rest), file_limit - file_bytes)
        } else if let Some(rest) = name.strip_prefix("imports/").filter(|r| plain_name(r, 1)) {
            (staging.join("imports").join(rest), file_limit - file_bytes)
        } else {
            return Err(refuse(format!("{NOT_OURS} (It holds \"{}\".)", name.chars().take(80).collect::<String>())));
        };
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = std::fs::File::create(&target)?;
        let written =
            std::io::copy(&mut (&mut entry).take(limit + 1), &mut out).map_err(|_| refuse("The zip is damaged."))?;
        if written > limit {
            return Err(refuse(if name == "pac.db" {
                "The database in the backup is too large.".to_string()
            } else {
                format!(
                    "The receipts in the backup need more than the {} MB this PAC may store.",
                    file_limit / (1024 * 1024)
                )
            }));
        }
        if name != "pac.db" {
            file_bytes += written;
        }
    }
    if !has_db {
        return Err(refuse(NOT_OURS));
    }
    Ok(receipts)
}

/// A stored file name from a backup, such as "2026/12-ab12cd34-receipt.pdf":
/// at most `parts` parts, each of letters, digits, ".", "-", and "_", and
/// none of them "." or "..".
fn plain_name(name: &str, parts: usize) -> bool {
    let pieces: Vec<&str> = name.split('/').collect();
    pieces.len() <= parts
        && pieces.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.len() <= 120
                && p.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        })
}

/// Checks the backup's database, and brings it to this version's migrations.
async fn check_database(pool: &SqlitePool, db: &Path) -> anyhow::Result<()> {
    let options = SqliteConnectOptions::new().filename(db).foreign_keys(true);
    let backup =
        SqlitePoolOptions::new().max_connections(1).connect_with(options).await.map_err(|_| refuse(NOT_OURS))?;
    let result = check_open_database(pool, &backup).await;
    backup.close().await;
    result
}

async fn check_open_database(pool: &SqlitePool, backup: &SqlitePool) -> anyhow::Result<()> {
    let integrity: String =
        sqlx::query_scalar("PRAGMA integrity_check").fetch_one(backup).await.map_err(|_| refuse(NOT_OURS))?;
    if integrity != "ok" {
        return Err(refuse("The database in the backup is damaged."));
    }
    let (code, ours): (i64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(type IN ('trigger', 'view')), 0), COALESCE(SUM(type = 'table' AND name = '_sqlx_migrations'), 0)
         FROM sqlite_master",
    )
    .fetch_one(backup)
    .await?;
    if code > 0 || ours == 0 {
        return Err(refuse(NOT_OURS));
    }
    sqlx::migrate!("./migrations").run(backup).await.map_err(|e| match e {
        sqlx::migrate::MigrateError::VersionMissing(_) => {
            refuse("This backup comes from a newer PAC Keeper. Update PAC Keeper here first.")
        }
        _ => refuse(NOT_OURS),
    })?;
    if schema(backup).await? != schema(pool).await? {
        return Err(refuse(NOT_OURS));
    }
    let broken: Vec<(String,)> =
        sqlx::query_as("SELECT \"table\" FROM pragma_foreign_key_check").fetch_all(backup).await?;
    if !broken.is_empty() {
        return Err(refuse("The records in the backup do not fit together. Make the backup again."));
    }
    let names: Vec<(String, i64)> =
        sqlx::query_as("SELECT stored_name, 2 FROM attachments UNION ALL SELECT stored_name, 1 FROM import_batches")
            .fetch_all(backup)
            .await?;
    if names.iter().any(|(name, parts)| !plain_name(name, *parts as usize)) {
        return Err(refuse(NOT_OURS));
    }
    Ok(())
}

/// Every table, index, and other object, as SQLite stores them.
async fn schema(pool: &SqlitePool) -> anyhow::Result<Vec<(String, String, String, Option<String>)>> {
    Ok(sqlx::query_as(
        "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
    )
    .fetch_all(pool)
    .await?)
}

/// Copies the backup's records into the database, in one transaction.
async fn copy_records(
    pool: &SqlitePool,
    db: &Path,
    encrypted: bool,
    families: Option<&HashSet<i64>>,
) -> anyhow::Result<bool> {
    let mut conn = pool.acquire().await?;
    // The backup is plain. On an encrypted connection, SQLCipher takes an
    // attached database to have the same key unless KEY says otherwise.
    let attach = if encrypted { "ATTACH DATABASE ? AS backup KEY ''" } else { "ATTACH DATABASE ? AS backup" };
    sqlx::query(attach).bind(db.display().to_string()).execute(&mut *conn).await?;
    let result = copy_attached(&mut conn, families).await;
    if sqlx::query("DETACH DATABASE backup").execute(&mut *conn).await.is_err() {
        // Do not give a connection that still holds the backup back to the pool.
        let _ = conn.detach().close().await;
    }
    result
}

async fn copy_attached(conn: &mut sqlx::SqliteConnection, families: Option<&HashSet<i64>>) -> anyhow::Result<bool> {
    let mut tx = conn.begin().await?;
    sqlx::query("PRAGMA defer_foreign_keys = ON").execute(&mut *tx).await?;
    for table in restored_tables() {
        sqlx::query(AssertSqlSafe(format!("DELETE FROM main.{table}"))).execute(&mut *tx).await?;
    }
    for table in restored_tables().rev() {
        sqlx::query(AssertSqlSafe(format!("INSERT INTO main.{table} SELECT * FROM backup.{table}")))
            .execute(&mut *tx)
            .await?;
    }
    // A copy of the public demo is not a demo folder here.
    sqlx::query("DELETE FROM main.settings WHERE key = 'demo_data'").execute(&mut *tx).await?;
    let mut left_out = false;
    if let Some(families) = families {
        let people: Vec<i64> = sqlx::query_scalar(
            "SELECT person_id FROM main.lunch_children UNION SELECT person_id FROM main.lunch_orders",
        )
        .fetch_all(&mut *tx)
        .await?;
        for person in people.into_iter().filter(|p| !families.contains(p)) {
            left_out = true;
            for sql in [
                "DELETE FROM main.lunch_order_lines WHERE order_id IN (SELECT id FROM main.lunch_orders WHERE person_id = ?1)
                   OR child_id IN (SELECT id FROM main.lunch_children WHERE person_id = ?1)",
                "DELETE FROM main.lunch_orders WHERE person_id = ?1",
                "DELETE FROM main.lunch_children WHERE person_id = ?1",
            ] {
                sqlx::query(sql).bind(person).execute(&mut *tx).await?;
            }
        }
    }
    tx.commit().await?;
    Ok(left_out)
}

fn unique() -> String {
    let mut bytes = [0u8; 6];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);
    format!("{}-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"), hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn folder(name: &str) -> (SqlitePool, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pac-backup-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (crate::server::open_single(&dir).await.unwrap(), dir)
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buf);
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        buf.into_inner()
    }

    /// The backup's database, changed by `sql`, in a new zip with no receipts.
    async fn with_changed_db(backup: &[u8], dir: &Path, sql: &str) -> Vec<u8> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(backup.to_vec())).unwrap();
        let mut db = Vec::new();
        archive.by_name("pac.db").unwrap().read_to_end(&mut db).unwrap();
        let path = dir.join(format!("changed-{}.db", unique()));
        std::fs::write(&path, &db).unwrap();
        let pool = SqlitePool::connect_with(SqliteConnectOptions::new().filename(&path)).await.unwrap();
        sqlx::raw_sql(AssertSqlSafe(sql.to_string())).execute(&pool).await.unwrap();
        pool.close().await;
        zip_of(&[("pac.db", &std::fs::read(&path).unwrap())])
    }

    async fn try_restore(pool: &SqlitePool, dir: &Path, zip: &[u8], options: &Options) -> Result<Restored, String> {
        let path = dir.join(format!("upload-{}.zip", unique()));
        std::fs::write(&path, zip).unwrap();
        restore(pool, dir, &Files::plain(), &path, options).await.map_err(|e| match e.downcast_ref::<Refused>() {
            Some(r) => r.0.clone(),
            None => panic!("not a refusal: {e:#}"),
        })
    }

    #[tokio::test]
    async fn every_table_is_listed() {
        let (pool, dir) = folder("tables").await;
        let tables: HashSet<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations'",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
        .into_iter()
        .collect();
        let listed: HashSet<String> = TABLES.iter().map(|t| t.to_string()).collect();
        assert_eq!(tables, listed, "add the new table to backup::TABLES, children first");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_backup_restores_into_another_folder() {
        let (from, from_dir) = folder("from").await;
        crate::demo::load_shifted(&from, &from_dir, 0).await.unwrap();
        sqlx::query("INSERT INTO lunch_children (person_id, name, created_at) VALUES (77, 'Ava', '2026-01-01')")
            .execute(&from)
            .await
            .unwrap();
        let backup = snapshot(&from, &from_dir, &Files::plain()).await.unwrap();

        // The other folder has its own sign-in user, which stays.
        let (to, to_dir) = folder("to").await;
        crate::web::add_user(&to, "tess", "Tess", "a-long-password", "treasurer").await.unwrap();
        let r = try_restore(&to, &to_dir, &backup, &Options::default()).await.unwrap();

        let count = |pool: &SqlitePool, sql: &'static str| {
            let pool = pool.clone();
            async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(&pool).await.unwrap() }
        };
        assert_eq!(r.transactions, count(&from, "SELECT COUNT(*) FROM transactions").await);
        assert_eq!(r.receipts as i64, count(&from, "SELECT COUNT(*) FROM attachments").await);
        assert!(!r.left_out_lunch);
        let stored: Vec<String> =
            sqlx::query_scalar("SELECT stored_name FROM attachments").fetch_all(&to).await.unwrap();
        assert!(stored.iter().all(|s| to_dir.join("receipts").join(s).is_file()), "every receipt file is there");
        assert_eq!(count(&to, "SELECT COUNT(*) FROM users WHERE username = 'tess'").await, 1);
        assert_eq!(count(&to, "SELECT COUNT(*) FROM settings WHERE key = 'demo_data'").await, 0);
        assert_eq!(count(&to, "SELECT COUNT(*) FROM lunch_children").await, 1);
        assert!(to_dir.join("backups/before-restore.zip").is_file());
        assert_eq!(
            crate::db::Settings::load(&to).await.unwrap().pac_name,
            crate::db::Settings::load(&from).await.unwrap().pac_name
        );
        // Every report still balances on the restored records.
        let fy = crate::fiscal::ensure(&to, chrono::NaiveDate::from_ymd_opt(2025, 7, 1).unwrap()).await.unwrap();
        for f in crate::reports::fund_summaries(&to, &fy, fy.start_date, fy.end_date).await.unwrap() {
            assert!(f.balances());
        }

        // On a hosted server, hot lunch children of people from elsewhere are left out.
        let hosted = Options { families: Some(HashSet::from([5])), ..Options::default() };
        let r = try_restore(&to, &to_dir, &backup, &hosted).await.unwrap();
        assert!(r.left_out_lunch);
        assert_eq!(count(&to, "SELECT COUNT(*) FROM lunch_children").await, 0);
        let small = Options { max_file_bytes: Some(10), ..Options::default() };
        assert!(try_restore(&to, &to_dir, &backup, &small).await.unwrap_err().contains("MB this PAC may store"));

        // Backups that are not PAC Keeper's, or that point outside the folder, are refused.
        let refused = [
            b"not a zip at all".to_vec(),
            zip_of(&[("receipts/2026/a.pdf", b"x")]),
            zip_of(&[("pac.db", b"not a database")]),
            zip_of(&[("../outside.txt", b"x")]),
            zip_of(&[("receipts/../../control.db", b"x")]),
            with_changed_db(
                &backup,
                &to_dir,
                "CREATE TRIGGER t AFTER INSERT ON meetings BEGIN DELETE FROM accounts; END;",
            )
            .await,
            with_changed_db(&backup, &to_dir, "UPDATE attachments SET stored_name = '../../control.db'").await,
            with_changed_db(&backup, &to_dir, "CREATE TABLE extra (x)").await,
            with_changed_db(&backup, &to_dir, "DROP TABLE _sqlx_migrations").await,
        ];
        for zip in &refused {
            assert!(try_restore(&to, &to_dir, zip, &Options::default()).await.is_err());
        }
        let newer = with_changed_db(
            &backup,
            &to_dir,
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (9999, 'later', 1, x'00', 0)",
        )
        .await;
        assert!(try_restore(&to, &to_dir, &newer, &Options::default()).await.unwrap_err().contains("newer PAC Keeper"));
        // After the refusals the records are still the backup's.
        assert_eq!(count(&to, "SELECT COUNT(*) FROM transactions").await, r.transactions);
        assert!(
            !std::fs::read_dir(&to_dir)
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with(".restore"))
        );
        let _ = std::fs::remove_dir_all(from_dir);
        let _ = std::fs::remove_dir_all(to_dir);
    }
}
