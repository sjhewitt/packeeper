//! Fiscal years. The start month is a setting; each year is a row so that a
//! PAC can change its year end without rewriting history.

use chrono::{Datelike, Months, NaiveDate};
use sqlx::SqlitePool;

use crate::db::Settings;
use crate::models::FiscalYear;

/// The start and end of the fiscal year that contains `date`.
pub fn bounds(date: NaiveDate, start_month: u32) -> (NaiveDate, NaiveDate) {
    let year = if date.month() >= start_month { date.year() } else { date.year() - 1 };
    let start = NaiveDate::from_ymd_opt(year, start_month, 1).expect("valid month");
    let end = start + Months::new(12) - chrono::Days::new(1);
    (start, end)
}

/// "2025–26" for a July start, "2026" for a January start.
pub fn label(start: NaiveDate, end: NaiveDate) -> String {
    if start.year() == end.year() {
        start.year().to_string()
    } else {
        format!("{}–{:02}", start.year(), end.year() % 100)
    }
}

pub async fn list(pool: &SqlitePool) -> anyhow::Result<Vec<FiscalYear>> {
    Ok(sqlx::query_as("SELECT * FROM fiscal_years ORDER BY start_date DESC").fetch_all(pool).await?)
}

pub async fn get(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<FiscalYear>> {
    Ok(sqlx::query_as("SELECT * FROM fiscal_years WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

pub async fn containing(pool: &SqlitePool, date: NaiveDate) -> anyhow::Result<Option<FiscalYear>> {
    Ok(sqlx::query_as("SELECT * FROM fiscal_years WHERE start_date <= ? AND end_date >= ?")
        .bind(date)
        .bind(date)
        .fetch_optional(pool)
        .await?)
}

/// The fiscal year that contains `date`. Creates it from the settings if it is missing.
pub async fn ensure(pool: &SqlitePool, date: NaiveDate) -> anyhow::Result<FiscalYear> {
    if let Some(fy) = containing(pool, date).await? {
        return Ok(fy);
    }
    let settings = Settings::load(pool).await?;
    let (mut start, mut end) = bounds(date, settings.fy_start_month);
    // Do not overlap a neighbouring year, for example after a change of start month.
    let prev_end: Option<NaiveDate> = sqlx::query_scalar("SELECT MAX(end_date) FROM fiscal_years WHERE end_date < ?")
        .bind(date)
        .fetch_one(pool)
        .await?;
    if let Some(prev_end) = prev_end {
        start = start.max(prev_end.succ_opt().expect("date in range"));
    }
    let next_start: Option<NaiveDate> =
        sqlx::query_scalar("SELECT MIN(start_date) FROM fiscal_years WHERE start_date > ?")
            .bind(date)
            .fetch_one(pool)
            .await?;
    if let Some(next_start) = next_start {
        end = end.min(next_start.pred_opt().expect("date in range"));
    }
    let id = sqlx::query("INSERT INTO fiscal_years (label, start_date, end_date) VALUES (?, ?, ?)")
        .bind(unique_label(pool, &label(start, end)).await?)
        .bind(start)
        .bind(end)
        .execute(pool)
        .await?
        .last_insert_rowid();
    Ok(get(pool, id).await?.expect("just inserted"))
}

async fn unique_label(pool: &SqlitePool, base: &str) -> anyhow::Result<String> {
    let mut candidate = base.to_string();
    let mut n = 2;
    loop {
        let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fiscal_years WHERE label = ?")
            .bind(&candidate)
            .fetch_one(pool)
            .await?;
        if taken == 0 {
            return Ok(candidate);
        }
        candidate = format!("{base} ({n})");
        n += 1;
    }
}

pub async fn current(pool: &SqlitePool) -> anyhow::Result<FiscalYear> {
    ensure(pool, chrono::Local::now().date_naive()).await
}

/// The year before `fy`, if one exists.
pub async fn previous(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<Option<FiscalYear>> {
    Ok(sqlx::query_as("SELECT * FROM fiscal_years WHERE end_date < ? ORDER BY end_date DESC LIMIT 1")
        .bind(fy.start_date)
        .fetch_optional(pool)
        .await?)
}

/// The year after `fy`. Creates it if it is missing.
pub async fn next(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<FiscalYear> {
    ensure(pool, fy.end_date.succ_opt().expect("date in range")).await
}

/// The first day of each month from `start` to `end`.
pub fn months(start: NaiveDate, end: NaiveDate) -> Vec<NaiveDate> {
    let mut out = Vec::new();
    let mut m = start.with_day(1).expect("day 1");
    while m <= end {
        out.push(m);
        m = m + Months::new(1);
    }
    out
}

pub fn month_end(month_start: NaiveDate) -> NaiveDate {
    month_start + Months::new(1) - chrono::Days::new(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn july_year() {
        assert_eq!(bounds(d(2026, 10, 11), 7), (d(2026, 7, 1), d(2027, 6, 30)));
        assert_eq!(bounds(d(2026, 3, 1), 7), (d(2025, 7, 1), d(2026, 6, 30)));
        assert_eq!(label(d(2026, 7, 1), d(2027, 6, 30)), "2026–27");
    }

    #[test]
    fn calendar_year() {
        assert_eq!(bounds(d(2026, 1, 1), 1), (d(2026, 1, 1), d(2026, 12, 31)));
        assert_eq!(label(d(2026, 1, 1), d(2026, 12, 31)), "2026");
    }

    #[test]
    fn month_list() {
        let ms = months(d(2025, 7, 1), d(2026, 6, 30));
        assert_eq!(ms.len(), 12);
        assert_eq!(month_end(d(2026, 2, 1)), d(2026, 2, 28));
    }
}
