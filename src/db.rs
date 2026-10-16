//! Database connection, settings, and first-run seed data.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};

use crate::models::{Account, LineItem};

pub async fn open(data_dir: &Path) -> anyhow::Result<SqlitePool> {
    open_with(data_dir, None).await
}

/// Opens `pac.db`, encrypted with `key` on a hosted server with PAC_DATA_KEY.
pub async fn open_with(data_dir: &Path, key: Option<&crate::crypt::DbKey>) -> anyhow::Result<SqlitePool> {
    let path = data_dir.join("pac.db");
    let mut options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(10));
    if let Some(key) = key {
        options = key.apply(options);
    }
    let pool = SqlitePoolOptions::new().max_connections(4).connect_with(options).await.with_context(|| {
        let hint = if key.is_some() { " with PAC_DATA_KEY. Is it the key that encrypted it?" } else { "" };
        format!("open database {}{hint}", path.display())
    })?;
    sqlx::migrate!("./migrations").run(&pool).await.context("run migrations")?;
    Ok(pool)
}

pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// PAC-wide settings, stored as key/value rows.
#[derive(Debug, Clone)]
pub struct Settings {
    pub pac_name: String,
    pub school_name: String,
    pub fy_start_month: u32,
    pub enrolment: i64,
    pub gaming_rate_cents: i64,
    pub treasurer_name: String,
    pub chair_name: String,
    pub gaming_org_number: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            pac_name: "Parent Advisory Committee".into(),
            school_name: String::new(),
            fy_start_month: 7,
            enrolment: 0,
            gaming_rate_cents: 2000,
            treasurer_name: String::new(),
            chair_name: String::new(),
            gaming_org_number: String::new(),
        }
    }
}

impl Settings {
    pub async fn load(pool: &SqlitePool) -> anyhow::Result<Settings> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM settings").fetch_all(pool).await?;
        let mut s = Settings::default();
        for (key, value) in rows {
            match key.as_str() {
                "pac_name" => s.pac_name = value,
                "school_name" => s.school_name = value,
                "fy_start_month" => s.fy_start_month = value.parse().unwrap_or(7).clamp(1, 12),
                "enrolment" => s.enrolment = value.parse().unwrap_or(0),
                "gaming_rate_cents" => s.gaming_rate_cents = value.parse().unwrap_or(2000),
                "treasurer_name" => s.treasurer_name = value,
                "chair_name" => s.chair_name = value,
                "gaming_org_number" => s.gaming_org_number = value,
                _ => {}
            }
        }
        Ok(s)
    }

    pub async fn save(&self, pool: &SqlitePool) -> anyhow::Result<()> {
        let pairs = [
            ("pac_name", self.pac_name.clone()),
            ("school_name", self.school_name.clone()),
            ("fy_start_month", self.fy_start_month.to_string()),
            ("enrolment", self.enrolment.to_string()),
            ("gaming_rate_cents", self.gaming_rate_cents.to_string()),
            ("treasurer_name", self.treasurer_name.clone()),
            ("chair_name", self.chair_name.clone()),
            ("gaming_org_number", self.gaming_org_number.clone()),
        ];
        let mut tx = pool.begin().await?;
        for (key, value) in pairs {
            sqlx::query(
                "INSERT INTO settings (key, value) VALUES (?, ?)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(value)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The full PAC name for report headings, with the school if set.
    pub fn heading(&self) -> String {
        if self.school_name.is_empty() {
            self.pac_name.clone()
        } else {
            format!("{} — {}", self.school_name, self.pac_name)
        }
    }

    pub fn gaming_rate(&self) -> crate::money::Money {
        crate::money::Money(self.gaming_rate_cents)
    }

    /// Estimated annual gaming grant: enrolment x per-student rate.
    pub fn expected_gaming_grant_cents(&self) -> i64 {
        self.enrolment * self.gaming_rate_cents
    }
}

/// Starter line items for a new database. The treasurer edits these to match the PAC.
/// The gaming expense lines follow the eligible uses in the 2026 PAC gaming grant guide.
const DEFAULT_LINE_ITEMS: &[(&str, &str, &str, Option<&str>)] = &[
    // (fund, kind, name, GASR line)
    ("general", "income", "Fundraising – Hot lunch", None),
    ("general", "income", "Fundraising – Events", None),
    ("general", "income", "Donations", None),
    ("general", "income", "Reimbursement from gaming account", None),
    ("general", "income", "Interest", None),
    ("general", "expense", "Hot lunch costs", None),
    ("general", "expense", "Fundraising event costs", None),
    ("general", "expense", "Staff appreciation", None),
    ("general", "expense", "Classroom grants", None),
    ("general", "expense", "Hospitality and meetings", None),
    ("general", "expense", "Insurance", None),
    ("general", "expense", "Office and supplies", None),
    ("general", "expense", "Bank fees", None),
    ("general", "expense", "Gaming-eligible costs (to be reimbursed)", None),
    ("general", "transfer", "Transfer between general accounts", None),
    ("gaming", "income", "PAC gaming grant", Some("3")),
    ("gaming", "income", "Interest and GST rebates", Some("14")),
    ("gaming", "expense", "Field trips (within BC)", None),
    ("gaming", "expense", "Presentations and performances", None),
    ("gaming", "expense", "School spirit and family events", None),
    ("gaming", "expense", "Clubs and student competitions", None),
    ("gaming", "expense", "Sports equipment and uniforms", None),
    ("gaming", "expense", "Playground and beautification", None),
    ("gaming", "expense", "Graduation and student recognition", None),
    ("gaming", "expense", "Yearbook and newsletters", None),
    ("gaming", "expense", "BCCPAC membership and conference", None),
    ("gaming", "expense", "Emergency supplies", None),
    ("gaming", "expense", "Bank charges", None),
    ("gaming", "transfer", "Transfer between gaming accounts", None),
];

/// Line items added in seed version 2, for licensed gaming events.
const EVENT_LINE_ITEMS: &[(&str, &str, &str, Option<&str>)] = &[
    ("gaming", "income", "Licensed gaming event revenue", Some("6")),
    ("gaming", "expense", "Licensed gaming event prizes", Some("prize")),
    ("gaming", "expense", "Licensed gaming event expenses", Some("event_expense")),
];

const SEED_VERSION: i64 = 2;

/// Seeds the starter line items into a new database. An older database gets
/// only the line items added since its seed version, once.
pub async fn seed(pool: &SqlitePool) -> anyhow::Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM line_items").fetch_one(pool).await?;
    let recorded: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'seed_version'").fetch_optional(pool).await?;
    let version: i64 = recorded.as_deref().and_then(|v| v.parse().ok()).unwrap_or(1);
    // Only a new database gets every starter line item. When a treasurer
    // deletes every line item, they stay deleted.
    let items: Vec<_> = if count == 0 && recorded.is_none() {
        DEFAULT_LINE_ITEMS.iter().chain(EVENT_LINE_ITEMS).collect()
    } else if version < 2 {
        EVENT_LINE_ITEMS.iter().collect()
    } else {
        Vec::new()
    };
    let next_sort: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), 0) FROM line_items").fetch_one(pool).await?;
    for (i, (fund, kind, name, gasr_line)) in items.into_iter().enumerate() {
        // Bank statements are the record for bank charges; other expenses need a receipt.
        let receipt_required = *kind == "expense" && !name.starts_with("Bank");
        // OR IGNORE: keep a line item that the treasurer already made with this name.
        sqlx::query(
            "INSERT OR IGNORE INTO line_items (name, fund, kind, gasr_line, receipt_required, sort_order)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(name)
        .bind(fund)
        .bind(kind)
        .bind(gasr_line)
        .bind(receipt_required)
        .bind(next_sort + (i as i64 + 1) * 10)
        .execute(pool)
        .await?;
    }
    sqlx::query("INSERT INTO settings (key, value) VALUES ('seed_version', ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(SEED_VERSION.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn gaming_events(pool: &SqlitePool) -> anyhow::Result<Vec<crate::models::GamingEvent>> {
    Ok(sqlx::query_as("SELECT * FROM gaming_events ORDER BY start_date DESC, id DESC").fetch_all(pool).await?)
}

pub async fn gaming_event(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<crate::models::GamingEvent>> {
    Ok(sqlx::query_as("SELECT * FROM gaming_events WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

/// Active fundraisers first, then by name.
pub async fn fundraisers(pool: &SqlitePool) -> anyhow::Result<Vec<crate::models::Fundraiser>> {
    Ok(sqlx::query_as("SELECT * FROM fundraisers ORDER BY active DESC, name COLLATE NOCASE").fetch_all(pool).await?)
}

pub async fn fundraiser(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<crate::models::Fundraiser>> {
    Ok(sqlx::query_as("SELECT * FROM fundraisers WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

pub async fn accounts(pool: &SqlitePool) -> anyhow::Result<Vec<Account>> {
    Ok(sqlx::query_as("SELECT * FROM accounts ORDER BY active DESC, kind, name").fetch_all(pool).await?)
}

pub async fn account(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Account>> {
    Ok(sqlx::query_as("SELECT * FROM accounts WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

pub async fn line_items(pool: &SqlitePool) -> anyhow::Result<Vec<LineItem>> {
    Ok(sqlx::query_as(
        "SELECT * FROM line_items
         ORDER BY fund, CASE kind WHEN 'income' THEN 0 WHEN 'expense' THEN 1 ELSE 2 END,
                  sort_order, name",
    )
    .fetch_all(pool)
    .await?)
}

/// Balance of an account at the end of `date`: the opening balance plus the
/// transactions from the opening date to `date`. The app does not track an
/// account before its opening date, so the balance there is zero.
/// With `cleared_only`, manual entries that the bank has not shown are left out.
pub async fn balance_at(
    pool: &SqlitePool,
    account: &Account,
    date: chrono::NaiveDate,
    cleared_only: bool,
) -> anyhow::Result<i64> {
    if date < account.opening_date {
        return Ok(0);
    }
    let sum: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(amount_cents), 0) FROM transactions
         WHERE account_id = ? AND date >= ? AND date <= ? AND (cleared = 1 OR ? = 0)",
    )
    .bind(account.id)
    .bind(account.opening_date)
    .bind(date)
    .bind(cleared_only)
    .fetch_one(pool)
    .await?;
    Ok(account.opening_balance_cents + sum)
}

/// Balance of an account at the start of `date`, before that day's transactions.
/// On the opening date this is the opening balance.
pub async fn balance_before(
    pool: &SqlitePool,
    account: &Account,
    date: chrono::NaiveDate,
    cleared_only: bool,
) -> anyhow::Result<i64> {
    if date <= account.opening_date {
        return Ok(if date == account.opening_date { account.opening_balance_cents } else { 0 });
    }
    balance_at(pool, account, date.pred_opt().expect("date in range"), cleared_only).await
}

/// The bank's own running balance from the last imported row on or before `date`.
pub async fn bank_balance_at(
    pool: &SqlitePool,
    account_id: i64,
    date: chrono::NaiveDate,
) -> anyhow::Result<Option<(chrono::NaiveDate, i64)>> {
    Ok(sqlx::query_as(
        "SELECT date, bank_balance_cents FROM transactions
         WHERE account_id = ? AND date <= ? AND bank_balance_cents IS NOT NULL
         ORDER BY date DESC, id DESC LIMIT 1",
    )
    .bind(account_id)
    .bind(date)
    .fetch_optional(pool)
    .await?)
}
