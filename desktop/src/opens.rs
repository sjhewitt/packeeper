//! Open counts. When the app opens, it tells PAC Keeper's server, so the
//! operator can see how many people use the app (`/operator/stats`). It sends
//! the version, the kind of computer, and whether this is the first open today
//! (UTC) or the first open on this computer. It sends nothing from the records
//! and no name or number for the computer or the PAC. Settings → Open counts
//! turns this off; the app keeps that choice in its own settings folder, not
//! with the records.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use pac_keeper::server::CountOpens;
use tauri::{AppHandle, Manager};

use crate::show_settings;

/// Where the app reports its opens. PAC_OPENS_URL chooses another address.
const ADDRESS: &str = "https://app.packeeper.ca/desktop/opened";

const KIND: &str = if cfg!(target_os = "macos") { "mac" } else { "windows" };

/// The app's open counts. Tauri keeps it: `app.state::<AppOpens>()`.
pub struct AppOpens {
    /// What Settings shows.
    pub shared: Arc<CountOpens>,
    /// The file that keeps `Saved`.
    file: Option<PathBuf>,
    saved: Mutex<Saved>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Saved {
    count: bool,
    /// The last day (UTC, in days since 1970) that the server got an open.
    last_day: Option<u64>,
}

impl Saved {
    /// What this open tells the server: the first open today, and the first
    /// open on this computer.
    fn this_open(&self, today: u64) -> (bool, bool) {
        (self.last_day != Some(today), self.last_day.is_none())
    }
}

impl AppOpens {
    /// Reads the saved setting. The count is on until the person turns it off.
    pub fn load(app: &AppHandle) -> AppOpens {
        let file = app.path().app_config_dir().ok().map(|dir| dir.join("opens.json"));
        let value = file
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .unwrap_or_default();
        let saved = Saved {
            count: value.get("count").and_then(|v| v.as_bool()).unwrap_or(true),
            last_day: value.get("last_day").and_then(|v| v.as_u64()),
        };
        AppOpens { shared: Arc::new(CountOpens::new(saved.count)), file, saved: Mutex::new(saved) }
    }

    fn get(&self) -> Saved {
        *self.saved.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, change: impl FnOnce(&mut Saved)) -> anyhow::Result<()> {
        let path = self.file.as_ref().context("the computer gives no settings folder")?;
        let mut saved = self.saved.lock().unwrap_or_else(|e| e.into_inner());
        let mut new = *saved;
        change(&mut new);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::json!({ "count": new.count, "last_day": new.last_day }).to_string())?;
        *saved = new;
        Ok(())
    }
}

/// Reports this open, when the count is on. A development build reports
/// only to PAC_OPENS_URL: its opens are not people using the app.
pub fn count(app: &AppHandle) {
    let saved = app.state::<AppOpens>().get();
    if !saved.count {
        return;
    }
    let address = match std::env::var("PAC_OPENS_URL") {
        Ok(address) => address,
        Err(_) if cfg!(debug_assertions) => return,
        Err(_) => ADDRESS.to_string(),
    };
    let today = today();
    let (first_today, new) = saved.this_open(today);
    let version = app.package_info().version.to_string();
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || match send(&address, &version, first_today, new) {
        // Only an open that the server got moves the day on. After an open
        // without a connection, the next open is still the first today.
        Ok(()) => {
            if let Err(e) = app.state::<AppOpens>().save(|s| s.last_day = Some(today)) {
                tracing::warn!("save the day of the open: {e:#}");
            }
        }
        Err(e) => tracing::info!("could not count this open: {e:#}"),
    });
}

fn today() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |since| since.as_secs() / (24 * 60 * 60))
}

fn send(address: &str, version: &str, first_today: bool, new: bool) -> anyhow::Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(30))).build().into();
    agent
        .post(address)
        .header("User-Agent", format!("PAC Keeper/{version}"))
        .send_form([
            ("version", version),
            ("kind", KIND),
            ("first_today", if first_today { "true" } else { "false" }),
            ("new", if new { "true" } else { "false" }),
        ])
        .with_context(|| format!("send the open to {address}"))?;
    Ok(())
}

/// Does what an Open counts button in Settings asks: `on` or `off`.
pub fn act(app: &AppHandle, action: &str) {
    let on = match action {
        "on" => true,
        "off" => false,
        _ => {
            tracing::warn!("unknown open count action {action}");
            return;
        }
    };
    let app = app.clone();
    // Settings opens again after the navigation callback ends, as with the Updates buttons.
    tauri::async_runtime::spawn(async move {
        let me = app.state::<AppOpens>();
        if let Err(e) = me.save(|s| s.count = on) {
            tracing::error!("save the open count setting: {e:#}");
            show_settings(&app, false, &format!("PAC Keeper could not save the setting: {e:#}"));
            return;
        }
        me.shared.set(on);
        show_settings(&app, true, if on { "Open counts are on." } else { "Open counts are off." });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_an_open_tells_the_server() {
        let first = Saved { count: true, last_day: None };
        assert_eq!(first.this_open(20_000), (true, true));
        let yesterday = Saved { count: true, last_day: Some(19_999) };
        assert_eq!(yesterday.this_open(20_000), (true, false));
        let today = Saved { count: true, last_day: Some(20_000) };
        assert_eq!(today.this_open(20_000), (false, false));
    }
}
