//! The pac-keeper command line.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, Subcommand};
use pac_keeper::{demo, fiscal, hosted, models, reports, server, web};

#[derive(Parser)]
#[command(
    name = "pac-keeper",
    version,
    about = "PAC Keeper: bookkeeping and reports for a BC Parent Advisory Committee",
    after_help = "https://packeeper.ca"
)]
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
        /// Run a public demo: load the example PAC, show a public sign-in,
        /// turn off file uploads, and reset the data on a schedule. Only runs
        /// on a data folder that holds example data or nothing.
        #[arg(long, env = "PAC_DEMO", action = clap::ArgAction::SetTrue, value_parser = clap::builder::BoolishValueParser::new())]
        demo: bool,
        /// Minutes between demo resets.
        #[arg(long, env = "PAC_DEMO_RESET_MINUTES", default_value_t = 60, value_parser = clap::value_parser!(u32).range(5..))]
        demo_reset_minutes: u32,
        /// Host many PACs: people sign up, sign in by email, and an operator
        /// approves each PAC. The data folder holds a folder for each PAC.
        #[arg(long, env = "PAC_HOSTED", action = clap::ArgAction::SetTrue, value_parser = clap::builder::BoolishValueParser::new())]
        hosted: bool,
        /// The public address of a hosted server, for links in emails.
        #[arg(long, env = "PAC_BASE_URL", default_value = "http://127.0.0.1:8080")]
        base_url: String,
        /// Receipt storage for each PAC on a hosted server, in megabytes.
        #[arg(long, env = "PAC_QUOTA_MB", default_value_t = 1024)]
        quota_mb: i64,
    },
    /// Make someone an operator of a hosted server: they approve new PACs.
    Operator {
        email: String,
        #[arg(long, default_value = "Operator")]
        name: String,
    },
    /// Copy every database, receipt, and bank file into a dated folder.
    /// Safe while the server runs. Works on a hosted or a single-PAC folder.
    Backup {
        /// The folder that receives the dated copy.
        #[arg(long)]
        to: PathBuf,
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
    let dir = cli.data_dir.clone();
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    let default = Command::Serve {
        bind: "127.0.0.1:8080".parse()?,
        auth: false,
        demo: false,
        demo_reset_minutes: 60,
        hosted: false,
        base_url: "http://127.0.0.1:8080".into(),
        quota_mb: 1024,
    };
    match cli.command.unwrap_or(default) {
        Command::Serve { bind, hosted: true, demo, base_url, quota_mb, .. } => {
            if demo {
                anyhow::bail!("--demo runs on a single-PAC folder, not with --hosted");
            }
            server::serve_hosted(&dir, bind, base_url, quota_mb).await
        }
        Command::Serve { bind, auth, demo, demo_reset_minutes, .. } => {
            let pool = server::open_single(&dir).await?;
            server::serve(pool, &dir, bind, auth, demo.then_some(demo_reset_minutes)).await
        }
        Command::User { command } => users(&server::open_single(&dir).await?, command).await,
        Command::Pdf { report, output } => pdf(&server::open_single(&dir).await?, report, output).await,
        Command::Demo => {
            demo::load(&server::open_single(&dir).await?, &dir).await?;
            println!("Loaded the example PAC. Start PAC Keeper with the same --data-dir and open it in a browser.");
            Ok(())
        }
        Command::Operator { email, name } => operator(&dir, &email, &name).await,
        Command::Backup { to } => {
            let dest = hosted::backup(&dir, &to).await?;
            println!("Copied every database, receipt, and bank file to {}", dest.display());
            Ok(())
        }
    }
}

async fn operator(dir: &Path, email: &str, name: &str) -> anyhow::Result<()> {
    if !hosted::looks_like_email(email) {
        anyhow::bail!("{email} is not an email address");
    }
    let hosted = server::open_hosted(dir, String::new(), 1024).await?;
    let person = hosted.ensure_person(&email.to_lowercase(), name).await?;
    if person.password_hash.is_none() {
        let password = read_password(email)?;
        hosted.set_password(person.id, &password).await?;
    }
    hosted.mark_verified(person.id).await?;
    sqlx::query("UPDATE people SET is_operator = 1 WHERE id = ?").bind(person.id).execute(&hosted.control).await?;
    println!("{email} is an operator. Sign in, then open /operator to approve PACs.");
    Ok(())
}

/// A new password, from PAC_PASSWORD or typed twice.
fn read_password(who: &str) -> anyhow::Result<String> {
    let password = match std::env::var("PAC_PASSWORD") {
        Ok(p) => p,
        Err(_) => {
            let p = rpassword::prompt_password(format!("Password for {who}: "))?;
            if p != rpassword::prompt_password("Again: ")? {
                anyhow::bail!("the passwords do not match");
            }
            p
        }
    };
    if password.chars().count() < 10 {
        anyhow::bail!("use a password of at least 10 characters");
    }
    Ok(password)
}

async fn users(pool: &sqlx::SqlitePool, command: UserCommand) -> anyhow::Result<()> {
    match command {
        UserCommand::Add { username, role, name } => {
            // PAC_PASSWORD allows scripted set-up on a server with no terminal.
            let password = read_password(&username)?;
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
