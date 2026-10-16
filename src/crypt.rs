//! Encryption of a hosted server's records, with one key: PAC_DATA_KEY.
//!
//! The databases (`control.db` and each PAC's `pac.db`) use SQLCipher. The
//! receipts, the bank files, and the copy from before a restore are sealed
//! with AES-256-GCM. Every key comes from the data key, and each PAC has its
//! own keys, so the server stores no key on disk.
//!
//! The data key stays in the app's settings, outside the data folder. A copy
//! of the data folder or of a backup is then useless without it. If the data
//! key is lost, the records are lost. The running server holds the key, so
//! this does not keep out a person who can use the server's account.
//!
//! A hosted server that starts with a data key encrypts the records that are
//! still plain (`encrypt_existing`). A single-PAC folder and the desktop app
//! are not encrypted.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use anyhow::Context;
use hmac::Mac;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, SqliteConnection};

/// The first bytes of every plain SQLite database.
const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
/// The first bytes of a sealed file. The nonce and the sealed bytes follow.
const SEALED_HEADER: &[u8; 8] = b"PKSEAL1\0";
const NONCE_BYTES: usize = 12;

/// The folders of a PAC that hold files to seal.
const FILE_FOLDERS: [&str; 3] = ["receipts", "imports", "backups"];

/// The key in PAC_DATA_KEY. Every other key comes from it.
pub struct DataKey([u8; 32]);

impl DataKey {
    /// From 64 hex digits, as `pac-keeper new-data-key` writes them.
    pub fn parse(text: &str) -> anyhow::Result<DataKey> {
        let bytes = hex::decode(text.trim())
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .context("PAC_DATA_KEY must be 64 hex digits. Make one with: pac-keeper new-data-key")?;
        Ok(DataKey(bytes))
    }

    pub fn generate() -> DataKey {
        let mut bytes = [0u8; 32];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);
        DataKey(bytes)
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// The key of `control.db`.
    pub fn control(&self) -> DbKey {
        DbKey(self.derive("control.db"))
    }

    /// The keys of the PAC in `pacs/<folder>/`.
    pub fn folder(&self, folder: &str) -> FolderKey {
        FolderKey {
            db: DbKey(self.derive(&format!("pac.db {folder}"))),
            files: self.derive(&format!("files {folder}")),
        }
    }

    fn derive(&self, purpose: &str) -> [u8; 32] {
        let mut mac = <hmac::Hmac<sha2::Sha256> as hmac::KeyInit>::new_from_slice(&self.0).expect("any key length");
        mac.update(b"pac-keeper data key v1: ");
        mac.update(purpose.as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(&mac.finalize().into_bytes());
        out
    }
}

/// The key of one SQLCipher database.
#[derive(Clone)]
pub struct DbKey([u8; 32]);

impl DbKey {
    /// Opens the database with this key. sqlx sends SQLCipher's `PRAGMA key`
    /// before any other. It is a raw key, so no slow key derivation runs.
    pub fn apply(&self, options: SqliteConnectOptions) -> SqliteConnectOptions {
        options.pragma("key", format!("\"{}\"", self.raw()))
    }

    /// The key as text for `ATTACH DATABASE ... KEY ?`.
    fn raw(&self) -> String {
        format!("x'{}'", hex::encode(self.0))
    }
}

/// The keys of one PAC's folder: for its database and for its files.
pub struct FolderKey {
    pub db: DbKey,
    files: [u8; 32],
}

impl FolderKey {
    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.files))
    }

    fn seal(&self, plain: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_BYTES];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut nonce);
        let sealed = self
            .cipher()
            .encrypt(Nonce::from_slice(&nonce), plain)
            .map_err(|_| anyhow::anyhow!("could not seal the file"))?;
        Ok([SEALED_HEADER.as_slice(), &nonce, &sealed].concat())
    }

    fn open(&self, stored: &[u8]) -> anyhow::Result<Vec<u8>> {
        let rest = stored.strip_prefix(SEALED_HEADER.as_slice()).context("the file is not sealed")?;
        anyhow::ensure!(rest.len() > NONCE_BYTES, "the sealed file is damaged");
        let (nonce, sealed) = rest.split_at(NONCE_BYTES);
        self.cipher()
            .decrypt(Nonce::from_slice(nonce), sealed)
            .map_err(|_| anyhow::anyhow!("the sealed file is damaged, or the key is not the one that sealed it"))
    }

    /// Seals every file under `dir` that is not sealed yet. Returns how many.
    fn seal_folder(&self, dir: &Path) -> anyhow::Result<usize> {
        // A stored name has no "~", so these are copies that a crash left.
        let (partials, paths): (Vec<PathBuf>, Vec<PathBuf>) =
            crate::backup::walk(dir).into_iter().partition(|p| p.to_string_lossy().ends_with("~sealing"));
        for partial in partials {
            std::fs::remove_file(partial)?;
        }
        let mut sealed = 0;
        for path in paths {
            if is_sealed_file(&path)? {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let partial = path.with_file_name(format!("{name}~sealing"));
            std::fs::write(&partial, self.seal(&std::fs::read(&path)?)?)?;
            std::fs::rename(&partial, &path)?;
            sealed += 1;
        }
        Ok(sealed)
    }
}

/// How a PAC's folder keeps its receipts and bank files: plain, or sealed
/// with the PAC's key.
#[derive(Clone, Default)]
pub struct Files(Option<Arc<FolderKey>>);

impl Files {
    pub fn plain() -> Files {
        Files(None)
    }

    pub fn sealed(key: FolderKey) -> Files {
        Files(Some(Arc::new(key)))
    }

    /// True when the files are sealed, and so the database is encrypted.
    pub fn is_sealed(&self) -> bool {
        self.0.is_some()
    }

    pub async fn write(&self, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
        match &self.0 {
            None => tokio::fs::write(path, bytes).await?,
            Some(key) => tokio::fs::write(path, key.seal(bytes)?).await?,
        }
        Ok(())
    }

    pub async fn read(&self, path: &Path) -> anyhow::Result<Vec<u8>> {
        let stored = tokio::fs::read(path).await?;
        self.open(stored)
    }

    /// The plain bytes of a stored file.
    pub fn open(&self, stored: Vec<u8>) -> anyhow::Result<Vec<u8>> {
        match &self.0 {
            None => Ok(stored),
            Some(key) => key.open(&stored),
        }
    }

    /// Seals the plain files under `dir` in place. Does nothing when files stay plain.
    pub fn seal_folder(&self, dir: &Path) -> anyhow::Result<usize> {
        match &self.0 {
            None => Ok(0),
            Some(key) => key.seal_folder(dir),
        }
    }
}

/// True when this build has SQLCipher. Plain SQLite takes `PRAGMA key` and
/// ignores it, so with it the records would stay plain.
pub async fn supported() -> anyhow::Result<bool> {
    let mut conn = SqliteConnection::connect("sqlite::memory:").await?;
    let version: Option<String> = sqlx::query_scalar("PRAGMA cipher_version").fetch_optional(&mut conn).await?;
    conn.close().await?;
    Ok(version.is_some())
}

/// True when the file is a plain SQLite database. False when it is missing,
/// empty, or encrypted.
pub fn is_plain_database(path: &Path) -> anyhow::Result<bool> {
    Ok(starts_with(path, SQLITE_HEADER)?.unwrap_or(false))
}

/// True when the file holds a database that is not plain: an encrypted one.
pub fn is_encrypted_database(path: &Path) -> anyhow::Result<bool> {
    Ok(starts_with(path, SQLITE_HEADER)?.is_some_and(|plain| !plain))
}

fn is_sealed_file(path: &Path) -> anyhow::Result<bool> {
    Ok(starts_with(path, SEALED_HEADER)?.unwrap_or(false))
}

/// Whether the file starts with `header`. None when the file is missing or empty.
fn starts_with(path: &Path, header: &[u8]) -> anyhow::Result<Option<bool>> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let mut head = vec![0u8; header.len()];
    let mut got = 0;
    while got < head.len() {
        match file.read(&mut head[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok((got > 0).then(|| head[..got] == *header))
}

/// Encrypts whatever is still plain under a hosted server's folder:
/// `control.db`, and each PAC's database and files. The server runs this at
/// start, before it opens a database. It refuses a database that another
/// program has open.
pub async fn encrypt_existing(root: &Path, key: &DataKey) -> anyhow::Result<()> {
    anyhow::ensure!(supported().await?, "this PAC Keeper was built without encryption, so it cannot use PAC_DATA_KEY");
    if encrypt_database(&root.join("control.db"), &key.control()).await? {
        println!("Encrypted control.db");
    }
    let Ok(entries) = std::fs::read_dir(root.join("pacs")) else { return Ok(()) };
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(folder) = entry.file_name().to_str().map(str::to_string).filter(|_| dir.is_dir()) else { continue };
        let folder_key = key.folder(&folder);
        if encrypt_database(&dir.join("pac.db"), &folder_key.db).await? {
            println!("Encrypted pacs/{folder}/pac.db");
        }
        remove_restore_leftovers(&dir)?;
        let files = Files::sealed(folder_key);
        let sealed = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            FILE_FOLDERS.iter().map(|sub| files.seal_folder(&dir.join(sub))).sum()
        })
        .await??;
        if sealed > 0 {
            println!("Sealed {sealed} file(s) in pacs/{folder}");
        }
    }
    Ok(())
}

/// Deletes the plain upload and unpacked files of a restore that a crash
/// stopped (`backup::restore`). No restore runs while the server starts.
fn remove_restore_leftovers(dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".restore-") {
            std::fs::remove_dir_all(entry.path())?;
        } else if name.starts_with(".upload-") {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Encrypts a plain database in place. False when it is missing or encrypted
/// already. A crash before the end leaves the plain database as it was.
async fn encrypt_database(path: &Path, key: &DbKey) -> anyhow::Result<bool> {
    if !is_plain_database(path)? {
        return Ok(false);
    }
    let partial = path_with_suffix(path, "~encrypting");
    let _ = std::fs::remove_file(&partial);
    // ATTACH makes the new file only on a connection that may create files.
    let options = SqliteConnectOptions::new().filename(path).create_if_missing(true);
    let mut conn = SqliteConnection::connect_with(&options).await?;
    // Only the one connection to a database can take it out of WAL mode, so
    // this fails while a server has it open. It also ends the WAL file.
    let mode: Result<String, _> = sqlx::query_scalar("PRAGMA journal_mode = DELETE").fetch_one(&mut conn).await;
    if !mode.as_ref().is_ok_and(|m| m.eq_ignore_ascii_case("delete")) {
        anyhow::bail!("another program has {} open. Stop it, then start again.", path.display());
    }
    let objects: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master").fetch_one(&mut conn).await?;
    sqlx::query("ATTACH DATABASE ? AS encrypted KEY ?")
        .bind(partial.display().to_string())
        .bind(key.raw())
        .execute(&mut conn)
        .await?;
    sqlx::query("SELECT sqlcipher_export('encrypted')").execute(&mut conn).await?;
    sqlx::query("DETACH DATABASE encrypted").execute(&mut conn).await?;
    conn.close().await?;

    // Check the copy before it takes the place of the plain database.
    let mut conn = SqliteConnection::connect_with(&key.apply(SqliteConnectOptions::new().filename(&partial))).await?;
    let check: String = sqlx::query_scalar("PRAGMA quick_check").fetch_one(&mut conn).await?;
    let copied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master").fetch_one(&mut conn).await?;
    conn.close().await?;
    anyhow::ensure!(
        check == "ok" && copied == objects && !is_plain_database(&partial)?,
        "the encrypted copy of {} failed its check",
        path.display()
    );
    std::fs::rename(&partial, path)?;
    Ok(true)
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// A data key for tests, when this build can encrypt.
#[cfg(test)]
pub fn test_key() -> Option<DataKey> {
    cfg!(feature = "encryption").then(DataKey::generate)
}

#[cfg(all(test, feature = "encryption"))]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pac-crypt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_data_key_is_64_hex_digits() {
        let key = DataKey::generate();
        assert_eq!(DataKey::parse(&format!(" {}\n", key.to_hex())).unwrap().to_hex(), key.to_hex());
        assert!(DataKey::parse("abc").is_err());
        assert!(DataKey::parse(&"zz".repeat(32)).is_err());
        assert!(DataKey::parse(&"ab".repeat(33)).is_err());
    }

    #[test]
    fn each_pac_has_its_own_keys() {
        let key = DataKey::generate();
        let (a, b) = (key.folder("aaaa"), key.folder("bbbb"));
        assert_ne!(a.db.raw(), b.db.raw());
        assert_ne!(a.db.raw(), key.control().raw());
        assert_ne!(a.files, b.files);
        assert_eq!(a.files, key.folder("aaaa").files, "the same folder always gets the same key");
        assert_ne!(a.files, DataKey::generate().folder("aaaa").files);
    }

    #[test]
    fn a_sealed_file_opens_only_with_its_key() {
        let key = DataKey::generate();
        let files = Files::sealed(key.folder("aaaa"));
        let stored = files.0.as_ref().unwrap().seal(b"receipt for pizza").unwrap();
        assert!(!stored.windows(5).any(|w| w == b"pizza"));
        assert_eq!(files.open(stored.clone()).unwrap(), b"receipt for pizza");
        assert!(Files::sealed(key.folder("bbbb")).open(stored.clone()).is_err());
        assert!(files.open(b"receipt for pizza".to_vec()).is_err(), "a plain file is not taken as sealed");
        let mut damaged = stored.clone();
        *damaged.last_mut().unwrap() ^= 1;
        assert!(files.open(damaged).is_err());
        assert_eq!(Files::plain().open(stored.clone()).unwrap(), stored, "plain files stay as they are");
    }

    #[test]
    fn sealing_a_folder_seals_each_file_once() {
        let dir = temp("seal-folder");
        std::fs::create_dir_all(dir.join("2026")).unwrap();
        std::fs::write(dir.join("2026/1-ab-receipt.pdf"), b"%PDF receipt").unwrap();
        std::fs::write(dir.join("statement.csv"), b"date,amount").unwrap();
        std::fs::write(dir.join("statement.csv~sealing"), b"left by a crash").unwrap();
        let files = Files::sealed(DataKey::generate().folder("aaaa"));
        assert_eq!(files.seal_folder(&dir).unwrap(), 2);
        assert!(!dir.join("statement.csv~sealing").exists());
        assert_eq!(files.seal_folder(&dir).unwrap(), 0, "sealed files stay as they are");
        let stored = std::fs::read(dir.join("2026/1-ab-receipt.pdf")).unwrap();
        assert!(stored.starts_with(SEALED_HEADER));
        assert_eq!(files.open(stored).unwrap(), b"%PDF receipt");
        assert_eq!(Files::plain().seal_folder(&dir).unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A plain database becomes encrypted, keeps its records, and then opens
    /// only with its key.
    #[tokio::test]
    async fn a_plain_database_gets_encrypted() {
        let dir = temp("encrypt-db");
        let path = dir.join("pac.db");
        let pool = crate::db::open(&dir).await.unwrap();
        sqlx::query("INSERT INTO accounts (name, kind, opening_date) VALUES ('Chequing', 'general', '2026-07-01')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        assert!(is_plain_database(&path).unwrap());

        let key = DataKey::generate().folder("aaaa");
        assert!(encrypt_database(&path, &key.db).await.unwrap());
        assert!(is_encrypted_database(&path).unwrap() && !is_plain_database(&path).unwrap());
        assert!(!path_with_suffix(&path, "~encrypting").exists());
        assert!(!encrypt_database(&path, &key.db).await.unwrap(), "an encrypted database stays as it is");

        let pool = crate::db::open_with(&dir, Some(&key.db)).await.unwrap();
        let name: String = sqlx::query_scalar("SELECT name FROM accounts").fetch_one(&pool).await.unwrap();
        assert_eq!(name, "Chequing");
        pool.close().await;
        assert!(crate::db::open(&dir).await.is_err(), "no key");
        assert!(crate::db::open_with(&dir, Some(&DataKey::generate().folder("aaaa").db)).await.is_err(), "wrong key");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A database that a server has open stays plain, so the server does not
    /// lose what it writes next.
    #[tokio::test]
    async fn an_open_database_is_not_encrypted() {
        let dir = temp("encrypt-open");
        let pool = crate::db::open(&dir).await.unwrap();
        let key = DataKey::generate().folder("aaaa");
        let err = encrypt_database(&dir.join("pac.db"), &key.db).await.unwrap_err();
        assert!(err.to_string().contains("another program"), "{err}");
        assert!(is_plain_database(&dir.join("pac.db")).unwrap());
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts").fetch_one(&pool).await.unwrap();
        assert_eq!(n, 0, "the database still works");
        pool.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
