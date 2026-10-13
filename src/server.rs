//! Starting a PAC Keeper server: a single-PAC folder, a hosted server, or the
//! server inside the desktop app.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use axum_extra::extract::cookie::Key;

use crate::{db, demo, fiscal, hosted, reports, web};

/// The database of a single-PAC folder. Refuses a hosted server's folder.
pub async fn open_single(dir: &Path) -> anyhow::Result<sqlx::SqlitePool> {
    if dir.join("control.db").exists() {
        anyhow::bail!("{} is a hosted server's folder; run it with serve --hosted", dir.display());
    }
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let pool = db::open(dir).await?;
    db::seed(&pool).await?;
    fiscal::current(&pool).await?;
    Ok(pool)
}

pub async fn open_hosted(dir: &Path, base_url: String, quota_mb: i64) -> anyhow::Result<hosted::Hosted> {
    if dir.join("pac.db").exists() {
        anyhow::bail!("{} holds one PAC's records. A hosted server needs its own, separate folder.", dir.display());
    }
    let mailer = hosted::mail::Mailer::from_env(dir.join("outbox"))?;
    hosted::Hosted::open(dir, base_url, mailer, quota_mb * 1024 * 1024).await
}

fn single_state(
    pool: sqlx::SqlitePool,
    data_dir: &Path,
    bind: SocketAddr,
    auth: bool,
) -> anyhow::Result<web::AppState> {
    Ok(web::AppState {
        single: Some(web::Single { pool, data_dir: data_dir.to_path_buf() }),
        hosted: None,
        renderer: Arc::new(reports::pdf::Renderer::new()),
        pdf_permits: Arc::new(tokio::sync::Semaphore::new(2)),
        auth,
        demo: None,
        loopback_only: bind.ip().is_loopback(),
        desktop_secret: None,
        key: load_key(data_dir)?,
    })
}

/// Runs a single-PAC server until Ctrl-C. `demo` is the number of minutes
/// between demo resets, when this is a public demo.
pub async fn serve(
    pool: sqlx::SqlitePool,
    data_dir: &Path,
    bind: SocketAddr,
    auth: bool,
    demo: Option<u32>,
) -> anyhow::Result<()> {
    if let Some(minutes) = demo {
        demo::reset(&pool, data_dir).await?;
        let (pool, dir) = (pool.clone(), data_dir.to_path_buf());
        tokio::spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(u64::from(minutes) * 60));
            every.tick().await; // The first tick is immediate; the data is fresh already.
            loop {
                every.tick().await;
                match demo::reset(&pool, &dir).await {
                    Ok(()) => tracing::info!("demo data reset"),
                    Err(e) => tracing::error!("demo reset failed: {e:#}"),
                }
            }
        });
    }
    let mut state = single_state(pool, data_dir, bind, auth || demo.is_some())?;
    state.demo = demo;
    let app = web::router(state);
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("listen on {bind}"))?;
    println!("PAC Keeper is running at http://{bind}");
    println!("Data folder: {}", data_dir.canonicalize().unwrap_or(data_dir.to_path_buf()).display());
    if let Some(minutes) = demo {
        println!(
            "Public demo: sign in as {} / {}. The data resets every {minutes} minutes.",
            demo::DEMO_USERNAME,
            demo::DEMO_PASSWORD
        );
    } else if !auth && !bind.ip().is_loopback() {
        println!(
            "Warning: sign-in is off but the app listens on {bind}. Anyone on the network can change the records. Use --auth."
        );
    }
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await?;
    Ok(())
}

/// Runs a hosted multi-PAC server until Ctrl-C.
pub async fn serve_hosted(dir: &Path, bind: SocketAddr, base_url: String, quota_mb: i64) -> anyhow::Result<()> {
    let hosted = open_hosted(dir, base_url, quota_mb).await?;
    let operators: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM people WHERE is_operator = 1").fetch_one(&hosted.control).await?;
    println!("PAC Keeper (hosted) is running at http://{bind}");
    println!("Public address: {}", hosted.base_url);
    println!("Data folder: {}", dir.canonicalize().unwrap_or(dir.to_path_buf()).display());
    println!("Email: {}", hosted.mailer.describe());
    if operators == 0 {
        println!("No operator yet. Add one with: pac-keeper --data-dir {} operator you@example.org", dir.display());
    }
    let state = web::AppState {
        single: None,
        hosted: Some(Arc::new(hosted)),
        renderer: Arc::new(reports::pdf::Renderer::new()),
        pdf_permits: Arc::new(tokio::sync::Semaphore::new(2)),
        auth: true,
        demo: None,
        loopback_only: false,
        desktop_secret: None,
        key: load_key(dir)?,
    };
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("listen on {bind}"))?;
    axum::serve(listener, web::router(state)).with_graceful_shutdown(shutdown_signal()).await?;
    Ok(())
}

/// The server inside the desktop app: listens on a free port on this computer
/// only, and answers only the app's own window.
pub struct DesktopServer {
    pub addr: SocketAddr,
    /// The same routes the server answers, to fetch files in-process.
    pub router: axum::Router,
    /// The app window proves itself with this secret; see `web::auth`.
    pub secret: String,
    pub data_dir: PathBuf,
}

impl DesktopServer {
    /// The first address the window opens. It hands the secret to the window.
    pub fn start_url(&self) -> String {
        format!("http://{}/__desktop?k={}", self.addr, self.secret)
    }

    /// Fetches a file the window asked for, without the network. `None` means
    /// the answer is a page (an error, for example), not a file.
    pub async fn fetch(&self, path_and_query: &str) -> anyhow::Result<Option<DesktopFile>> {
        use axum::http::{Request, header};
        use tower::ServiceExt;

        let req = Request::get(path_and_query)
            .header(header::HOST, self.addr.to_string())
            .header(header::COOKIE, format!("{}={}", web::DESKTOP_COOKIE, self.secret))
            .body(axum::body::Body::empty())?;
        let res = self.router.clone().oneshot(req).await?;
        let disposition = res.headers().get(header::CONTENT_DISPOSITION).and_then(|v| v.to_str().ok());
        let (true, Some(disposition)) = (res.status().is_success(), disposition) else {
            return Ok(None);
        };
        let content_type = res.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
        let name = disposition_filename(disposition);
        // Only PDFs and pictures open in a viewer. Any other file is saved,
        // so a receipt that is a program never runs by a click.
        let viewer_extension = match content_type {
            "application/pdf" => Some("pdf"),
            "image/png" => Some("png"),
            "image/jpeg" => Some("jpg"),
            "image/gif" => Some("gif"),
            "image/webp" => Some("webp"),
            "image/heic" => Some("heic"),
            _ => None,
        };
        let (filename, open) = match viewer_extension {
            Some(ext) if disposition.starts_with("inline") => {
                let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
                (format!("{stem}.{ext}"), FileUse::View)
            }
            _ => (name, FileUse::Save),
        };
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await?.to_vec();
        Ok(Some(DesktopFile { filename, bytes, open }))
    }
}

/// A file for the desktop app to hand to the computer.
pub struct DesktopFile {
    /// A safe name with no folder in it.
    pub filename: String,
    pub bytes: Vec<u8>,
    pub open: FileUse,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FileUse {
    /// Show it in the computer's viewer (PDFs and pictures).
    View,
    /// Save it in Downloads (backups and other files).
    Save,
}

/// The addresses in the app that answer with a file, not a page.
pub fn is_file_path(path: &str) -> bool {
    (path.starts_with("/reports/") && path.ends_with(".pdf"))
        || path == "/settings/backup.zip"
        || path.strip_prefix("/attachments/").is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
}

fn disposition_filename(disposition: &str) -> String {
    let raw = disposition.split_once("filename=\"").and_then(|(_, rest)| rest.split_once('"')).map_or("", |(n, _)| n);
    let clean: String =
        raw.chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let clean = clean.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if clean.is_empty() { "file".to_string() } else { clean.to_string() }
}

pub async fn start_desktop(data_dir: &Path) -> anyhow::Result<DesktopServer> {
    let pool = open_single(data_dir).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("listen on this computer")?;
    let addr = listener.local_addr()?;
    let mut bytes = [0u8; 24];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);
    let secret = hex::encode(bytes);
    let mut state = single_state(pool, data_dir, addr, false)?;
    state.desktop_secret = Some(secret.clone());
    let router = web::router(state);
    let serving = router.clone();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, serving).await {
            tracing::error!("desktop server stopped: {e}");
        }
    });
    Ok(DesktopServer { addr, router, secret, data_dir: data_dir.to_path_buf() })
}

/// Ctrl-C, or SIGTERM from a container runtime.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// The key that encrypts session cookies, made on first run.
pub fn load_key(data_dir: &Path) -> anyhow::Result<Key> {
    let path = data_dir.join("secret.key");
    if let Ok(bytes) = std::fs::read(&path)
        && bytes.len() >= 64
    {
        return Ok(Key::from(&bytes));
    }
    let key = Key::generate();
    std::fs::write(&path, key.master())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::*;

    async fn desktop() -> (DesktopServer, PathBuf) {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pac-desktop-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let pool = open_single(&dir).await.unwrap();
        demo::load_shifted(&pool, &dir, 0).await.unwrap();
        (start_desktop(&dir).await.unwrap(), dir)
    }

    async fn send(d: &DesktopServer, req: Request<Body>) -> axum::response::Response {
        d.router.clone().oneshot(req).await.unwrap()
    }

    fn get(d: &DesktopServer, path: &str) -> axum::http::request::Builder {
        Request::get(path).header(header::HOST, d.addr.to_string())
    }

    #[tokio::test]
    async fn only_the_app_window_opens_the_records() {
        let (d, dir) = desktop().await;
        let cookie = format!("{}={}", web::DESKTOP_COOKIE, d.secret);

        let res = send(&d, get(&d, "/").body(Body::empty()).unwrap()).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "no cookie");
        let res = send(&d, get(&d, "/").header(header::COOKIE, "pk_desktop=guess").body(Body::empty()).unwrap()).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "wrong cookie");
        let res = send(&d, get(&d, "/__desktop?k=guess").body(Body::empty()).unwrap()).await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "wrong secret");

        let start = d.start_url();
        let res =
            send(&d, get(&d, start.split_once(&d.addr.to_string()).unwrap().1).body(Body::empty()).unwrap()).await;
        assert!(res.status().is_redirection());
        let set = res.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(set.starts_with(&cookie) && set.contains("HttpOnly") && set.contains("SameSite=Strict"));

        let res = send(&d, get(&d, "/").header(header::COOKIE, &cookie).body(Body::empty()).unwrap()).await;
        assert_eq!(res.status(), StatusCode::OK);

        // A website that points its own name at 127.0.0.1 is refused.
        let res = send(
            &d,
            Request::get("/")
                .header(header::HOST, format!("evil.example:{}", d.addr.port()))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);

        // A post from another site is refused.
        let res = send(
            &d,
            Request::post("/settings")
                .header(header::HOST, d.addr.to_string())
                .header(header::ORIGIN, "https://evil.example")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("pac_name=Hacked"))
                .unwrap(),
        )
        .await;
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn files_open_or_save_by_kind() {
        let (d, dir) = desktop().await;
        let pdf = d.fetch("/reports/annual.pdf").await.unwrap().expect("a PDF");
        assert_eq!(pdf.open, FileUse::View);
        assert!(pdf.filename.ends_with(".pdf") && pdf.bytes.starts_with(b"%PDF"));

        let zip = d.fetch("/settings/backup.zip").await.unwrap().expect("a backup");
        assert_eq!(zip.open, FileUse::Save);
        assert!(zip.filename.starts_with("pac-keeper-backup-") && zip.filename.ends_with(".zip"));

        assert!(d.fetch("/attachments/999999").await.unwrap().is_none(), "a missing file is a page");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn file_paths_and_names() {
        assert!(is_file_path("/reports/monthly.pdf"));
        assert!(is_file_path("/attachments/12"));
        assert!(is_file_path("/settings/backup.zip"));
        assert!(!is_file_path("/attachments/12/delete"));
        assert!(!is_file_path("/attachments/"));
        assert!(!is_file_path("/reports"));

        assert_eq!(disposition_filename("inline; filename=\"receipt.pdf\""), "receipt.pdf");
        assert_eq!(disposition_filename("attachment; filename=\"../../evil.sh\""), "_.._evil.sh");
        assert_eq!(disposition_filename("attachment; filename=\"C:\\x.exe\""), "C__x.exe");
        assert_eq!(disposition_filename("attachment; filename=\"..\""), "file");
        assert_eq!(disposition_filename("attachment"), "file");
    }
}
