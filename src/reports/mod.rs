//! Report data. The web pages and the PDFs use the same numbers.

pub mod gaming;
pub mod pdf;
pub mod proposal;

use std::collections::HashMap;

use chrono::NaiveDate;
use sqlx::SqlitePool;

use crate::db;
use crate::ledger::{self, Filter, TxRow};
use crate::models::{Account, FiscalYear, Fund, LineItem, Transaction};
use crate::money::Money;

/// Signed sums of allocations per line item over a date range, plus the
/// unallocated parts of transactions per fund.
#[derive(Debug, Default)]
pub struct Actuals {
    pub by_line: HashMap<i64, i64>,
    pub uncategorized_in: HashMap<&'static str, i64>,
    pub uncategorized_out: HashMap<&'static str, i64>,
}

impl Actuals {
    pub fn line(&self, id: i64) -> i64 {
        self.by_line.get(&id).copied().unwrap_or(0)
    }
}

pub async fn actuals(pool: &SqlitePool, from: NaiveDate, to: NaiveDate, cleared_only: bool) -> anyhow::Result<Actuals> {
    let mut out = Actuals::default();
    let rows: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT al.line_item_id, SUM(al.amount_cents)
         FROM allocations al
         JOIN transactions t ON t.id = al.transaction_id
         JOIN accounts a ON a.id = t.account_id
         WHERE t.date >= ? AND t.date <= ? AND t.date >= a.opening_date AND (t.cleared = 1 OR ? = 0)
         GROUP BY al.line_item_id",
    )
    .bind(from)
    .bind(to)
    .bind(cleared_only)
    .fetch_all(pool)
    .await?;
    out.by_line = rows.into_iter().collect();
    let rest: Vec<(String, i64)> = sqlx::query_as(
        "SELECT a.kind, t.amount_cents - COALESCE((SELECT SUM(amount_cents) FROM allocations WHERE transaction_id = t.id), 0)
         FROM transactions t JOIN accounts a ON a.id = t.account_id
         WHERE t.date >= ? AND t.date <= ? AND t.date >= a.opening_date AND (t.cleared = 1 OR ? = 0)",
    )
    .bind(from)
    .bind(to)
    .bind(cleared_only)
    .fetch_all(pool)
    .await?;
    for (kind, remainder) in rest {
        let fund = Fund::parse(&kind).unwrap_or(Fund::General).as_str();
        if remainder > 0 {
            *out.uncategorized_in.entry(fund).or_default() += remainder;
        } else if remainder < 0 {
            *out.uncategorized_out.entry(fund).or_default() += remainder;
        }
    }
    Ok(out)
}

pub async fn budgets(pool: &SqlitePool, fy_id: i64) -> anyhow::Result<HashMap<i64, (i64, String)>> {
    let rows: Vec<(i64, i64, String)> =
        sqlx::query_as("SELECT line_item_id, amount_cents, notes FROM budget_amounts WHERE fiscal_year_id = ?")
            .bind(fy_id)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(id, amount, notes)| (id, (amount, notes))).collect())
}

/// One budget line in a fund summary. Amounts are positive for income received
/// and for money spent.
#[derive(Debug, Clone)]
pub struct LineRow {
    pub line_item_id: Option<i64>,
    pub name: String,
    pub budget: Money,
    pub period: Money,
    pub ytd: Money,
}

impl LineRow {
    pub fn remaining(&self) -> Money {
        self.budget - self.ytd
    }

    /// Percent of budget used, for the progress bar. 0 when there is no budget.
    pub fn percent(&self) -> i64 {
        if self.budget.cents() <= 0 {
            return if self.ytd.cents() > 0 { 100 } else { 0 };
        }
        (self.ytd.cents() * 100 / self.budget.cents()).max(0)
    }
}

#[derive(Debug, Clone)]
pub struct FundSummary {
    pub fund: Fund,
    pub income: Vec<LineRow>,
    pub expense: Vec<LineRow>,
    pub transfers_period: Money,
    pub transfers_ytd: Money,
    /// Balance of the fund's accounts at the start of the period.
    pub opening: Money,
    /// Opening balances of accounts that the app starts to track during the period.
    pub opened: Money,
    pub closing: Money,
    /// Balance at the start of the fiscal year.
    pub year_opening: Money,
    /// Like `opened`, from the start of the fiscal year to the period end.
    pub year_opened: Money,
}

impl FundSummary {
    pub fn title(&self) -> &'static str {
        self.fund.title()
    }

    fn total(rows: &[LineRow], f: impl Fn(&LineRow) -> Money) -> Money {
        rows.iter().map(f).sum()
    }

    pub fn income_budget(&self) -> Money {
        Self::total(&self.income, |r| r.budget)
    }
    pub fn income_period(&self) -> Money {
        Self::total(&self.income, |r| r.period)
    }
    pub fn income_ytd(&self) -> Money {
        Self::total(&self.income, |r| r.ytd)
    }
    pub fn expense_budget(&self) -> Money {
        Self::total(&self.expense, |r| r.budget)
    }
    pub fn expense_period(&self) -> Money {
        Self::total(&self.expense, |r| r.period)
    }
    pub fn expense_ytd(&self) -> Money {
        Self::total(&self.expense, |r| r.ytd)
    }
    pub fn net_budget(&self) -> Money {
        self.income_budget() - self.expense_budget()
    }
    pub fn net_period(&self) -> Money {
        self.income_period() - self.expense_period()
    }
    pub fn net_ytd(&self) -> Money {
        self.income_ytd() - self.expense_ytd()
    }

    /// True when opening + new accounts + income - expenses + transfers = closing.
    pub fn balances(&self) -> bool {
        self.opening + self.opened + self.net_period() + self.transfers_period == self.closing
    }

    pub fn has_activity(&self) -> bool {
        !self.closing.is_zero()
            || !self.opening.is_zero()
            || self.income.iter().chain(&self.expense).any(|r| !r.budget.is_zero() || !r.ytd.is_zero())
    }
}

/// The fund's balance at the end of `date`.
pub async fn fund_balance(
    pool: &SqlitePool,
    accounts: &[Account],
    fund: Fund,
    date: NaiveDate,
    cleared_only: bool,
) -> anyhow::Result<Money> {
    let mut total = 0;
    for a in accounts.iter().filter(|a| a.fund() == fund) {
        total += db::balance_at(pool, a, date, cleared_only).await?;
    }
    Ok(Money(total))
}

/// The fund's balance at the start of `date`.
pub async fn fund_balance_before(
    pool: &SqlitePool,
    accounts: &[Account],
    fund: Fund,
    date: NaiveDate,
) -> anyhow::Result<Money> {
    let mut total = 0;
    for a in accounts.iter().filter(|a| a.fund() == fund) {
        total += db::balance_before(pool, a, date, false).await?;
    }
    Ok(Money(total))
}

/// Opening balances of the fund's accounts that start after `from` and on or before `to`.
fn opened_between(accounts: &[Account], fund: Fund, from: NaiveDate, to: NaiveDate) -> Money {
    accounts
        .iter()
        .filter(|a| a.fund() == fund && a.opening_date > from && a.opening_date <= to)
        .map(|a| a.opening_balance())
        .sum()
}

/// Budget against actual for both funds. `period` is a month for the monthly
/// report or the whole year for the annual report.
pub async fn fund_summaries(
    pool: &SqlitePool,
    fy: &FiscalYear,
    period_start: NaiveDate,
    period_end: NaiveDate,
) -> anyhow::Result<Vec<FundSummary>> {
    let items = db::line_items(pool).await?;
    let accounts = db::accounts(pool).await?;
    let budget = budgets(pool, fy.id).await?;
    let period = actuals(pool, period_start, period_end, false).await?;
    let ytd = actuals(pool, fy.start_date, period_end, false).await?;

    let mut out = Vec::new();
    for fund in Fund::ALL {
        let row = |li: &LineItem, sign: i64| LineRow {
            line_item_id: Some(li.id),
            name: li.name.clone(),
            budget: Money(budget.get(&li.id).map(|b| b.0).unwrap_or(0)),
            period: Money(sign * period.line(li.id)),
            ytd: Money(sign * ytd.line(li.id)),
        };
        let keep = |r: &LineRow, li: &LineItem| li.active || !r.budget.is_zero() || !r.ytd.is_zero();
        let mut income = Vec::new();
        let mut expense = Vec::new();
        let mut transfers_period = 0;
        let mut transfers_ytd = 0;
        for li in items.iter().filter(|li| li.fund() == fund) {
            match li.kind.as_str() {
                "income" => {
                    let r = row(li, 1);
                    if keep(&r, li) {
                        income.push(r);
                    }
                }
                "expense" => {
                    let r = row(li, -1);
                    if keep(&r, li) {
                        expense.push(r);
                    }
                }
                _ => {
                    transfers_period += period.line(li.id);
                    transfers_ytd += ytd.line(li.id);
                }
            }
        }
        let key = fund.as_str();
        let unc_in = (
            period.uncategorized_in.get(key).copied().unwrap_or(0),
            ytd.uncategorized_in.get(key).copied().unwrap_or(0),
        );
        if unc_in != (0, 0) {
            income.push(LineRow {
                line_item_id: None,
                name: "Uncategorized".into(),
                budget: Money(0),
                period: Money(unc_in.0),
                ytd: Money(unc_in.1),
            });
        }
        let unc_out = (
            period.uncategorized_out.get(key).copied().unwrap_or(0),
            ytd.uncategorized_out.get(key).copied().unwrap_or(0),
        );
        if unc_out != (0, 0) {
            expense.push(LineRow {
                line_item_id: None,
                name: "Uncategorized".into(),
                budget: Money(0),
                period: Money(-unc_out.0),
                ytd: Money(-unc_out.1),
            });
        }
        out.push(FundSummary {
            fund,
            income,
            expense,
            transfers_period: Money(transfers_period),
            transfers_ytd: Money(transfers_ytd),
            opening: fund_balance_before(pool, &accounts, fund, period_start).await?,
            opened: opened_between(&accounts, fund, period_start, period_end),
            closing: fund_balance(pool, &accounts, fund, period_end, false).await?,
            year_opening: fund_balance_before(pool, &accounts, fund, fy.start_date).await?,
            year_opened: opened_between(&accounts, fund, fy.start_date, period_end),
        });
    }
    Ok(out)
}

/// One account over a period, with a check against the bank's own balance.
#[derive(Debug, Clone)]
pub struct AccountSummary {
    pub account: Account,
    pub opening: Money,
    pub money_in: Money,
    pub money_out: Money,
    pub closing: Money,
    /// The last balance the bank reported in an import, on or before the period end.
    pub bank_balance: Option<(NaiveDate, Money)>,
    /// Our cleared balance on the bank balance date. It should equal the bank balance.
    pub book_at_bank_date: Option<Money>,
    /// Manual entries the bank has not shown yet, such as uncashed cheques.
    pub outstanding: Vec<Transaction>,
}

impl AccountSummary {
    pub fn reconciled(&self) -> Option<bool> {
        Some(self.bank_balance?.1 == self.book_at_bank_date?)
    }

    pub fn outstanding_total(&self) -> Money {
        self.outstanding.iter().map(|t| t.amount()).sum()
    }
}

pub async fn account_summaries(
    pool: &SqlitePool,
    from: NaiveDate,
    to: NaiveDate,
) -> anyhow::Result<Vec<AccountSummary>> {
    let mut out = Vec::new();
    for account in db::accounts(pool).await? {
        let (money_in, money_out): (i64, i64) = sqlx::query_as(
            "SELECT COALESCE(SUM(CASE WHEN amount_cents > 0 THEN amount_cents END), 0),
                    COALESCE(SUM(CASE WHEN amount_cents < 0 THEN amount_cents END), 0)
             FROM transactions WHERE account_id = ? AND date >= ? AND date <= ? AND date >= ?",
        )
        .bind(account.id)
        .bind(from)
        .bind(to)
        .bind(account.opening_date)
        .fetch_one(pool)
        .await?;
        // An account that starts during the period shows its opening balance as its opening.
        let opening = db::balance_before(pool, &account, from.max(account.opening_date), false).await?;
        let closing = db::balance_at(pool, &account, to, false).await?;
        if account.opening_date > to {
            continue;
        }
        if !account.active && money_in == 0 && money_out == 0 && closing == 0 {
            continue;
        }
        let bank = db::bank_balance_at(pool, account.id, to).await?;
        let book_at_bank_date = match bank {
            Some((d, _)) => Some(Money(db::balance_at(pool, &account, d, true).await?)),
            None => None,
        };
        let outstanding: Vec<Transaction> = sqlx::query_as(
            "SELECT * FROM transactions WHERE account_id = ? AND cleared = 0 AND date <= ? ORDER BY date",
        )
        .bind(account.id)
        .bind(to)
        .fetch_all(pool)
        .await?;
        out.push(AccountSummary {
            account,
            opening: Money(opening),
            money_in: Money(money_in),
            money_out: Money(money_out),
            closing: Money(closing),
            bank_balance: bank.map(|(d, b)| (d, Money(b))),
            book_at_bank_date,
            outstanding,
        });
    }
    Ok(out)
}

pub async fn transactions(pool: &SqlitePool, from: NaiveDate, to: NaiveDate) -> anyhow::Result<Vec<TxRow>> {
    let mut rows = ledger::list(pool, &Filter { from: Some(from), to: Some(to), ..Default::default() }).await?;
    rows.sort_by_key(|r| (r.tx.date, r.tx.id));
    Ok(rows)
}

/// Income and spending for each month of a year, per fund.
#[derive(Debug, Clone)]
pub struct MonthRow {
    pub month: NaiveDate,
    pub general_in: Money,
    pub general_out: Money,
    pub gaming_in: Money,
    pub gaming_out: Money,
}

pub async fn monthly_totals(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<Vec<MonthRow>> {
    let items: HashMap<i64, LineItem> = db::line_items(pool).await?.into_iter().map(|li| (li.id, li)).collect();
    let mut out = Vec::new();
    for month in crate::fiscal::months(fy.start_date, fy.end_date) {
        let a = actuals(pool, month, crate::fiscal::month_end(month), false).await?;
        let mut row =
            MonthRow { month, general_in: Money(0), general_out: Money(0), gaming_in: Money(0), gaming_out: Money(0) };
        for (id, amount) in &a.by_line {
            let Some(li) = items.get(id) else { continue };
            let gaming = li.fund() == Fund::Gaming;
            match li.kind.as_str() {
                "income" if gaming => row.gaming_in += Money(*amount),
                "income" => row.general_in += Money(*amount),
                "expense" if gaming => row.gaming_out += Money(-*amount),
                "expense" => row.general_out += Money(-*amount),
                _ => {}
            }
        }
        row.general_in += Money(a.uncategorized_in.get("general").copied().unwrap_or(0));
        row.general_out += Money(-a.uncategorized_out.get("general").copied().unwrap_or(0));
        row.gaming_in += Money(a.uncategorized_in.get("gaming").copied().unwrap_or(0));
        row.gaming_out += Money(-a.uncategorized_out.get("gaming").copied().unwrap_or(0));
        out.push(row);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fiscal;

    async fn demo_db() -> (SqlitePool, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("pac-test-{}-{}", std::process::id(), rand_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let pool = db::open(&dir).await.unwrap();
        db::seed(&pool).await.unwrap();
        crate::demo::load(&pool, &dir).await.unwrap();
        (pool, dir)
    }

    /// Unique per test. The clock alone is not enough: tests run in parallel,
    /// and on macOS two can read the same clock value.
    fn rand_suffix() -> String {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        format!("{t}-{n}")
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[tokio::test]
    async fn every_report_balances() {
        let (pool, dir) = demo_db().await;
        let fy = fiscal::ensure(&pool, d("2025-07-01")).await.unwrap();

        // The whole year, for both funds.
        let year = fund_summaries(&pool, &fy, fy.start_date, fy.end_date).await.unwrap();
        for f in &year {
            assert!(f.balances(), "{} does not balance for the year", f.title());
        }
        assert_eq!(year[0].year_opening, Money(841237));
        assert_eq!(year[0].closing, Money(1041535));
        // 5,484.96 from grants and interest, plus the raffle: 1,850.00 - 600.00 - 85.50.
        assert_eq!(year[1].closing, Money(664946));

        // Every month.
        for m in fiscal::months(fy.start_date, fy.end_date) {
            for f in fund_summaries(&pool, &fy, m, fiscal::month_end(m)).await.unwrap() {
                assert!(f.balances(), "{} does not balance in {m}", f.title());
            }
        }

        // The gaming report agrees with the bank and finds no problems.
        let g = gaming::build(&pool, &fy).await.unwrap();
        assert_eq!(g.closing(), g.accounts_total);
        assert_eq!(g.closing(), Money(664946));
        assert_eq!(g.problems(), 0, "{:?}", g.checks);
        assert_eq!(
            g.disbursements.iter().find(|x| !x.reimbursements.is_empty()).unwrap().reimbursement_total(),
            Money(50000)
        );

        // The licensed raffle fills lines 6 to 13 and Section 4, not the disbursements.
        assert_eq!(g.events.len(), 1);
        let raffle = &g.events[0];
        assert_eq!(raffle.amount, Money(185000));
        assert_eq!(raffle.event.as_ref().unwrap().licence_number, "D000000");
        assert_eq!((raffle.from, raffle.to), (d("2026-04-01"), d("2026-05-15")));
        assert_eq!(g.prizes.ticket_raffle, Money(60000));
        assert_eq!(g.event_expenses.ticket_raffle, Money(8550));
        assert_eq!(g.net_proceeds(), g.total_revenue() - Money(68550));
        assert!(g.disbursements.iter().all(|x| x.tx.reference != "111" && x.tx.reference != "112"));
        assert_eq!(g.event_costs.len(), 2);

        // Raffle money is newer than the grant, so it is not counted as unspent grant.
        assert_eq!(g.grants.len(), 1);
        assert_eq!(g.grants[0].unspent, Money(664946 - 240 - 185000 - 287 - 321));

        // Next year's report starts where this one ends.
        let next = fiscal::next(&pool, &fy).await.unwrap();
        let g2 = gaming::build(&pool, &next).await.unwrap();
        assert_eq!(g2.opening, g.closing());
        // The uncashed cheque is not on the bank-basis report.
        assert_eq!(g2.outstanding.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn gaming_revenue_lines_and_checks() {
        let (pool, dir) = demo_db().await;
        let gaming_account: i64 =
            sqlx::query_scalar("SELECT id FROM accounts WHERE kind = 'gaming'").fetch_one(&pool).await.unwrap();
        let line = |name: &'static str, gasr: Option<&'static str>| {
            let pool = pool.clone();
            async move {
                sqlx::query("INSERT INTO line_items (name, fund, kind, gasr_line) VALUES (?, 'gaming', 'income', ?)")
                    .bind(name)
                    .bind(gasr)
                    .execute(&pool)
                    .await
                    .unwrap()
                    .last_insert_rowid()
            }
        };
        let donation = line("Gaming donations", Some("15")).await;
        let wrong = line("Moved from general", None).await;
        for (date, payee, cents, li) in [
            ("2026-02-02", "Lions Club", 25000, donation),
            ("2026-03-03", "Lions Club", 5000, donation),
            ("2026-04-04", "General account", 10000, wrong),
        ] {
            let id = sqlx::query(
                "INSERT INTO transactions (account_id, date, description, payee, amount_cents, source, created_at)
                 VALUES (?, ?, 'DEPOSIT', ?, ?, 'import', '')",
            )
            .bind(gaming_account)
            .bind(d(date))
            .bind(payee)
            .bind(cents)
            .execute(&pool)
            .await
            .unwrap()
            .last_insert_rowid();
            sqlx::query("INSERT INTO allocations (transaction_id, line_item_id, amount_cents) VALUES (?, ?, ?)")
                .bind(id)
                .bind(li)
                .bind(cents)
                .execute(&pool)
                .await
                .unwrap();
        }
        let fy = fiscal::ensure(&pool, d("2025-07-01")).await.unwrap();
        let g = gaming::build(&pool, &fy).await.unwrap();
        // Donations from one organization share a line.
        assert_eq!(g.donations.len(), 1);
        assert_eq!(g.donations[0].name, "Lions Club");
        assert_eq!(g.donations[0].amount, Money(30000));
        // Money from the general account goes on line 5 and is a problem.
        assert_eq!(g.not_allowed, Money(10000));
        assert_eq!(g.problems(), 1, "{:?}", g.checks);
        // The report still agrees with the bank.
        assert_eq!(g.closing(), g.accounts_total);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn account_opened_mid_year() {
        let (pool, dir) = demo_db().await;
        sqlx::query(
            "INSERT INTO accounts (name, kind, opening_date, opening_balance_cents) VALUES ('Savings', 'general', '2025-11-15', 100000)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let fy = fiscal::ensure(&pool, d("2025-07-01")).await.unwrap();
        let general = &fund_summaries(&pool, &fy, fy.start_date, fy.end_date).await.unwrap()[0];
        assert_eq!(general.year_opening, Money(841237));
        assert_eq!(general.opened, Money(100000));
        assert!(general.balances());
        // The month before the account opens does not include it.
        let oct = &fund_summaries(&pool, &fy, d("2025-10-01"), d("2025-10-31")).await.unwrap()[0];
        assert!(oct.balances());
        assert!(oct.opened.is_zero());
        let _ = std::fs::remove_dir_all(dir);
    }
}
