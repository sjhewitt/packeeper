use std::io::Write;
use std::path::{Path, PathBuf};

use askama::Template;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::{Form, Query};
use serde::Deserialize;

use super::{AppResult, AppState, Flash, Layout, UserExt, redirect_err, redirect_ok, render, require_edit};
use crate::db::Settings;
use crate::money::{self, Money};

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    layout: Layout,
    s: Settings,
    data_dir: String,
    auth: bool,
    months: Vec<(u32, &'static str)>,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

pub async fn page(State(state): State<AppState>, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    render(SettingsPage {
        layout: Layout::new(&state, &user, "settings", "Settings", &flash).await?,
        s: Settings::load(&state.pool).await?,
        data_dir: state.data_dir.canonicalize().unwrap_or(state.data_dir.clone()).display().to_string(),
        auth: state.auth,
        months: MONTHS.iter().enumerate().map(|(i, m)| (i as u32 + 1, *m)).collect(),
    })
}

#[derive(Deserialize)]
pub struct SettingsForm {
    pac_name: String,
    school_name: String,
    fy_start_month: u32,
    enrolment: String,
    gaming_rate: String,
    treasurer_name: String,
    chair_name: String,
    gaming_org_number: String,
}

pub async fn save(State(state): State<AppState>, user: UserExt, Form(f): Form<SettingsForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let enrolment = if f.enrolment.trim().is_empty() {
        0
    } else {
        match f.enrolment.trim().parse::<i64>() {
            Ok(n) if n >= 0 => n,
            _ => return Ok(redirect_err("/settings", "Enrolment must be a whole number.")),
        }
    };
    let Some(rate) = money::parse(&f.gaming_rate).filter(|r| *r >= 0) else {
        return Ok(redirect_err("/settings", "Enter the gaming grant rate per student, such as 20.00."));
    };
    let s = Settings {
        pac_name: if f.pac_name.trim().is_empty() { Settings::default().pac_name } else { f.pac_name.trim().into() },
        school_name: f.school_name.trim().into(),
        fy_start_month: f.fy_start_month.clamp(1, 12),
        enrolment,
        gaming_rate_cents: rate,
        treasurer_name: f.treasurer_name.trim().into(),
        chair_name: f.chair_name.trim().into(),
        gaming_org_number: f.gaming_org_number.trim().into(),
    };
    s.save(&state.pool).await?;
    Ok(redirect_ok("/settings", "Settings saved."))
}

impl SettingsPage {
    fn rate(&self) -> String {
        Money(self.s.gaming_rate_cents).plain()
    }
}

/// A zip of the database, receipts, and imported bank files.
pub async fn backup(State(state): State<AppState>, user: UserExt) -> AppResult<Response> {
    // The backup holds everything, including password hashes.
    require_edit(&user)?;
    let dir = state.data_dir.join("backups");
    tokio::fs::create_dir_all(&dir).await?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let snapshot = dir.join(format!("pac-{stamp}.db"));
    // VACUUM INTO writes a consistent copy while the app keeps running.
    sqlx::query("VACUUM INTO ?").bind(snapshot.display().to_string()).execute(&state.pool).await?;
    let data_dir = state.data_dir.clone();
    let snap = snapshot.clone();
    let bytes = tokio::task::spawn_blocking(move || build_zip(&data_dir, &snap)).await??;
    let _ = tokio::fs::remove_file(&snapshot).await;
    let filename = format!("pac-keeper-backup-{stamp}.zip");
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\"")),
        ],
        bytes,
    )
        .into_response())
}

fn build_zip(data_dir: &Path, snapshot: &Path) -> anyhow::Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("pac.db", opts)?;
        zip.write_all(&std::fs::read(snapshot)?)?;
        for sub in ["receipts", "imports"] {
            for file in walk(&data_dir.join(sub)) {
                let rel = file.strip_prefix(data_dir)?.to_string_lossy().replace('\\', "/");
                zip.start_file(rel, opts)?;
                zip.write_all(&std::fs::read(&file)?)?;
            }
        }
        zip.finish()?;
    }
    Ok(buf.into_inner())
}

fn walk(dir: &Path) -> Vec<PathBuf> {
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
