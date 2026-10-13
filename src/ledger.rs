//! Transaction queries and edits shared by the web pages and the reports.

use std::collections::HashMap;

use chrono::NaiveDate;
use sqlx::{AssertSqlSafe, FromRow, QueryBuilder, Sqlite, SqlitePool};

use crate::models::{Account, Allocation, Attachment, LineItem, Transaction};
use crate::money::Money;

/// A transaction with the names a list needs.
#[derive(Debug, Clone, FromRow)]
pub struct TxRow {
    #[sqlx(flatten)]
    pub tx: Transaction,
    pub account_name: String,
    pub account_kind: String,
    pub allocated_cents: i64,
    pub line_names: Option<String>,
    pub single_line_id: Option<i64>,
    pub allocation_count: i64,
    pub receipt_count: i64,
    pub receipt_required: bool,
    /// A licensed gaming event line item with no event chosen.
    pub needs_event: bool,
    /// The fundraisers of the transaction's parts, joined with "; ".
    pub fundraiser_names: Option<String>,
    /// The fundraiser of the only (or first) part.
    pub single_fundraiser_id: Option<i64>,
}

impl TxRow {
    pub fn unallocated(&self) -> Money {
        Money(self.tx.amount_cents - self.allocated_cents)
    }

    pub fn is_categorized(&self) -> bool {
        self.allocation_count > 0 && self.tx.amount_cents == self.allocated_cents
    }

    /// True when the transaction's only (or first) line item is `id`.
    pub fn is_line(&self, id: &i64) -> bool {
        self.single_line_id == Some(*id)
    }

    pub fn is_split(&self) -> bool {
        self.allocation_count > 1
    }

    /// Money out with no receipt, where the line item asks for one.
    pub fn needs_receipt(&self) -> bool {
        self.tx.amount_cents < 0 && self.receipt_count == 0 && self.receipt_required
    }

    pub fn line_names(&self) -> &str {
        self.line_names.as_deref().unwrap_or("")
    }

    pub fn fundraiser_names(&self) -> &str {
        self.fundraiser_names.as_deref().unwrap_or("")
    }

    /// True when the transaction's only (or first) part belongs to fundraiser `id`.
    pub fn is_fundraiser(&self, id: &i64) -> bool {
        self.single_fundraiser_id == Some(*id)
    }
}

const TX_ROW_SELECT: &str = "
    SELECT t.*,
           a.name AS account_name,
           a.kind AS account_kind,
           COALESCE((SELECT SUM(amount_cents) FROM allocations WHERE transaction_id = t.id), 0) AS allocated_cents,
           (SELECT GROUP_CONCAT(li.name, '; ') FROM allocations al
              JOIN line_items li ON li.id = al.line_item_id WHERE al.transaction_id = t.id) AS line_names,
           (SELECT MIN(line_item_id) FROM allocations WHERE transaction_id = t.id) AS single_line_id,
           (SELECT COUNT(*) FROM allocations WHERE transaction_id = t.id) AS allocation_count,
           (SELECT COUNT(*) FROM attachments WHERE transaction_id = t.id) AS receipt_count,
           -- A gaming transfer that repays general expenses relies on their receipts.
           COALESCE((SELECT MIN(li.receipt_required) FROM allocations al
              JOIN line_items li ON li.id = al.line_item_id WHERE al.transaction_id = t.id), 1)
             AND NOT EXISTS (SELECT 1 FROM transactions r WHERE r.reimbursed_by_id = t.id) AS receipt_required,
           t.gaming_event_id IS NULL AND EXISTS (SELECT 1 FROM allocations al
              JOIN line_items li ON li.id = al.line_item_id
              WHERE al.transaction_id = t.id AND li.gasr_line IN ('6', 'prize', 'event_expense')) AS needs_event,
           (SELECT GROUP_CONCAT(f.name, '; ') FROM fundraisers f
              WHERE f.id IN (SELECT fundraiser_id FROM allocations WHERE transaction_id = t.id)) AS fundraiser_names,
           (SELECT fundraiser_id FROM allocations WHERE transaction_id = t.id ORDER BY id LIMIT 1) AS single_fundraiser_id
    FROM transactions t JOIN accounts a ON a.id = t.account_id";

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub account_id: Option<i64>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub line_item_id: Option<i64>,
    pub uncategorized: bool,
    pub missing_receipt: bool,
    pub uncleared: bool,
    pub search: Option<String>,
    pub fund: Option<String>,
    pub gaming_event_id: Option<i64>,
    pub fundraiser_id: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn list(pool: &SqlitePool, f: &Filter) -> anyhow::Result<Vec<TxRow>> {
    let mut q: QueryBuilder<Sqlite> = QueryBuilder::new(format!("SELECT * FROM ({TX_ROW_SELECT}) WHERE 1 = 1"));
    if let Some(id) = f.account_id {
        q.push(" AND account_id = ").push_bind(id);
    }
    if let Some(fund) = &f.fund {
        q.push(" AND account_kind = ").push_bind(fund.clone());
    }
    if let Some(d) = f.from {
        q.push(" AND date >= ").push_bind(d);
    }
    if let Some(d) = f.to {
        q.push(" AND date <= ").push_bind(d);
    }
    if let Some(id) = f.line_item_id {
        q.push(" AND id IN (SELECT transaction_id FROM allocations WHERE line_item_id = ").push_bind(id).push(")");
    }
    if f.uncategorized {
        q.push(" AND (allocation_count = 0 OR allocated_cents != amount_cents)");
    }
    if f.missing_receipt {
        q.push(" AND amount_cents < 0 AND receipt_count = 0 AND receipt_required = 1");
    }
    if f.uncleared {
        q.push(" AND cleared = 0");
    }
    if let Some(id) = f.gaming_event_id {
        q.push(" AND gaming_event_id = ").push_bind(id);
    }
    if let Some(id) = f.fundraiser_id {
        q.push(" AND id IN (SELECT transaction_id FROM allocations WHERE fundraiser_id = ").push_bind(id).push(")");
    }
    if let Some(s) = f.search.as_ref().filter(|s| !s.trim().is_empty()) {
        let like = format!("%{}%", s.trim());
        q.push(" AND (description LIKE ")
            .push_bind(like.clone())
            .push(" OR payee LIKE ")
            .push_bind(like.clone())
            .push(" OR purpose LIKE ")
            .push_bind(like.clone())
            .push(" OR notes LIKE ")
            .push_bind(like.clone())
            .push(" OR reference LIKE ")
            .push_bind(like)
            .push(")");
    }
    q.push(" ORDER BY date DESC, id DESC");
    if let Some(limit) = f.limit {
        q.push(" LIMIT ").push_bind(limit);
    }
    Ok(q.build_query_as().fetch_all(pool).await?)
}

pub async fn row(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<TxRow>> {
    Ok(sqlx::query_as(AssertSqlSafe(format!("SELECT * FROM ({TX_ROW_SELECT}) WHERE id = ?")))
        .bind(id)
        .fetch_optional(pool)
        .await?)
}

pub async fn get(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Transaction>> {
    Ok(sqlx::query_as("SELECT * FROM transactions WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

pub async fn allocations(pool: &SqlitePool, tx_id: i64) -> anyhow::Result<Vec<Allocation>> {
    Ok(sqlx::query_as("SELECT * FROM allocations WHERE transaction_id = ? ORDER BY id")
        .bind(tx_id)
        .fetch_all(pool)
        .await?)
}

pub async fn attachments(pool: &SqlitePool, tx_id: i64) -> anyhow::Result<Vec<Attachment>> {
    Ok(sqlx::query_as("SELECT * FROM attachments WHERE transaction_id = ? ORDER BY id")
        .bind(tx_id)
        .fetch_all(pool)
        .await?)
}

/// One part of a transaction: a line item, an amount with the sign of the
/// transaction, a memo, and the fundraiser it belongs to, if any.
#[derive(Debug, Clone, Default)]
pub struct Part {
    pub line_item_id: i64,
    pub amount_cents: i64,
    pub memo: String,
    pub fundraiser_id: Option<i64>,
}

impl Part {
    /// The whole transaction on one line item.
    pub fn whole(line_item_id: i64, tx: &Transaction) -> Part {
        Part { line_item_id, amount_cents: tx.amount_cents, ..Default::default() }
    }
}

/// Replaces the allocations of a transaction. Every line item must be in the
/// account's fund, and the parts must have the sign of the transaction.
/// Fundraisers are for general-account transactions only. A part with no
/// fundraiser gets its line item's fundraiser, if the line item has one.
pub async fn set_allocations(
    pool: &SqlitePool,
    tx: &Transaction,
    account: &Account,
    parts: &[Part],
) -> anyhow::Result<()> {
    let items: HashMap<i64, LineItem> = crate::db::line_items(pool).await?.into_iter().map(|li| (li.id, li)).collect();
    let parts: Vec<Part> = parts
        .iter()
        .map(|p| {
            let from_line = items.get(&p.line_item_id).and_then(|li| li.fundraiser_id);
            Part { fundraiser_id: p.fundraiser_id.or(from_line), ..p.clone() }
        })
        .collect();
    for part in &parts {
        let li =
            items.get(&part.line_item_id).ok_or_else(|| anyhow::anyhow!("unknown line item {}", part.line_item_id))?;
        if li.fund() != account.fund() {
            anyhow::bail!(
                "\"{}\" is a {} line item. {} is a {} account. Gaming funds and general funds must stay apart.",
                li.name,
                li.fund(),
                account.name,
                account.fund()
            );
        }
        if part.amount_cents != 0 && (part.amount_cents < 0) != (tx.amount_cents < 0) {
            anyhow::bail!("each part must have the same sign as the transaction");
        }
        if let Some(fid) = part.fundraiser_id {
            if account.is_gaming() {
                anyhow::bail!(
                    "Fundraisers are for general-account money. Record a licensed raffle or bingo as a licensed gaming event."
                );
            }
            crate::db::fundraiser(pool, fid).await?.ok_or_else(|| anyhow::anyhow!("unknown fundraiser {fid}"))?;
        }
    }
    let total: i64 = parts.iter().map(|p| p.amount_cents).sum();
    if total.abs() > tx.amount_cents.abs() {
        anyhow::bail!(
            "the parts add up to {}, which is more than the transaction amount {}",
            Money(total),
            tx.amount()
        );
    }
    let mut dbtx = pool.begin().await?;
    sqlx::query("DELETE FROM allocations WHERE transaction_id = ?").bind(tx.id).execute(&mut *dbtx).await?;
    for part in parts.iter().filter(|p| p.amount_cents != 0) {
        sqlx::query(
            "INSERT INTO allocations (transaction_id, line_item_id, amount_cents, memo, fundraiser_id) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(tx.id)
        .bind(part.line_item_id)
        .bind(part.amount_cents)
        .bind(&part.memo)
        .bind(part.fundraiser_id)
        .execute(&mut *dbtx)
        .await?;
    }
    dbtx.commit().await?;
    Ok(())
}

/// After a line item gets fundraiser `new`: its parts with no fundraiser, and
/// its parts that had the line item's old fundraiser, get `new`. Parts with
/// another fundraiser keep it. Returns the number of parts changed.
pub async fn give_line_fundraiser(
    pool: &SqlitePool,
    line_item_id: i64,
    old: Option<i64>,
    new: i64,
) -> anyhow::Result<u64> {
    let li: LineItem =
        sqlx::query_as("SELECT * FROM line_items WHERE id = ?").bind(line_item_id).fetch_one(pool).await?;
    anyhow::ensure!(li.fund() == crate::models::Fund::General, "fundraisers are for general-fund line items only");
    Ok(sqlx::query(
        "UPDATE allocations SET fundraiser_id = ?
         WHERE line_item_id = ? AND (fundraiser_id IS NULL OR fundraiser_id IS ?) AND fundraiser_id IS NOT ?",
    )
    .bind(new)
    .bind(line_item_id)
    .bind(old)
    .bind(new)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Counts for the dashboard and nav badges.
#[derive(Debug, Clone, Default)]
pub struct Counts {
    pub uncategorized: i64,
    pub missing_receipts: i64,
    pub uncleared: i64,
}

pub async fn counts(pool: &SqlitePool) -> anyhow::Result<Counts> {
    let (uncategorized, missing_receipts, uncleared): (i64, i64, i64) = sqlx::query_as(AssertSqlSafe(format!(
        "SELECT
           COALESCE(SUM(allocation_count = 0 OR allocated_cents != amount_cents), 0),
           COALESCE(SUM(amount_cents < 0 AND receipt_count = 0 AND receipt_required = 1), 0),
           COALESCE(SUM(cleared = 0), 0)
         FROM ({TX_ROW_SELECT})"
    )))
    .fetch_one(pool)
    .await?;
    Ok(Counts { uncategorized, missing_receipts, uncleared })
}
