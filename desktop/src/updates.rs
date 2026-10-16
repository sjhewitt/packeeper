//! New versions of the app. The app asks GitHub for the newest release (see
//! `plugins.updater` in tauri.conf.json), downloads it, and checks its
//! signature. The new version installs when the app closes, or at once when
//! the person clicks "Restart and update now". Settings → Updates turns the
//! automatic checks on and off; the app keeps that choice in its own settings
//! folder, not with the records.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use pac_keeper::server::Updates;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{WINDOW, flash};

/// The first check waits a little, so the app opens quickly.
const FIRST_CHECK: Duration = Duration::from_secs(10);
/// An app that stays open checks again after this time.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the app looks at the time of its last check. Wall-clock time,
/// so a computer that sleeps still checks once a day.
const LOOK_EVERY: Duration = Duration::from_secs(60 * 60);

/// The app's updates. Tauri keeps it: `app.state::<AppUpdates>()`.
pub struct AppUpdates {
    /// What the server's pages show.
    pub shared: Arc<Updates>,
    /// The file that keeps the automatic setting.
    setting: Option<PathBuf>,
    /// The new version that is downloaded and checked.
    downloaded: Mutex<Option<(Update, Vec<u8>)>>,
    /// A check or a download runs now.
    busy: AtomicBool,
}

impl AppUpdates {
    /// Reads the automatic setting. Automatic updates are on until the person
    /// turns them off.
    pub fn load(app: &AppHandle) -> AppUpdates {
        let setting = app.path().app_config_dir().ok().map(|dir| dir.join("updates.json"));
        let automatic = setting
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|v| v.get("automatic")?.as_bool())
            .unwrap_or(true);
        AppUpdates {
            shared: Arc::new(Updates::new(automatic)),
            setting,
            downloaded: Mutex::default(),
            busy: AtomicBool::new(false),
        }
    }

    fn save(&self, automatic: bool) -> anyhow::Result<()> {
        let path = self.setting.as_ref().context("the computer gives no settings folder")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::json!({ "automatic": automatic }).to_string())?;
        Ok(())
    }

    fn take_downloaded(&self) -> Option<(Update, Vec<u8>)> {
        self.downloaded.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// Checks soon after the start, then once a day, while automatic updates are on.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        let mut last: Option<SystemTime> = None;
        loop {
            let due = last.is_none_or(|t| t.elapsed().map_or(true, |since| since >= CHECK_EVERY));
            if due && app.state::<AppUpdates>().shared.get().automatic {
                check(&app, false).await;
                last = Some(SystemTime::now());
            }
            tokio::time::sleep(LOOK_EVERY).await;
        }
    });
}

/// Does what an Updates button in Settings asks: `on`, `off`, `check`, or `restart`.
pub fn act(app: &AppHandle, action: &str) {
    let app = app.clone();
    match action {
        "on" | "off" => {
            let on = action == "on";
            tauri::async_runtime::spawn(async move { set_automatic(&app, on).await });
        }
        "check" => {
            flash(&app, true, "PAC Keeper is looking for a new version.");
            tauri::async_runtime::spawn(async move { check(&app, true).await });
        }
        "restart" => {
            // The install takes a moment, and may ask for a password on a Mac.
            tauri::async_runtime::spawn_blocking(move || restart(&app));
        }
        _ => tracing::warn!("unknown update action {action}"),
    }
}

async fn set_automatic(app: &AppHandle, on: bool) {
    let me = app.state::<AppUpdates>();
    if let Err(e) = me.save(on) {
        tracing::error!("save the update setting: {e:#}");
        show_settings(app, false, &format!("PAC Keeper could not save the setting: {e:#}"));
        return;
    }
    me.shared.change(|u| u.automatic = on);
    if on {
        show_settings(app, true, "Automatic updates are on.");
        check(app, false).await;
    } else {
        show_settings(app, true, "Automatic updates are off.");
    }
}

enum Found {
    Newest,
    Ready(String),
    /// A development build finds new versions, but never installs them.
    NotForThisBuild(String),
}

/// Looks for a new version and downloads it. `asked`: the person clicked
/// Check now, so the page shows each step and the result.
async fn check(app: &AppHandle, asked: bool) {
    let me = app.state::<AppUpdates>();
    if me.busy.swap(true, Ordering::SeqCst) {
        if asked {
            flash(app, true, "PAC Keeper is looking for a new version already.");
        }
        return;
    }
    let found = find(app, asked).await;
    me.busy.store(false, Ordering::SeqCst);
    let (ok, message) = match found {
        Ok(Found::Newest) => (true, format!("PAC Keeper {} is the newest version.", app.package_info().version)),
        Ok(Found::Ready(version)) => {
            (true, format!("PAC Keeper {version} is ready. It installs when you close PAC Keeper."))
        }
        Ok(Found::NotForThisBuild(version)) => {
            (true, format!("PAC Keeper {version} is available. A development build does not install it."))
        }
        Err(e) => (false, format!("PAC Keeper could not get a new version: {e:#}")),
    };
    if ok {
        tracing::info!("update check: {message}");
    } else {
        tracing::warn!("update check: {message}");
    }
    if asked {
        tell(app, ok, &message);
    }
}

async fn find(app: &AppHandle, asked: bool) -> anyhow::Result<Found> {
    let me = app.state::<AppUpdates>();
    if let Some(version) = me.shared.get().ready {
        return Ok(Found::Ready(version));
    }
    let Some(update) = app.updater()?.check().await? else {
        return Ok(Found::Newest);
    };
    let version = update.version.clone();
    // A development build is not an installed app: an install would replace
    // the build folder.
    if cfg!(debug_assertions) {
        return Ok(Found::NotForThisBuild(version));
    }
    if asked {
        flash(app, true, &format!("PAC Keeper is downloading version {version}."));
    }
    let bytes = update.download(|_, _| {}, || {}).await?;
    *me.downloaded.lock().unwrap_or_else(|e| e.into_inner()) = Some((update, bytes));
    me.shared.change(|u| u.ready = Some(version.clone()));
    Ok(Found::Ready(version))
}

/// Installs the downloaded version and starts it. On Windows, the installer
/// closes PAC Keeper and starts it again when it is done.
fn restart(app: &AppHandle) {
    let me = app.state::<AppUpdates>();
    let Some((update, bytes)) = me.take_downloaded() else {
        tell(app, false, "No new version is ready. Click Check now first.");
        return;
    };
    match update.install(&bytes) {
        Ok(()) => app.request_restart(),
        Err(e) => {
            tracing::error!("install version {}: {e:#}", update.version);
            me.shared.change(|u| u.ready = None);
            tell(app, false, &format!("PAC Keeper could not install version {}: {e}", update.version));
        }
    }
}

/// Installs a downloaded version as the app closes, so the next start runs
/// it. On Windows, the installer runs after the app ends and does not start
/// it again.
pub fn install_on_exit(app: &AppHandle) {
    let Some((update, bytes)) = app.state::<AppUpdates>().take_downloaded() else {
        return;
    };
    tracing::info!("install version {}", update.version);
    if let Err(e) = update.restart_after_install(false).install(&bytes) {
        tracing::error!("install the new version: {e:#}");
    }
}

/// Shows the result of a check. On Settings, the page opens again to show it
/// with the new state; on any other page, as a message at the top.
fn tell(app: &AppHandle, ok: bool, message: &str) {
    let on_settings = app
        .get_webview_window(WINDOW)
        .and_then(|window| window.url().ok())
        .is_some_and(|url| url.path() == "/settings");
    if on_settings {
        show_settings(app, ok, message);
    } else {
        flash(app, ok, message);
    }
}

/// Opens Settings with a message, as the server's own pages do after a change.
fn show_settings(app: &AppHandle, ok: bool, message: &str) {
    let Some(window) = app.get_webview_window(WINDOW) else { return };
    let Ok(mut url) = window.url() else { return };
    url.set_path("/settings");
    url.set_fragment(None);
    url.query_pairs_mut().clear().append_pair(if ok { "msg" } else { "err" }, message);
    let _ = window.navigate(url);
}
