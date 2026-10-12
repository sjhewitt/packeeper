//! PAC Finances: bookkeeping and reports for a BC school Parent Advisory Committee.

mod db;
mod demo;
mod fiscal;
mod import;
mod ledger;
mod models;
mod money;
mod reports;
mod web;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use axum_extra::extract::cookie::Key;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about = "Bookkeeping and reports for a BC Parent Advisory Committee")]
struct Cli {
    /// Folder for the database, receipts, and imported bank files.
    #[arg(long, env = "PAC_DATA_DIR", default_value = "data", global = true)]
    data_dir: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the web app (the default).
    Serve {
        /// Address to listen on. Use 0.0.0.0:8080 on a server.
        #[arg(long, env = "PAC_BIND", default_value = "127.0.0.1:8080")]
        bind: SocketAddr,
        /// Require sign-in. Turn this on when the app runs on a server.
        #[arg(long, env = "PAC_AUTH", action = clap::ArgAction::SetTrue, value_parser = clap::builder::BoolishValueParser::new())]
        auth: bool,
    },
    /// Manage sign-in users (for --auth).
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    /// Write a PDF report to a file.
    Pdf {
        #[command(subcommand)]
        report: PdfReport,
        /// Output file (default: report.pdf in the current folder).
        #[arg(short, long, global = true)]
        output: Option<PathBuf>,
    },
    /// Fill an empty data folder with an example PAC, to try the app.
    Demo,
}

#[derive(Subcommand)]
enum UserCommand {
    /// Add a user, or reset an existing user's password and role.
    Add {
        username: String,
        /// treasurer (can edit) or viewer (read only).
        #[arg(long, default_value = "viewer")]
        role: String,
        /// Name shown in the app.
        #[arg(long, default_value = "")]
        name: String,
    },
    /// List users.
    List,
    /// Remove a user.
    Remove { username: String },
}

#[derive(Subcommand)]
enum PdfReport {
    /// Monthly treasurer's report, for a month such as 2026-09.
    Monthly { month: String },
    /// Annual report for the AGM, for a fiscal year label such as 2025–26 (default: current).
    Annual { year: Option<String> },
    /// Budget proposal for a fiscal year (default: next year).
    Budget { year: Option<String> },
    /// Gaming Account Summary worksheet (default: current year).
    Gaming { year: Option<String> },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.data_dir).with_context(|| format!("create {}", cli.data_dir.display()))?;
    let pool = db::open(&cli.data_dir).await?;
    db::seed(&pool).await?;
    fiscal::current(&pool).await?;

    match cli.command.unwrap_or(Command::Serve { bind: "127.0.0.1:8080".parse()?, auth: false }) {
        Command::Serve { bind, auth } => serve(pool, &cli.data_dir, bind, auth).await,
        Command::User { command } => users(&pool, command).await,
        Command::Pdf { report, output } => pdf(&pool, report, output).await,
        Command::Demo => demo::load(&pool, &cli.data_dir).await,
    }
}

async fn serve(pool: sqlx::SqlitePool, data_dir: &Path, bind: SocketAddr, auth: bool) -> anyhow::Result<()> {
    let state = web::AppState {
        pool,
        data_dir: data_dir.to_path_buf(),
        renderer: Arc::new(reports::pdf::Renderer::new()),
        auth,
        key: load_key(data_dir)?,
    };
    let app = web::router(state);
    let listener = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("listen on {bind}"))?;
    println!("PAC Finances is running at http://{bind}");
    println!("Data folder: {}", data_dir.canonicalize().unwrap_or(data_dir.to_path_buf()).display());
    if !auth && !bind.ip().is_loopback() {
        println!(
            "Warning: sign-in is off but the app listens on {bind}. Anyone on the network can change the records. Use --auth."
        );
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// The key that encrypts session cookies, made on first run.
fn load_key(data_dir: &Path) -> anyhow::Result<Key> {
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

async fn users(pool: &sqlx::SqlitePool, command: UserCommand) -> anyhow::Result<()> {
    match command {
        UserCommand::Add { username, role, name } => {
            // PAC_PASSWORD allows scripted set-up on a server with no terminal.
            let password = match std::env::var("PAC_PASSWORD") {
                Ok(p) => p,
                Err(_) => {
                    let p = rpassword::prompt_password(format!("Password for {username}: "))?;
                    if p != rpassword::prompt_password("Again: ")? {
                        anyhow::bail!("the passwords do not match");
                    }
                    p
                }
            };
            if password.len() < 8 {
                anyhow::bail!("use a password of at least 8 characters");
            }
            web::add_user(pool, &username, &name, &password, &role).await?;
            println!("Saved user {username} ({role}).");
        }
        UserCommand::List => {
            let rows: Vec<(String, String, String)> =
                sqlx::query_as("SELECT username, display_name, role FROM users ORDER BY username")
                    .fetch_all(pool)
                    .await?;
            for (u, n, r) in rows {
                println!("{u:<20} {r:<10} {n}");
            }
        }
        UserCommand::Remove { username } => {
            let n = sqlx::query("DELETE FROM users WHERE username = ?")
                .bind(&username)
                .execute(pool)
                .await?
                .rows_affected();
            println!("Removed {n} user(s).");
        }
    }
    Ok(())
}

async fn find_year(pool: &sqlx::SqlitePool, label: Option<String>) -> anyhow::Result<models::FiscalYear> {
    match label {
        None => fiscal::current(pool).await,
        Some(l) => {
            let normal = |s: &str| s.replace(['–', '-', '/'], "");
            fiscal::list(pool)
                .await?
                .into_iter()
                .find(|y| normal(&y.label) == normal(&l))
                .ok_or_else(|| anyhow::anyhow!("no fiscal year called {l}"))
        }
    }
}

async fn pdf(pool: &sqlx::SqlitePool, report: PdfReport, output: Option<PathBuf>) -> anyhow::Result<()> {
    let output = output.unwrap_or_else(|| PathBuf::from("report.pdf"));
    let (template, data) = match report {
        PdfReport::Monthly { month } => {
            let m = chrono::NaiveDate::parse_from_str(&format!("{month}-01"), "%Y-%m-%d")
                .context("month must look like 2026-09")?;
            ("monthly.typ", reports::pdf::monthly(pool, m).await?)
        }
        PdfReport::Annual { year } => {
            let fy = find_year(pool, year).await?;
            ("annual.typ", reports::pdf::annual(pool, &fy).await?)
        }
        PdfReport::Budget { year } => {
            let target = match year {
                Some(_) => find_year(pool, year).await?,
                None => fiscal::next(pool, &fiscal::current(pool).await?).await?,
            };
            let base = fiscal::previous(pool, &target).await?.context("no year before the budget year")?;
            let p = reports::proposal::build(pool, &base, &target).await?;
            ("budget.typ", reports::pdf::budget(pool, &p).await?)
        }
        PdfReport::Gaming { year } => {
            let fy = find_year(pool, year).await?;
            let g = reports::gaming::build(pool, &fy).await?;
            ("gaming.typ", reports::pdf::gasr(pool, &g).await?)
        }
    };
    let bytes = reports::pdf::Renderer::new().render(template, &data)?;
    std::fs::write(&output, bytes)?;
    println!("Wrote {}", output.display());
    Ok(())
}
