//! Bank file import: parse, find duplicates, match manual entries, apply rules.

pub mod csv;
pub mod ofx;

use chrono::NaiveDate;
use sqlx::SqlitePool;

use crate::db;
use crate::models::{Account, LineItem, Rule};

/// One row from a bank file, before it becomes a transaction.
#[derive(Debug, Clone)]
pub struct BankRow {
    pub date: NaiveDate,
    pub description: String,
    pub amount_cents: i64,
    pub balance_cents: Option<i64>,
    pub reference: String,
    pub bank_ref: String,
}

impl BankRow {
    pub fn amount(&self) -> crate::money::Money {
        crate::money::Money(self.amount_cents)
    }

    pub fn balance(&self) -> Option<crate::money::Money> {
        self.balance_cents.map(crate::money::Money)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    New,
    Duplicate,
    /// Matches a manual entry (such as a cheque) that is waiting for the bank.
    MatchesManual(i64),
    /// Dated before the account's opening balance, so already counted in it.
    BeforeOpening,
}

impl RowStatus {
    pub fn label(self) -> &'static str {
        match self {
            RowStatus::New => "new",
            RowStatus::Duplicate => "already imported",
            RowStatus::MatchesManual(_) => "matches manual entry",
            RowStatus::BeforeOpening => "before opening date",
        }
    }

    pub fn css(self) -> &'static str {
        match self {
            RowStatus::New => "new",
            RowStatus::Duplicate => "dup",
            RowStatus::MatchesManual(_) => "match",
            RowStatus::BeforeOpening => "dup",
        }
    }
}

/// Manual entries are matched to bank rows within this many days.
const MATCH_WINDOW_DAYS: i64 = 30;

/// Works out what each row would do, without writing anything.
pub async fn classify(pool: &SqlitePool, account: &Account, rows: &[BankRow]) -> anyhow::Result<Vec<RowStatus>> {
    let account_id = account.id;
    let mut claimed: Vec<i64> = Vec::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if row.date < account.opening_date {
            out.push(RowStatus::BeforeOpening);
            continue;
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE account_id = ? AND bank_ref = ?")
            .bind(account_id)
            .bind(&row.bank_ref)
            .fetch_one(pool)
            .await?;
        if exists > 0 {
            out.push(RowStatus::Duplicate);
            continue;
        }
        match find_manual_match(pool, account_id, row, &claimed).await? {
            Some(id) => {
                claimed.push(id);
                out.push(RowStatus::MatchesManual(id));
            }
            None => out.push(RowStatus::New),
        }
    }
    Ok(out)
}

/// An uncleared manual entry with the same amount, close in date. A matching
/// cheque number wins over date distance.
async fn find_manual_match(
    pool: &SqlitePool,
    account_id: i64,
    row: &BankRow,
    claimed: &[i64],
) -> anyhow::Result<Option<i64>> {
    let candidates: Vec<(i64, NaiveDate, String)> = sqlx::query_as(
        "SELECT id, date, reference FROM transactions
         WHERE account_id = ? AND cleared = 0 AND amount_cents = ?",
    )
    .bind(account_id)
    .bind(row.amount_cents)
    .fetch_all(pool)
    .await?;
    let best = candidates
        .into_iter()
        .filter(|(id, _, _)| !claimed.contains(id))
        .filter(|(_, date, _)| (row.date - *date).num_days().abs() <= MATCH_WINDOW_DAYS)
        .filter(|(_, _, reference)| reference.is_empty() || row.reference.is_empty() || *reference == row.reference)
        .min_by_key(|(_, date, reference)| {
            let cheque_match = !reference.is_empty() && *reference == row.reference;
            (!cheque_match, (row.date - *date).num_days().abs())
        });
    Ok(best.map(|(id, _, _)| id))
}

#[derive(Debug, Default, Clone)]
pub struct ImportResult {
    pub total: i64,
    pub new: i64,
    pub duplicate: i64,
    pub matched: i64,
    pub before_opening: i64,
    pub auto_assigned: i64,
}

/// Writes the rows. Runs in one database transaction. `statement_balance` is
/// the bank's closing balance from an OFX file; it is recorded on the last
/// transaction on or before its date, so that reports can check the books against the bank.
pub async fn apply(
    pool: &SqlitePool,
    account: &Account,
    batch_id: i64,
    rows: &[BankRow],
    statement_balance: Option<(NaiveDate, i64)>,
) -> anyhow::Result<ImportResult> {
    let statuses = classify(pool, account, rows).await?;
    let rules = rules_for(pool, account).await?;
    let now = db::now();
    let mut result = ImportResult { total: rows.len() as i64, ..Default::default() };
    let mut tx = pool.begin().await?;
    for (row, status) in rows.iter().zip(statuses) {
        match status {
            RowStatus::Duplicate => result.duplicate += 1,
            RowStatus::BeforeOpening => result.before_opening += 1,
            RowStatus::MatchesManual(id) => {
                result.matched += 1;
                sqlx::query(
                    "UPDATE transactions
                     SET date = ?, description = ?, bank_balance_cents = ?, bank_ref = ?,
                         import_batch_id = ?, cleared = 1,
                         reference = CASE WHEN reference = '' THEN ? ELSE reference END
                     WHERE id = ?",
                )
                .bind(row.date)
                .bind(&row.description)
                .bind(row.balance_cents)
                .bind(&row.bank_ref)
                .bind(batch_id)
                .bind(&row.reference)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            RowStatus::New => {
                result.new += 1;
                let rule = match_rule(&rules, &row.description);
                let id = sqlx::query(
                    "INSERT INTO transactions
                       (account_id, date, description, payee, amount_cents, reference,
                        bank_balance_cents, source, import_batch_id, bank_ref, cleared, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, 'import', ?, ?, 1, ?)",
                )
                .bind(account.id)
                .bind(row.date)
                .bind(&row.description)
                .bind(rule.map(|(r, _)| r.payee.as_str()).unwrap_or(""))
                .bind(row.amount_cents)
                .bind(&row.reference)
                .bind(row.balance_cents)
                .bind(batch_id)
                .bind(&row.bank_ref)
                .bind(&now)
                .execute(&mut *tx)
                .await?
                .last_insert_rowid();
                if let Some((_, line_item)) = rule {
                    result.auto_assigned += 1;
                    sqlx::query(
                        "INSERT INTO allocations (transaction_id, line_item_id, amount_cents) VALUES (?, ?, ?)",
                    )
                    .bind(id)
                    .bind(line_item.id)
                    .bind(row.amount_cents)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
    }
    sqlx::query(
        "UPDATE import_batches
         SET status = 'done', rows_total = ?, rows_new = ?, rows_duplicate = ?, rows_matched = ?
         WHERE id = ?",
    )
    .bind(result.total)
    .bind(result.new)
    .bind(result.duplicate)
    .bind(result.matched)
    .bind(batch_id)
    .execute(&mut *tx)
    .await?;
    if let Some((date, balance)) = statement_balance {
        sqlx::query(
            "UPDATE transactions SET bank_balance_cents = ?
             WHERE id = (SELECT id FROM transactions WHERE account_id = ? AND cleared = 1 AND date <= ?
                         ORDER BY date DESC, id DESC LIMIT 1)",
        )
        .bind(balance)
        .bind(account.id)
        .bind(date)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(result)
}

/// Rules that can apply to this account, longest pattern first. A rule only
/// applies if its line item is in the same fund as the account.
pub async fn rules_for(pool: &SqlitePool, account: &Account) -> anyhow::Result<Vec<(Rule, LineItem)>> {
    let rules: Vec<Rule> = sqlx::query_as("SELECT * FROM rules WHERE account_id IS NULL OR account_id = ?")
        .bind(account.id)
        .fetch_all(pool)
        .await?;
    let items = db::line_items(pool).await?;
    let mut out: Vec<(Rule, LineItem)> = rules
        .into_iter()
        .filter_map(|r| {
            let li = items.iter().find(|li| li.id == r.line_item_id)?.clone();
            (li.fund() == account.fund() && li.active).then_some((r, li))
        })
        .collect();
    out.sort_by_key(|(r, _)| std::cmp::Reverse(r.pattern.len()));
    Ok(out)
}

pub fn match_rule<'a>(rules: &'a [(Rule, LineItem)], description: &str) -> Option<(&'a Rule, &'a LineItem)> {
    let d = description.to_lowercase();
    rules
        .iter()
        .find(|(r, _)| !r.pattern.trim().is_empty() && d.contains(&r.pattern.trim().to_lowercase()))
        .map(|(r, li)| (r, li))
}
