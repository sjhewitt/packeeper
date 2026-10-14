//! PAC meetings, and the period that the treasurer's report covers for each.
//!
//! A rule, such as "the 2nd Wednesday of each month, but not in July or
//! August", fills in a fiscal year's meetings once, the first time the year is
//! shown. After that the treasurer changes single dates, for example a
//! December meeting moved a week earlier.
//!
//! The report for a meeting covers the days from the previous meeting up to
//! the day before this meeting, so the meeting day starts the next period. A
//! period never starts before its fiscal year: the first report after the
//! summer starts on the first day of the year.

use chrono::{Datelike, Days, NaiveDate, Weekday};
use sqlx::SqlitePool;

use crate::models::FiscalYear;

/// Which day of the month the PAC meets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// 1 to 4 for the first to fourth weekday of the month; 5 for the last.
    pub week: u32,
    pub weekday: Weekday,
    /// Months with no meeting, 1 to 12.
    pub skip_months: Vec<u32>,
}

pub const WEEKS: [(u32, &str); 5] = [(1, "1st"), (2, "2nd"), (3, "3rd"), (4, "4th"), (5, "last")];
pub const WEEKDAYS: [(Weekday, &str); 7] = [
    (Weekday::Mon, "Monday"),
    (Weekday::Tue, "Tuesday"),
    (Weekday::Wed, "Wednesday"),
    (Weekday::Thu, "Thursday"),
    (Weekday::Fri, "Friday"),
    (Weekday::Sat, "Saturday"),
    (Weekday::Sun, "Sunday"),
];

impl Rule {
    pub async fn load(pool: &SqlitePool) -> anyhow::Result<Option<Rule>> {
        let get = |key: &'static str| async move {
            sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(pool)
                .await
        };
        let (Some(week), Some(weekday)) = (get("meeting_week").await?, get("meeting_weekday").await?) else {
            return Ok(None);
        };
        let (Ok(week), Ok(weekday)) = (week.parse::<u32>(), weekday.parse::<Weekday>()) else { return Ok(None) };
        let skip_months = get("meeting_skip_months")
            .await?
            .unwrap_or_default()
            .split(',')
            .filter_map(|m| m.trim().parse().ok())
            .filter(|m| (1..=12).contains(m))
            .collect();
        Ok(Some(Rule { week: week.clamp(1, 5), weekday, skip_months }))
    }

    pub async fn save(&self, pool: &SqlitePool) -> anyhow::Result<()> {
        let skip: Vec<String> = self.skip_months.iter().map(u32::to_string).collect();
        for (key, value) in [
            ("meeting_week", self.week.to_string()),
            ("meeting_weekday", weekday_name(self.weekday).to_string()),
            ("meeting_skip_months", skip.join(",")),
        ] {
            sqlx::query(
                "INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(value)
            .execute(pool)
            .await?;
        }
        Ok(())
    }

    /// The rule in words, such as "the 2nd Wednesday of each month, except July and August".
    pub fn describe(&self) -> String {
        let week = WEEKS.iter().find(|(n, _)| *n == self.week).map_or("", |(_, w)| w);
        let mut text = format!("the {week} {} of each month", weekday_name(self.weekday));
        if !self.skip_months.is_empty() {
            let months: Vec<&str> = self.skip_months.iter().map(|m| month_name(*m)).collect();
            text.push_str(&format!(", except {}", and_list(&months)));
        }
        text
    }

    pub fn skips(&self, month: &u32) -> bool {
        self.skip_months.contains(month)
    }

    /// The meeting days from `from` to `to`.
    pub fn dates(&self, from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
        crate::fiscal::months(from, to)
            .into_iter()
            .filter(|m| !self.skip_months.contains(&m.month()))
            .filter_map(|m| self.day_in(m))
            .filter(|d| *d >= from && *d <= to)
            .collect()
    }

    /// The meeting day in the month that starts on `first`.
    fn day_in(&self, first: NaiveDate) -> Option<NaiveDate> {
        let days: Vec<NaiveDate> = first
            .iter_days()
            .take_while(|d| d.month() == first.month())
            .filter(|d| d.weekday() == self.weekday)
            .collect();
        if self.week == 5 { days.last().copied() } else { days.get(self.week as usize - 1).copied() }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Meeting {
    pub id: i64,
    pub date: NaiveDate,
    pub note: String,
}

/// The days that the report for one meeting covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Period {
    pub meeting: NaiveDate,
    pub start: NaiveDate,
    pub end: NaiveDate,
}

impl Period {
    /// Such as "Oct 14, 2026 meeting: Sep 9 – Oct 13".
    pub fn label(&self) -> String {
        format!("{} meeting: {}", self.meeting.format("%b %-d, %Y"), self.range_short())
    }

    /// Such as "Sep 9 – Oct 13".
    pub fn range_short(&self) -> String {
        format!("{} – {}", self.start.format("%b %-d"), self.end.format("%b %-d"))
    }

    pub fn key(&self) -> String {
        self.meeting.format("%Y-%m-%d").to_string()
    }
}

/// The meetings in a fiscal year. The first time, the rule fills them in.
pub async fn in_year(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<Vec<Meeting>> {
    fill_once(pool, fy).await?;
    Ok(sqlx::query_as("SELECT * FROM meetings WHERE date BETWEEN ? AND ? ORDER BY date")
        .bind(fy.start_date)
        .bind(fy.end_date)
        .fetch_all(pool)
        .await?)
}

/// Fills in a fiscal year from the rule, once. A year without a rule stays
/// empty, and it fills in once the PAC has a rule.
async fn fill_once(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<()> {
    let filled: bool =
        sqlx::query_scalar("SELECT meetings_filled FROM fiscal_years WHERE id = ?").bind(fy.id).fetch_one(pool).await?;
    if !filled && let Some(rule) = Rule::load(pool).await? {
        fill(pool, fy, &rule, false).await?;
    }
    Ok(())
}

/// Puts the rule's meetings into a fiscal year. `replace` first removes the
/// year's meetings, for a changed rule.
pub async fn fill(pool: &SqlitePool, fy: &FiscalYear, rule: &Rule, replace: bool) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    if replace {
        sqlx::query("DELETE FROM meetings WHERE date BETWEEN ? AND ?")
            .bind(fy.start_date)
            .bind(fy.end_date)
            .execute(&mut *tx)
            .await?;
    }
    for date in rule.dates(fy.start_date, fy.end_date) {
        sqlx::query("INSERT OR IGNORE INTO meetings (date) VALUES (?)").bind(date).execute(&mut *tx).await?;
    }
    sqlx::query("UPDATE fiscal_years SET meetings_filled = 1 WHERE id = ?").bind(fy.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// The report period of each meeting in a fiscal year, in date order.
pub async fn periods(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<Vec<Period>> {
    let mut periods = Vec::new();
    for m in in_year(pool, fy).await? {
        if let Some(p) = period_of(pool, fy, m.date).await? {
            periods.push(p);
        }
    }
    Ok(periods)
}

/// The report period of the meeting on `meeting`. None when the PAC has no
/// meeting that day, or the period would be empty (a meeting on the first
/// day of the fiscal year).
pub async fn period(pool: &SqlitePool, meeting: NaiveDate) -> anyhow::Result<Option<Period>> {
    let known: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM meetings WHERE date = ?").bind(meeting).fetch_one(pool).await?;
    if known == 0 {
        return Ok(None);
    }
    let fy = crate::fiscal::ensure(pool, meeting).await?;
    period_of(pool, &fy, meeting).await
}

async fn period_of(pool: &SqlitePool, fy: &FiscalYear, meeting: NaiveDate) -> anyhow::Result<Option<Period>> {
    let previous: Option<NaiveDate> =
        sqlx::query_scalar("SELECT MAX(date) FROM meetings WHERE date < ?").bind(meeting).fetch_one(pool).await?;
    let start = previous.map_or(fy.start_date, |p| p.max(fy.start_date));
    let Some(end) = meeting.checked_sub_days(Days::new(1)) else { return Ok(None) };
    Ok((end >= start).then_some(Period { meeting, start, end }))
}

/// The period to offer first: the next meeting from `today`, or else the last one.
pub fn default_period(periods: &[Period], today: NaiveDate) -> Option<&Period> {
    periods.iter().find(|p| p.meeting >= today).or_else(|| periods.last())
}

pub async fn add(pool: &SqlitePool, date: NaiveDate, note: &str) -> anyhow::Result<bool> {
    let done = sqlx::query("INSERT OR IGNORE INTO meetings (date, note) VALUES (?, ?)")
        .bind(date)
        .bind(note.trim())
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// Moves a meeting or changes its note. False when another meeting has that date.
pub async fn update(pool: &SqlitePool, id: i64, date: NaiveDate, note: &str) -> anyhow::Result<bool> {
    let done = sqlx::query("UPDATE OR IGNORE meetings SET date = ?, note = ? WHERE id = ?")
        .bind(date)
        .bind(note.trim())
        .bind(id)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn remove(pool: &SqlitePool, id: i64) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM meetings WHERE id = ?").bind(id).execute(pool).await?;
    Ok(())
}

pub fn weekday_name(day: Weekday) -> &'static str {
    WEEKDAYS.iter().find(|(d, _)| *d == day).map_or("", |(_, n)| n)
}

pub fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
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
    NAMES.get(month as usize - 1).copied().unwrap_or("")
}

fn and_list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn the_rule_finds_each_meeting_day() {
        let rule = Rule { week: 2, weekday: Weekday::Wed, skip_months: vec![7, 8] };
        let dates = rule.dates(day("2026-07-01"), day("2027-06-30"));
        assert_eq!(dates.len(), 10, "no meeting in July or August");
        assert_eq!(dates[0], day("2026-09-09"));
        assert_eq!(dates[1], day("2026-10-14"));
        assert_eq!(*dates.last().unwrap(), day("2027-06-09"));
        assert_eq!(rule.describe(), "the 2nd Wednesday of each month, except July and August");
        let last = Rule { week: 5, weekday: Weekday::Mon, skip_months: vec![] };
        assert_eq!(last.dates(day("2026-11-01"), day("2026-11-30")), vec![day("2026-11-30")]);
    }

    #[tokio::test]
    async fn each_report_runs_from_the_last_meeting_to_the_day_before() {
        let dir = std::env::temp_dir().join(format!("pac-meetings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pool = crate::server::open_single(&dir).await.unwrap();
        let fy = crate::fiscal::ensure(&pool, day("2026-10-01")).await.unwrap();
        // No rule: no meetings, and the year fills in later.
        assert!(periods(&pool, &fy).await.unwrap().is_empty());
        let rule = Rule { week: 2, weekday: Weekday::Wed, skip_months: vec![7, 8] };
        rule.save(&pool).await.unwrap();
        assert_eq!(Rule::load(&pool).await.unwrap(), Some(rule));
        // A meeting in June of the year before.
        add(&pool, day("2026-06-10"), "").await.unwrap();

        let ps = periods(&pool, &fy).await.unwrap();
        assert_eq!(ps.len(), 10);
        // The first report of the year starts on its first day, not at the June meeting.
        assert_eq!(ps[0], Period { meeting: day("2026-09-09"), start: day("2026-07-01"), end: day("2026-09-08") });
        assert_eq!(ps[1], Period { meeting: day("2026-10-14"), start: day("2026-09-09"), end: day("2026-10-13") });
        assert_eq!(ps[1].label(), "Oct 14, 2026 meeting: Sep 9 – Oct 13");

        // The December meeting moves a week earlier; the periods follow.
        let dec: Meeting =
            sqlx::query_as("SELECT * FROM meetings WHERE date = '2026-12-09'").fetch_one(&pool).await.unwrap();
        assert!(update(&pool, dec.id, day("2026-12-02"), "moved for the winter concert").await.unwrap());
        assert!(!update(&pool, dec.id, day("2026-11-11"), "").await.unwrap(), "two meetings on one day");
        let moved = period(&pool, day("2026-12-02")).await.unwrap().unwrap();
        assert_eq!((moved.start, moved.end), (day("2026-11-11"), day("2026-12-01")));
        let jan = period(&pool, day("2027-01-13")).await.unwrap().unwrap();
        assert_eq!(jan.start, day("2026-12-02"));
        assert_eq!(period(&pool, day("2026-12-09")).await.unwrap(), None, "no meeting that day");

        // Deleting every meeting does not bring the rule's dates back.
        sqlx::query("DELETE FROM meetings").execute(&pool).await.unwrap();
        assert!(periods(&pool, &fy).await.unwrap().is_empty());
        assert_eq!(default_period(&ps, day("2026-10-01")).unwrap().meeting, day("2026-10-14"));
        assert_eq!(default_period(&ps, day("2027-08-01")).unwrap().meeting, day("2027-06-09"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
