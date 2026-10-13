//! PAC Keeper as a desktop app for macOS and Windows. The app runs the same
//! server as `pac-keeper serve` inside itself, on this computer only, and
//! shows it in a native window. See `pac_keeper::server::start_desktop`.

// No console window on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pac_keeper::server::{self, DesktopFile, DesktopServer, FileUse};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

const WINDOW: &str = "main";

/// The address of the in-app button that shows the data folder.
const DATA_FOLDER_PATH: &str = "/__desktop/data-folder";

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pac_keeper=info,pac_keeper_desktop=info".into()),
        )
        .init();

    tauri::Builder::default()
        // A second launch shows the open window. Two copies must not share the records.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window(WINDOW) {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            clear_viewer_files();
            let opened = data_dir(&handle).and_then(|dir| {
                tauri::async_runtime::block_on(server::start_desktop(&dir))
                    .map_err(|e| anyhow::anyhow!("PAC Keeper could not open its records in {}: {e:#}", dir.display()))
            });
            match opened {
                Ok(server) => {
                    tracing::info!("records in {}", server.data_dir.display());
                    open_window(&handle, Arc::new(server))?
                }
                Err(e) => {
                    tracing::error!("{e:#}");
                    open_error_window(&handle, &format!("{e:#}"))?;
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("PAC Keeper could not start");
}

/// The records live in the computer's app data folder, not in Documents:
/// iCloud and OneDrive often sync Documents, and syncing can damage a
/// database that is open. PAC_DATA_DIR chooses another folder.
fn data_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    if let Some(dir) = std::env::var_os("PAC_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    Ok(app.path().local_data_dir()?.join("PAC Keeper"))
}

fn open_window(app: &AppHandle, server: Arc<DesktopServer>) -> tauri::Result<()> {
    let start: Url = server.start_url().parse().expect("start address");
    // After a file request answers with a page (an error, for example), the
    // window shows that page. This holds the one address to let through.
    let let_through: Arc<Mutex<Option<String>>> = Arc::default();

    let on_nav = {
        let (app, server, let_through) = (app.clone(), server.clone(), let_through.clone());
        move |url: &Url| route(&app, &server, &let_through, url)
    };
    let on_new = {
        let (app, server, let_through) = (app.clone(), server.clone(), let_through.clone());
        move |url: Url, _| {
            // Links that open a new tab: in-app addresses stay in this window.
            if route(&app, &server, &let_through, &url)
                && let Some(window) = app.get_webview_window(WINDOW)
            {
                let _ = window.navigate(url);
            }
            NewWindowResponse::Deny
        }
    };

    WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::External(start))
        .title("PAC Keeper")
        .inner_size(1280.0, 860.0)
        .min_inner_size(820.0, 560.0)
        // Lets the page take dropped receipts and bank files itself.
        .disable_drag_drop_handler()
        .on_navigation(on_nav)
        .on_new_window(on_new)
        .build()?;
    Ok(())
}

/// Decides where an address goes. `true` lets the window open it.
fn route(app: &AppHandle, server: &Arc<DesktopServer>, let_through: &Arc<Mutex<Option<String>>>, url: &Url) -> bool {
    let ours = url.scheme() == "http" && url.host_str() == Some("127.0.0.1") && url.port() == Some(server.addr.port());
    if !ours {
        if matches!(url.scheme(), "http" | "https" | "mailto")
            && let Err(e) = app.opener().open_url(url.as_str(), None::<&str>)
        {
            tracing::error!("open {url}: {e}");
        }
        return false;
    }
    let path = url.path();
    tracing::debug!("navigation to {path}");
    if let_through.lock().unwrap().take_if(|through| through == path).is_some() {
        return true;
    }
    if path == DATA_FOLDER_PATH {
        if let Err(e) = app.opener().open_path(server.data_dir.display().to_string(), None::<&str>) {
            tracing::error!("open the data folder: {e}");
        }
        return false;
    }
    if !server::is_file_path(path) {
        return true;
    }
    let path_and_query = match url.query() {
        Some(q) => format!("{path}?{q}"),
        None => path.to_string(),
    };
    let (app, server, let_through, url) = (app.clone(), server.clone(), let_through.clone(), url.clone());
    // The navigation callback cannot wait; making a PDF takes a moment.
    tauri::async_runtime::spawn(async move {
        match server.fetch(&path_and_query).await {
            Ok(Some(file)) => hand_over(&app, file),
            Ok(None) => {
                // The answer is a page, such as "No fiscal year": show it.
                *let_through.lock().unwrap() = Some(url.path().to_string());
                if let Some(window) = app.get_webview_window(WINDOW) {
                    let _ = window.navigate(url);
                }
            }
            Err(e) => flash(&app, false, &format!("PAC Keeper could not make this file: {e:#}")),
        }
    });
    false
}

/// Opens a PDF or picture in the computer's viewer, or saves any other file
/// in Downloads and shows it there.
fn hand_over(app: &AppHandle, file: DesktopFile) {
    let result = match file.open {
        FileUse::View => viewer_path(&file.filename).and_then(|path| {
            std::fs::write(&path, &file.bytes)?;
            app.opener().open_path(path.display().to_string(), None::<&str>)?;
            Ok(())
        }),
        FileUse::Save => save_path(app, &file.filename).and_then(|path| {
            std::fs::write(&path, &file.bytes)?;
            let _ = app.opener().reveal_item_in_dir(&path);
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let folder = path.parent().map(|p| p.display().to_string()).unwrap_or_default();
            flash(app, true, &format!("Saved {name} in {folder}."));
            Ok(())
        }),
    };
    if let Err(e) = result {
        flash(app, false, &format!("PAC Keeper could not open {}: {e:#}", file.filename));
    }
}

/// Files for the viewer go to a folder of their own, one subfolder per file,
/// so a file that is open in a viewer is never written over.
fn viewer_root() -> PathBuf {
    std::env::temp_dir().join("PAC Keeper")
}

fn viewer_path(filename: &str) -> anyhow::Result<PathBuf> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = viewer_root().join(N.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string());
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(filename))
}

fn clear_viewer_files() {
    let _ = std::fs::remove_dir_all(viewer_root());
}

/// A free name in Downloads: "backup.zip", then "backup (2).zip", and so on.
fn save_path(app: &AppHandle, filename: &str) -> anyhow::Result<PathBuf> {
    let dir = app.path().download_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(free_name(&dir, filename))
}

fn free_name(dir: &Path, filename: &str) -> PathBuf {
    let (stem, ext) = match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (filename, String::new()),
    };
    let mut path = dir.join(filename);
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    path
}

/// Shows a message at the top of the page, like the app's own messages.
fn flash(app: &AppHandle, ok: bool, message: &str) {
    let Some(window) = app.get_webview_window(WINDOW) else { return };
    let script = format!(
        "(function (m) {{ var main = document.querySelector('main'); if (!main) return; \
         var d = document.createElement('div'); d.className = 'flash {}'; d.textContent = m; \
         main.prepend(d); window.scrollTo(0, 0); }})({});",
        if ok { "ok" } else { "err" },
        serde_json::to_string(message).unwrap_or_default()
    );
    let _ = window.eval(script);
}

/// When the records cannot open, the window shows why, from the app's own page.
fn open_error_window(app: &AppHandle, message: &str) -> tauri::Result<()> {
    let script = format!("window.PAC_KEEPER_ERROR = {};", serde_json::to_string(message).unwrap_or_default());
    let window: WebviewWindow = WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("index.html".into()))
        .title("PAC Keeper")
        .inner_size(640.0, 420.0)
        .initialization_script(script)
        .on_navigation(|url| url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost"))
        .build()?;
    let _ = window.set_focus();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_names_in_a_folder() {
        let dir = std::env::temp_dir().join(format!("pac-desktop-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_name(&dir, "backup.zip"), dir.join("backup.zip"));
        std::fs::write(dir.join("backup.zip"), b"").unwrap();
        assert_eq!(free_name(&dir, "backup.zip"), dir.join("backup (2).zip"));
        std::fs::write(dir.join("backup (2).zip"), b"").unwrap();
        assert_eq!(free_name(&dir, "backup.zip"), dir.join("backup (3).zip"));
        std::fs::write(dir.join("notes"), b"").unwrap();
        assert_eq!(free_name(&dir, "notes"), dir.join("notes (2)"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
