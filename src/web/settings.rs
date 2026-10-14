use std::path::{Path, PathBuf};

use askama::Template;
use axum::extract::Multipart;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::{Form, Query};
use serde::Deserialize;

use super::{AppResult, Ctx, Flash, Layout, UserExt, redirect_err, redirect_ok, render, require_edit};
use crate::db::Settings;
use crate::money::{self, Money};

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    layout: Layout,
    s: Settings,
    data_dir: String,
    auth: bool,
    hosted: bool,
    months: Vec<(u32, &'static str)>,
    /// The server runs hot lunch, and whether this PAC has it on.
    lunch_available: bool,
    lunch_on: bool,
    /// A copy of the records from before the last restore exists.
    before_restore: bool,
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

pub async fn page(state: Ctx, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    render(SettingsPage {
        layout: Layout::new(&state, &user, "settings", "Settings", &flash).await?,
        s: Settings::load(&state.pool).await?,
        data_dir: state.data_dir.canonicalize().unwrap_or(state.data_dir.clone()).display().to_string(),
        auth: state.auth,
        hosted: state.hosted.is_some(),
        months: MONTHS.iter().enumerate().map(|(i, m)| (i as u32 + 1, *m)).collect(),
        lunch_available: state.lunch.is_some(),
        lunch_on: state.lunch.is_some() && crate::lunch::enabled(&state.pool).await?,
        before_restore: before_restore_path(&state).exists(),
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

pub async fn save(state: Ctx, user: UserExt, Form(f): Form<SettingsForm>) -> AppResult<Response> {
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
    if let Some(h) = &state.hosted {
        h.hosted.rename(h.pac.id, &s.pac_name, &s.school_name).await?;
    }
    Ok(redirect_ok("/settings", "Settings saved."))
}

impl SettingsPage {
    fn rate(&self) -> String {
        Money(self.s.gaming_rate_cents).plain()
    }
}

/// A zip of the database, receipts, and imported bank files.
pub async fn backup(state: Ctx, user: UserExt) -> AppResult<Response> {
    // The backup holds everything, including password hashes.
    require_edit(&user)?;
    let bytes = crate::backup::snapshot(&state.pool, &state.data_dir).await?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    Ok(zip_response(bytes, &format!("pac-keeper-backup-{stamp}.zip")))
}

fn zip_response(bytes: Vec<u8>, filename: &str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{filename}\"")),
        ],
        bytes,
    )
        .into_response()
}

fn before_restore_path(state: &Ctx) -> PathBuf {
    state.data_dir.join("backups").join("before-restore.zip")
}

/// Replaces the PAC's records with a backup zip. See `crate::backup`.
pub async fn restore(state: Ctx, user: UserExt, mut multipart: Multipart) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(refused) = super::no_uploads_in_demo(&state, "/settings") {
        return Ok(refused);
    }
    let options = crate::backup::Options {
        max_file_bytes: state.hosted.as_ref().map(|h| h.hosted.quota_bytes.max(0) as u64),
        families: match &state.hosted {
            Some(h) => Some(h.hosted.families(h.pac.id).await?.iter().map(|p| p.id).collect()),
            None => None,
        },
    };
    // Write the upload to a file as it arrives: a backup can be large.
    let upload = state.data_dir.join(format!(".upload-{}.zip", chrono::Local::now().format("%Y%m%d%H%M%S%f")));
    let saved = save_upload(&mut multipart, &upload, crate::backup::upload_limit(&options)).await;
    let result = match saved {
        Ok(true) => crate::backup::restore(&state.pool, &state.data_dir, &upload, &options).await,
        Ok(false) => Err(anyhow::Error::new(crate::backup::Refused("Choose the backup file (.zip).".into()))),
        Err(e) => Err(e),
    };
    let _ = tokio::fs::remove_file(&upload).await;
    match result {
        Ok(r) => {
            let mut msg = format!(
                "Backup restored: {} transactions and {} receipts. The records from before are under \"Download the records from before the restore\".",
                r.transactions, r.receipts
            );
            if r.left_out_lunch {
                msg.push_str(" Hot lunch children and orders of parents who are not in this PAC were left out.");
            }
            Ok(redirect_ok("/settings", &msg))
        }
        Err(e) => match e.downcast_ref::<crate::backup::Refused>() {
            Some(refused) => Ok(redirect_err("/settings", &refused.0)),
            None => {
                tracing::error!("restore failed: {e:#}");
                Ok(redirect_err("/settings", "The backup could not be restored. Try again, or ask for help."))
            }
        },
    }
}

/// Writes the "backup" field of the form to `path`. False when there is none.
async fn save_upload(multipart: &mut Multipart, path: &Path, limit: u64) -> anyhow::Result<bool> {
    use tokio::io::AsyncWriteExt;
    while let Some(mut field) = multipart.next_field().await? {
        if field.name() != Some("backup") || field.file_name().is_none_or(str::is_empty) {
            continue;
        }
        let mut file = tokio::fs::File::create(path).await?;
        let mut written = 0u64;
        while let Some(chunk) = field.chunk().await? {
            written += chunk.len() as u64;
            if written > limit {
                return Err(crate::backup::Refused("The backup is larger than this PAC may store.".into()).into());
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        return Ok(written > 0);
    }
    Ok(false)
}

/// The records from before the last restore.
pub async fn before_restore(state: Ctx, user: UserExt) -> AppResult<Response> {
    require_edit(&user)?;
    let bytes = tokio::fs::read(before_restore_path(&state)).await.map_err(|_| super::AppError::NotFound)?;
    Ok(zip_response(bytes, "pac-keeper-before-restore.zip"))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use crate::web::test_support::{example, send, send_file};

    /// A backup goes back in through Settings, and the records from before can be downloaded.
    #[tokio::test]
    async fn a_backup_restores_from_the_settings_page() {
        let (app, pool, dir) = example("restore-web").await;
        let backup = crate::backup::snapshot(&pool, &dir).await.unwrap();
        sqlx::query("UPDATE settings SET value = 'Changed PAC' WHERE key = 'pac_name'").execute(&pool).await.unwrap();

        let (_, _, page) = send(&app, "GET", "/settings", "").await;
        assert!(page.contains("Restore the backup") && !page.contains("before-restore.zip"));
        let (status, location) = send_file(&app, "/settings/restore", "backup", "pac.zip", &backup).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(location.contains("msg=Backup%20restored"), "{location}");
        assert_eq!(crate::db::Settings::load(&pool).await.unwrap().pac_name, "Example Elementary PAC");

        let (_, location) = send_file(&app, "/settings/restore", "backup", "notes.zip", b"not a zip").await;
        assert!(location.contains("err=This%20is%20not%20a%20PAC%20Keeper%20backup"), "{location}");
        let (_, location) = send_file(&app, "/settings/restore", "other", "pac.zip", &backup).await;
        assert!(location.contains("err=Choose%20the%20backup"), "{location}");

        let (_, _, page) = send(&app, "GET", "/settings", "").await;
        assert!(page.contains("/settings/before-restore.zip"));
        let (status, _, _) = send(&app, "GET", "/settings/before-restore.zip", "").await;
        assert_eq!(status, StatusCode::OK);
        let _ = std::fs::remove_dir_all(dir);
    }
}
