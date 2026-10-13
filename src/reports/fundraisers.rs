//! Fundraisers: what each one took in and spent in a fiscal year, against its
//! goal for that year. A fundraiser is not tied to a year; its parts are
//! counted by the date of their transaction.
//!
//! Income is the money in and costs are the money out, of the transaction
//! parts that carry the fundraiser. Net is income less costs. These totals sit
//! beside the budget: the parts also count on their line items as usual.

use std::collections::HashMap;

use chrono::NaiveDate;
use sqlx::{FromRow, SqlitePool};

use crate::db;
use crate::fiscal;
use crate::models::{FiscalYear, Fundraiser};
use crate::money::Money;

/// A fundraiser's money for one period.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    pub income: Money,
    /// Money out, as a positive amount.
    pub costs: Money,
    /// The number of transaction parts.
    pub parts: i64,
}

impl Totals {
    pub fn net(&self) -> Money {
        self.income - self.costs
    }

    pub fn is_empty(&self) -> bool {
        self.parts == 0
    }

    fn add(&mut self, other: &Totals) {
        self.income += other.income;
        self.costs += other.costs;
        self.parts += other.parts;
    }
}

#[derive(FromRow)]
struct TotalsRow {
    key: i64,
    income: i64,
    costs: i64,
    parts: i64,
}

impl From<TotalsRow> for Totals {
    fn from(r: TotalsRow) -> Totals {
        Totals { income: Money(r.income), costs: Money(r.costs), parts: r.parts }
    }
}

const SUMS: &str = "SUM(CASE WHEN al.amount_cents > 0 THEN al.amount_cents ELSE 0 END) AS income,
                    SUM(CASE WHEN al.amount_cents < 0 THEN -al.amount_cents ELSE 0 END) AS costs,
                    COUNT(*) AS parts";

/// Totals for each fundraiser, for transactions dated `from` to `to`.
pub async fn totals(pool: &SqlitePool, from: NaiveDate, to: NaiveDate) -> anyhow::Result<HashMap<i64, Totals>> {
    let rows: Vec<TotalsRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT al.fundraiser_id AS key, {SUMS}
         FROM allocations al JOIN transactions t ON t.id = al.transaction_id
         WHERE al.fundraiser_id IS NOT NULL AND t.date >= ? AND t.date <= ?
         GROUP BY al.fundraiser_id"
    )))
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.key, Totals::from(r))).collect())
}

/// The goal of each fundraiser that has one for the year.
pub async fn goals(pool: &SqlitePool, fy_id: i64) -> anyhow::Result<HashMap<i64, Money>> {
    let rows: Vec<(i64, i64)> =
        sqlx::query_as("SELECT fundraiser_id, goal_cents FROM fundraiser_goals WHERE fiscal_year_id = ?")
            .bind(fy_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(id, cents)| (id, Money(cents))).collect())
}

/// Sets or, with `None`, removes a fundraiser's goal for a year.
pub async fn set_goal(pool: &SqlitePool, fundraiser_id: i64, fy_id: i64, goal: Option<i64>) -> anyhow::Result<()> {
    match goal {
        Some(cents) => {
            sqlx::query(
                "INSERT INTO fundraiser_goals (fundraiser_id, fiscal_year_id, goal_cents) VALUES (?, ?, ?)
                 ON CONFLICT (fundraiser_id, fiscal_year_id) DO UPDATE SET goal_cents = excluded.goal_cents",
            )
            .bind(fundraiser_id)
            .bind(fy_id)
            .bind(cents)
            .execute(pool)
            .await?
        }
        None => {
            sqlx::query("DELETE FROM fundraiser_goals WHERE fundraiser_id = ? AND fiscal_year_id = ?")
                .bind(fundraiser_id)
                .bind(fy_id)
                .execute(pool)
                .await?
        }
    };
    Ok(())
}

/// One fundraiser in one fiscal year.
#[derive(Debug, Clone)]
pub struct Status {
    pub fundraiser: Fundraiser,
    /// From the start of the year to the report date.
    pub totals: Totals,
    /// Within the report's month, for the monthly report.
    pub month: Totals,
    pub goal: Option<Money>,
    /// The whole of the year before, if there was one.
    pub last_year: Option<Totals>,
}

impl Status {
    pub fn net(&self) -> Money {
        self.totals.net()
    }

    /// The net as a share of the goal, in percent.
    pub fn percent(&self) -> Option<i64> {
        let goal = self.goal?.cents();
        (goal > 0).then(|| self.net().cents().max(0) * 100 / goal)
    }

    /// The width of the progress bar, 0 to 100.
    pub fn bar(&self) -> i64 {
        self.percent().unwrap_or(0).min(100)
    }

    pub fn reached(&self) -> bool {
        self.goal.is_some_and(|g| self.net() >= g)
    }

    /// The amount still to raise to meet the goal.
    pub fn to_go(&self) -> Option<Money> {
        self.goal.map(|g| g - self.net()).filter(|m| m.cents() > 0)
    }

    pub fn last_year_net(&self) -> Option<Money> {
        self.last_year.filter(|t| !t.is_empty()).map(|t| t.net())
    }
}

/// The status of every fundraiser in `fy`, counting transactions up to `to`
/// (the year end, or a month end for the monthly report). It lists active
/// fundraisers, and inactive ones with activity in the year.
pub async fn year(
    pool: &SqlitePool,
    fy: &FiscalYear,
    month: Option<NaiveDate>,
    to: NaiveDate,
) -> anyhow::Result<Vec<Status>> {
    let mut ytd = totals(pool, fy.start_date, to).await?;
    let mut in_month = match month {
        Some(m) => totals(pool, m, to).await?,
        None => HashMap::new(),
    };
    let mut goals = goals(pool, fy.id).await?;
    let mut last = match fiscal::previous(pool, fy).await? {
        Some(prev) => Some(totals(pool, prev.start_date, prev.end_date).await?),
        None => None,
    };
    let mut out = Vec::new();
    for f in db::fundraisers(pool).await? {
        let totals = ytd.remove(&f.id).unwrap_or_default();
        if !f.active && totals.is_empty() {
            continue;
        }
        out.push(Status {
            month: in_month.remove(&f.id).unwrap_or_default(),
            goal: goals.remove(&f.id),
            last_year: last.as_mut().map(|l| l.remove(&f.id).unwrap_or_default()),
            fundraiser: f,
            totals,
        });
    }
    Ok(out)
}

/// One fundraiser's status for the whole of `fy`, active or not.
pub async fn one(pool: &SqlitePool, f: &Fundraiser, fy: &FiscalYear) -> anyhow::Result<Status> {
    let last_year = match fiscal::previous(pool, fy).await? {
        Some(prev) => Some(totals(pool, prev.start_date, prev.end_date).await?.remove(&f.id).unwrap_or_default()),
        None => None,
    };
    Ok(Status {
        fundraiser: f.clone(),
        totals: totals(pool, fy.start_date, fy.end_date).await?.remove(&f.id).unwrap_or_default(),
        month: Totals::default(),
        goal: goals(pool, fy.id).await?.remove(&f.id),
        last_year,
    })
}

/// The sum of the statuses' year figures.
pub fn sum<'a>(rows: impl IntoIterator<Item = &'a Status>) -> Totals {
    let mut t = Totals::default();
    for r in rows {
        t.add(&r.totals);
    }
    t
}

/// One fundraiser's figures in each fiscal year that has activity or a goal,
/// newest year first.
pub async fn history(
    pool: &SqlitePool,
    fundraiser_id: i64,
) -> anyhow::Result<Vec<(FiscalYear, Totals, Option<Money>)>> {
    let rows: Vec<TotalsRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT fy.id AS key, {SUMS}
         FROM allocations al
         JOIN transactions t ON t.id = al.transaction_id
         JOIN fiscal_years fy ON t.date >= fy.start_date AND t.date <= fy.end_date
         WHERE al.fundraiser_id = ?
         GROUP BY fy.id"
    )))
    .bind(fundraiser_id)
    .fetch_all(pool)
    .await?;
    let mut by_year: HashMap<i64, Totals> = rows.into_iter().map(|r| (r.key, Totals::from(r))).collect();
    let goals: HashMap<i64, Money> = sqlx::query_as::<_, (i64, i64)>(
        "SELECT fiscal_year_id, goal_cents FROM fundraiser_goals WHERE fundraiser_id = ?",
    )
    .bind(fundraiser_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(fy, cents)| (fy, Money(cents)))
    .collect();
    Ok(fiscal::list(pool)
        .await?
        .into_iter()
        .filter_map(|fy| {
            let t = by_year.remove(&fy.id).unwrap_or_default();
            let goal = goals.get(&fy.id).copied();
            (!t.is_empty() || goal.is_some()).then_some((fy, t, goal))
        })
        .collect())
}

/// One transaction part that belongs to a fundraiser.
#[derive(Debug, Clone, FromRow)]
pub struct PartRow {
    pub transaction_id: i64,
    pub date: NaiveDate,
    /// The payee, or the bank description.
    pub name: String,
    pub purpose: String,
    pub account_name: String,
    pub line_item: String,
    pub memo: String,
    pub amount_cents: i64,
    /// The transaction has other parts.
    pub split: bool,
    pub cleared: bool,
    pub receipts: i64,
    pub needs_receipt: bool,
}

impl PartRow {
    pub fn amount(&self) -> Money {
        Money(self.amount_cents)
    }
}

/// A fundraiser's parts dated `from` to `to`, oldest first.
pub async fn parts(
    pool: &SqlitePool,
    fundraiser_id: i64,
    from: NaiveDate,
    to: NaiveDate,
) -> anyhow::Result<Vec<PartRow>> {
    Ok(sqlx::query_as(
        "SELECT t.id AS transaction_id, t.date,
                CASE WHEN t.payee != '' THEN t.payee ELSE t.description END AS name,
                t.purpose, a.name AS account_name, li.name AS line_item, al.memo, al.amount_cents,
                (SELECT COUNT(*) FROM allocations x WHERE x.transaction_id = t.id) > 1 AS split,
                t.cleared,
                (SELECT COUNT(*) FROM attachments WHERE transaction_id = t.id) AS receipts,
                t.amount_cents < 0 AND li.receipt_required = 1
                  AND NOT EXISTS (SELECT 1 FROM attachments WHERE transaction_id = t.id) AS needs_receipt
         FROM allocations al
         JOIN transactions t ON t.id = al.transaction_id
         JOIN accounts a ON a.id = t.account_id
         JOIN line_items li ON li.id = al.line_item_id
         WHERE al.fundraiser_id = ? AND t.date >= ? AND t.date <= ?
         ORDER BY t.date, t.id, al.id",
    )
    .bind(fundraiser_id)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?)
}

/// A fundraiser's parts summed by line item: income first, then costs, the
/// larger amounts first.
pub fn by_line(parts: &[PartRow]) -> Vec<(String, Money)> {
    let mut out: Vec<(String, Money)> = Vec::new();
    for p in parts {
        match out.iter_mut().find(|(name, _)| *name == p.line_item) {
            Some((_, m)) => *m += p.amount(),
            None => out.push((p.line_item.clone(), p.amount())),
        }
    }
    out.sort_by_key(|(_, m)| (m.is_negative(), std::cmp::Reverse(m.cents().abs())));
    out
}

/// Things to fix before the fundraiser's figures are final.
pub fn checks(parts: &[PartRow]) -> Vec<String> {
    let mut out = Vec::new();
    let missing = parts.iter().filter(|p| p.needs_receipt).count();
    if missing > 0 {
        out.push(format!("{missing} payment(s) have no receipt attached."));
    }
    let uncleared = parts.iter().filter(|p| !p.cleared).count();
    if uncleared > 0 {
        out.push(format!("{uncleared} transaction(s) have not cleared at the bank yet."));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{self, Part};
    use crate::reports::tests::demo_db;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    async fn named(pool: &SqlitePool, name: &str) -> Fundraiser {
        sqlx::query_as("SELECT * FROM fundraisers WHERE name = ?").bind(name).fetch_one(pool).await.unwrap()
    }

    fn find<'a>(rows: &'a [Status], name: &str) -> &'a Status {
        rows.iter().find(|r| r.fundraiser.name == name).unwrap()
    }

    #[tokio::test]
    async fn status_for_each_year() {
        let (pool, dir) = demo_db().await;
        let last = fiscal::containing(&pool, d("2026-01-01")).await.unwrap().unwrap();
        let this = fiscal::containing(&pool, d("2026-10-01")).await.unwrap().unwrap();

        let rows = year(&pool, &last, None, last.end_date).await.unwrap();
        assert_eq!(rows.len(), 5);
        let lunch = find(&rows, "Hot lunch");
        assert_eq!((lunch.totals.income, lunch.totals.costs), (Money(905625), Money(647660)));
        assert_eq!(lunch.net(), Money(257965));
        assert!(lunch.reached());
        let purdys = find(&rows, "Purdy's chocolates");
        assert_eq!((purdys.net(), purdys.goal), (Money(81000), Some(Money(75000))));
        // Only the carnival's part of the split Costco purchase counts.
        let carnival = find(&rows, "Spring carnival");
        assert_eq!((carnival.totals.income, carnival.totals.costs), (Money(230000), Money(99460)));
        assert_eq!(carnival.totals.parts, 3);
        assert_eq!(carnival.to_go(), Some(Money(19460)));
        assert_eq!(carnival.percent(), Some(87));
        assert!(rows.iter().all(|r| r.last_year.is_none()), "no year before the first");

        let rows = year(&pool, &this, None, this.end_date).await.unwrap();
        let fair = find(&rows, "Fall fair");
        assert_eq!(fair.net(), Money(161050));
        assert_eq!(fair.last_year_net(), Some(Money(143007)));
        let movie = find(&rows, "Movie night");
        assert!(movie.totals.is_empty() && movie.goal.is_none());
        assert_eq!(movie.last_year_net(), Some(Money(42760)));

        // The monthly report: the month and the year to date.
        let sept = d("2026-09-01");
        let rows = year(&pool, &this, Some(sept), fiscal::month_end(sept)).await.unwrap();
        let lunch = find(&rows, "Hot lunch");
        assert_eq!(lunch.month.net(), Money(35970));
        assert_eq!(lunch.totals, lunch.month, "September is the only month with hot lunch so far");
        assert!(find(&rows, "Purdy's chocolates").totals.is_empty(), "October is after the report");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn history_and_parts() {
        let (pool, dir) = demo_db().await;
        let purdys = named(&pool, "Purdy's chocolates").await;
        let years = history(&pool, purdys.id).await.unwrap();
        let labels: Vec<(Money, Money, Option<Money>)> =
            years.iter().map(|(_, t, g)| (t.income, t.costs, *g)).collect();
        assert_eq!(
            labels,
            vec![(Money(64000), Money(0), Some(Money(90000))), (Money(324000), Money(243000), Some(Money(75000)))],
            "newest year first"
        );
        let last = &years[1].0;
        let parts = parts_for(&pool, purdys.id, last).await;
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| !p.needs_receipt), "the bill has its receipt");
        let lines = by_line(&parts);
        assert_eq!(lines[0], ("Fundraising – Events".to_string(), Money(324000)));
        assert_eq!(lines[1], ("Fundraising event costs".to_string(), Money(-243000)));

        let carnival = named(&pool, "Spring carnival").await;
        let parts = parts_for(&pool, carnival.id, last).await;
        let split = parts.iter().find(|p| p.split).expect("the Costco part");
        assert_eq!(split.amount(), Money(-21460));
        let _ = std::fs::remove_dir_all(dir);
    }

    async fn parts_for(pool: &SqlitePool, id: i64, fy: &FiscalYear) -> Vec<PartRow> {
        parts(pool, id, fy.start_date, fy.end_date).await.unwrap()
    }

    #[tokio::test]
    async fn gaming_money_has_no_fundraiser() {
        let (pool, dir) = demo_db().await;
        let lunch = named(&pool, "Hot lunch").await;
        let accounts = db::accounts(&pool).await.unwrap();
        let gaming = accounts.iter().find(|a| a.is_gaming()).unwrap();
        let general = accounts.iter().find(|a| !a.is_gaming()).unwrap();
        let items = db::line_items(&pool).await.unwrap();
        let tx_in = |account: i64| {
            let pool = pool.clone();
            async move {
                let id: i64 = sqlx::query_scalar(
                    "SELECT id FROM transactions WHERE account_id = ? AND amount_cents < 0 ORDER BY id LIMIT 1",
                )
                .bind(account)
                .fetch_one(&pool)
                .await
                .unwrap();
                ledger::get(&pool, id).await.unwrap().unwrap()
            }
        };

        let tx = tx_in(gaming.id).await;
        let li = items.iter().find(|li| li.fund == "gaming" && li.kind == "expense").unwrap();
        let part = Part { fundraiser_id: Some(lunch.id), ..Part::whole(li.id, &tx) };
        let err = ledger::set_allocations(&pool, &tx, gaming, &[part]).await.unwrap_err();
        assert!(err.to_string().contains("general-account money"), "{err}");

        let tx = tx_in(general.id).await;
        let li = items.iter().find(|li| li.fund == "general" && li.kind == "expense").unwrap();
        let part = Part { fundraiser_id: Some(lunch.id), ..Part::whole(li.id, &tx) };
        ledger::set_allocations(&pool, &tx, general, &[part]).await.unwrap();
        let saved = ledger::allocations(&pool, tx.id).await.unwrap();
        assert_eq!(saved[0].fundraiser_id, Some(lunch.id));

        let err = ledger::set_allocations(
            &pool,
            &tx,
            general,
            &[Part { fundraiser_id: Some(9999), ..Part::whole(li.id, &tx) }],
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("unknown fundraiser"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn import_rules_set_the_fundraiser() {
        let (pool, dir) = demo_db().await;
        let lunch = named(&pool, "Hot lunch").await;
        let general = db::accounts(&pool).await.unwrap().into_iter().find(|a| !a.is_gaming()).unwrap();
        let batch = sqlx::query(
            "INSERT INTO import_batches (account_id, filename, stored_name, format, created_at) VALUES (?, 'sample.qfx', 'x', 'ofx', ?)",
        )
        .bind(general.id)
        .bind(db::now())
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let st = crate::import::ofx::parse(crate::demo::sample_statement(d("2026-10-12")).as_bytes()).unwrap();
        crate::import::apply(&pool, &general, batch, &st.rows, None).await.unwrap();
        let fundraiser: Option<i64> = sqlx::query_scalar(
            "SELECT al.fundraiser_id FROM allocations al JOIN transactions t ON t.id = al.transaction_id
             WHERE t.import_batch_id = ? AND t.description LIKE '%MUNCH A LUNCH%'",
        )
        .bind(batch)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(fundraiser, Some(lunch.id));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn progress_against_the_goal() {
        let f = Fundraiser {
            id: 1,
            name: "Purdy's".into(),
            description: String::new(),
            active: true,
            created_at: String::new(),
        };
        let mut s = Status {
            fundraiser: f,
            totals: Totals { income: Money(120000), costs: Money(90000), parts: 2 },
            month: Totals::default(),
            goal: Some(Money(60000)),
            last_year: None,
        };
        assert_eq!(s.net(), Money(30000));
        assert_eq!(s.percent(), Some(50));
        assert_eq!(s.to_go(), Some(Money(30000)));
        assert!(!s.reached());
        s.totals.income = Money(200000);
        assert_eq!(s.percent(), Some(183));
        assert_eq!(s.bar(), 100);
        assert!(s.reached());
        assert_eq!(s.to_go(), None);
        s.goal = None;
        assert_eq!((s.percent(), s.bar()), (None, 0));
    }
}
