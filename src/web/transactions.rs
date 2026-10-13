use std::path::Path;

use askama::Template;
use axum::body::Body;
use axum::extract::{Multipart, Path as UrlPath};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::{Form, Query};
use chrono::NaiveDate;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{
    AppError, AppResult, Ctx, Flash, Layout, UserExt, opt_id, redirect_err, redirect_ok, render, require_edit,
};
use crate::db;
use crate::fiscal;
use crate::ledger::{self, Filter, Part, TxRow};
use crate::models::{Account, Allocation, Attachment, FiscalYear, Fundraiser, GamingEvent, LineItem, Transaction};
use crate::money::{self, Money};

#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    account: Option<String>,
    fy: Option<String>,
    month: Option<String>,
    line: Option<String>,
    fundraiser: Option<String>,
    uncategorized: Option<String>,
    missing_receipt: Option<String>,
    uncleared: Option<String>,
    q: Option<String>,
    #[serde(flatten)]
    flash: Flash,
}

#[derive(Template)]
#[template(path = "transactions.html")]
struct ListPage {
    layout: Layout,
    rows: Vec<TxRow>,
    accounts: Vec<Account>,
    years: Vec<FiscalYear>,
    line_items: Vec<LineItem>,
    fundraisers: Vec<Fundraiser>,
    can_edit: bool,
    sel_account: Option<i64>,
    sel_fy: String,
    sel_month: String,
    sel_line: Option<i64>,
    sel_fundraiser: Option<i64>,
    uncategorized: bool,
    missing_receipt: bool,
    uncleared: bool,
    search: String,
    total_in: Money,
    total_out: Money,
    months: Vec<(String, String)>,
}

impl ListPage {
    fn is_account(&self, id: &i64) -> bool {
        self.sel_account == Some(*id)
    }

    fn is_line(&self, id: &i64) -> bool {
        self.sel_line == Some(*id)
    }

    fn is_fundraiser(&self, id: &i64) -> bool {
        self.sel_fundraiser == Some(*id)
    }
}

fn flag(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "on" | "true"))
}

pub async fn list(state: Ctx, user: UserExt, Query(q): Query<ListQuery>) -> AppResult<Response> {
    let pool = &state.pool;
    let years = fiscal::list(pool).await?;
    let current = fiscal::current(pool).await?;
    let any_filter = flag(&q.uncategorized) || flag(&q.missing_receipt) || flag(&q.uncleared);
    // Default to the current year, unless a "to do" filter is on: then show all years.
    let sel_fy = q.fy.clone().unwrap_or_else(|| if any_filter { "all".into() } else { current.id.to_string() });
    let fy = sel_fy.parse::<i64>().ok().and_then(|id| years.iter().find(|y| y.id == id).cloned());
    let mut filter = Filter {
        account_id: opt_id(&q.account),
        line_item_id: opt_id(&q.line),
        fundraiser_id: opt_id(&q.fundraiser),
        uncategorized: flag(&q.uncategorized),
        missing_receipt: flag(&q.missing_receipt),
        uncleared: flag(&q.uncleared),
        search: q.q.clone(),
        ..Default::default()
    };
    let mut months = Vec::new();
    if let Some(fy) = &fy {
        filter.from = Some(fy.start_date);
        filter.to = Some(fy.end_date);
        for m in fiscal::months(fy.start_date, fy.end_date) {
            months.push((m.format("%Y-%m").to_string(), m.format("%B %Y").to_string()));
        }
    }
    let sel_month = q.month.clone().unwrap_or_default();
    if let Ok(m) = NaiveDate::parse_from_str(&format!("{sel_month}-01"), "%Y-%m-%d") {
        filter.from = Some(m);
        filter.to = Some(fiscal::month_end(m));
    }
    let rows = ledger::list(pool, &filter).await?;
    let total_in = rows.iter().filter(|r| r.tx.amount_cents > 0).map(|r| r.tx.amount()).sum();
    let total_out = rows.iter().filter(|r| r.tx.amount_cents < 0).map(|r| r.tx.amount()).sum();
    let layout = Layout::new(&state, &user, "transactions", "Transactions", &q.flash).await?;
    render(ListPage {
        can_edit: layout.can_edit,
        layout,
        rows,
        accounts: db::accounts(pool).await?,
        years,
        line_items: db::line_items(pool).await?,
        fundraisers: db::fundraisers(pool).await?,
        sel_account: filter.account_id,
        sel_fy,
        sel_month,
        sel_line: filter.line_item_id,
        sel_fundraiser: filter.fundraiser_id,
        uncategorized: filter.uncategorized,
        missing_receipt: filter.missing_receipt,
        uncleared: filter.uncleared,
        search: q.q.unwrap_or_default(),
        total_in,
        total_out,
        months,
    })
}

#[derive(Template)]
#[template(path = "partials/tx_row.html")]
struct RowPartial {
    r: TxRow,
    line_items: Vec<LineItem>,
    fundraisers: Vec<Fundraiser>,
    can_edit: bool,
}

/// One row of the list, after an inline change.
async fn row_partial(pool: &sqlx::SqlitePool, id: i64) -> AppResult<Response> {
    render(RowPartial {
        r: ledger::row(pool, id).await?.ok_or(AppError::NotFound)?,
        line_items: db::line_items(pool).await?,
        fundraisers: db::fundraisers(pool).await?,
        can_edit: true,
    })
}

#[derive(Deserialize)]
pub struct AssignForm {
    line_item_id: Option<String>,
}

/// Inline line item change from the list. Returns the updated row.
pub async fn assign(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(form): Form<AssignForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let tx = ledger::get(pool, id).await?.ok_or(AppError::NotFound)?;
    let account = db::account(pool, tx.account_id).await?.ok_or(AppError::NotFound)?;
    // Changing the line item keeps the memo of a single part, and the
    // fundraiser that someone chose for it. A fundraiser that came from the
    // old line item goes with it, so the new line item's fundraiser (if any)
    // takes its place.
    let old = ledger::allocations(pool, id).await?;
    let parts = match opt_id(&form.line_item_id) {
        Some(li) => {
            let mut part = Part::whole(li, &tx);
            if let [only] = old.as_slice() {
                let from_line: Option<i64> = sqlx::query_scalar("SELECT fundraiser_id FROM line_items WHERE id = ?")
                    .bind(only.line_item_id)
                    .fetch_optional(pool)
                    .await?
                    .flatten();
                part.memo = only.memo.clone();
                part.fundraiser_id = only.fundraiser_id.filter(|f| from_line != Some(*f));
            }
            vec![part]
        }
        None => vec![],
    };
    ledger::set_allocations(pool, &tx, &account, &parts).await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    row_partial(pool, id).await
}

#[derive(Deserialize)]
pub struct FundraiserForm {
    fundraiser_id: Option<String>,
}

/// Inline fundraiser change from the list, for a transaction with one part.
/// Returns the updated row. A split transaction is edited on its own page.
pub async fn set_fundraiser(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(form): Form<FundraiserForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let tx = ledger::get(pool, id).await?.ok_or(AppError::NotFound)?;
    let account = db::account(pool, tx.account_id).await?.ok_or(AppError::NotFound)?;
    let allocs = ledger::allocations(pool, id).await?;
    let [only] = allocs.as_slice() else {
        return Err(AppError::BadRequest("Choose a line item first. For a split, open the transaction.".into()));
    };
    let part = Part {
        line_item_id: only.line_item_id,
        amount_cents: only.amount_cents,
        memo: only.memo.clone(),
        fundraiser_id: opt_id(&form.fundraiser_id),
    };
    ledger::set_allocations(pool, &tx, &account, &[part]).await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    row_partial(pool, id).await
}

#[derive(Template)]
#[template(path = "transaction.html")]
struct DetailPage {
    layout: Layout,
    r: TxRow,
    account: Account,
    fy: Option<FiscalYear>,
    allocations: Vec<(Allocation, String)>,
    line_items: Vec<LineItem>,
    attachments: Vec<Attachment>,
    rule_pattern: String,
    /// For a gaming payment: general-account expenses it could repay.
    reimburse_candidates: Vec<(Transaction, bool)>,
    reimbursed_total: Money,
    /// For a general expense: the gaming transfer that repaid it.
    reimbursed_by: Option<Transaction>,
    /// For a gaming transaction: the licensed gaming events it can belong to.
    events: Vec<GamingEvent>,
    /// For a general transaction: the fundraisers its parts can belong to.
    fundraisers: Vec<Fundraiser>,
}

impl DetailPage {
    fn is_event(&self, id: &i64) -> bool {
        self.r.tx.gaming_event_id == Some(*id)
    }

    /// True when any part of the transaction belongs to fundraiser `id`.
    fn in_fundraiser(&self, id: &i64) -> bool {
        self.allocations.iter().any(|(a, _)| a.fundraiser_id == Some(*id))
    }

    fn line_fundraiser(&self, id: &i64) -> String {
        line_fundraiser(&self.line_items, &self.fundraisers, *id)
    }

    /// The words for "no fundraiser chosen" on a part of line item `id`.
    fn none_label(&self, id: &i64) -> String {
        match self.line_fundraiser(id) {
            name if name.is_empty() => "No fundraiser".to_string(),
            name => format!("{name} (from the line item)"),
        }
    }

    /// True when part `a` has its line item's fundraiser. The form then shows
    /// "from the line item", so the fundraiser follows a change of line item.
    fn has_line_fundraiser(&self, a: &Allocation) -> bool {
        a.fundraiser_id.is_some()
            && self.line_items.iter().any(|li| li.id == a.line_item_id && li.fundraiser_id == a.fundraiser_id)
    }
}

pub async fn detail(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Query(flash): Query<Flash>,
) -> AppResult<Response> {
    let pool = &state.pool;
    let r = ledger::row(pool, id).await?.ok_or(AppError::NotFound)?;
    let account = db::account(pool, r.tx.account_id).await?.ok_or(AppError::NotFound)?;
    let items = db::line_items(pool).await?;
    let allocations = ledger::allocations(pool, id)
        .await?
        .into_iter()
        .map(|a| {
            let name = items.iter().find(|li| li.id == a.line_item_id).map(|li| li.name.clone()).unwrap_or_default();
            (a, name)
        })
        .collect();
    let fy = fiscal::containing(pool, r.tx.date).await?;

    let mut reimburse_candidates = Vec::new();
    if account.is_gaming()
        && r.tx.amount_cents < 0
        && let Some(fy) = &fy
    {
        let txs: Vec<Transaction> = sqlx::query_as(
            "SELECT t.* FROM transactions t JOIN accounts a ON a.id = t.account_id
             WHERE a.kind = 'general' AND t.amount_cents < 0 AND t.date >= ? AND t.date <= ?
               AND (t.reimbursed_by_id IS NULL OR t.reimbursed_by_id = ?)
             ORDER BY t.date DESC, t.id DESC",
        )
        .bind(fy.start_date)
        .bind(r.tx.date)
        .bind(id)
        .fetch_all(pool)
        .await?;
        reimburse_candidates = txs
            .into_iter()
            .map(|t| {
                let linked = t.reimbursed_by_id == Some(id);
                (t, linked)
            })
            .collect();
    }
    let reimbursed_total = reimburse_candidates.iter().filter(|(_, l)| *l).map(|(t, _)| -t.amount()).sum();
    let reimbursed_by = match r.tx.reimbursed_by_id {
        Some(rid) => ledger::get(pool, rid).await?,
        None => None,
    };
    let rule_pattern = suggest_pattern(&r.tx.description);
    let title = r.tx.display_name().to_string();
    let events = if account.is_gaming() { db::gaming_events(pool).await? } else { Vec::new() };
    let fundraisers = if account.is_gaming() { Vec::new() } else { db::fundraisers(pool).await? };
    render(DetailPage {
        layout: Layout::new(&state, &user, "transactions", title, &flash).await?,
        events,
        fundraisers,
        line_items: items.into_iter().filter(|li| li.fund() == account.fund()).collect(),
        attachments: ledger::attachments(pool, id).await?,
        r,
        account,
        fy,
        allocations,
        rule_pattern,
        reimburse_candidates,
        reimbursed_total,
        reimbursed_by,
    })
}

/// The leading words of a bank description, without reference numbers.
fn suggest_pattern(description: &str) -> String {
    description
        .split_whitespace()
        .take_while(|w| !w.chars().any(|c| c.is_ascii_digit()) || w.len() < 3)
        .take(3)
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Deserialize)]
pub struct UpdateForm {
    payee: String,
    purpose: String,
    reference: String,
    notes: String,
    /// Only for gaming-account transactions.
    gaming_event_id: Option<String>,
    // Only for manual entries:
    date: Option<String>,
    description: Option<String>,
    amount: Option<String>,
    cleared: Option<String>,
}

pub async fn update(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<UpdateForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let tx = ledger::get(pool, id).await?.ok_or(AppError::NotFound)?;
    let back = format!("/transactions/{id}");
    let account = db::account(pool, tx.account_id).await?.ok_or(AppError::NotFound)?;
    // Licensed gaming event money moves through the gaming account only.
    let event_id = if account.is_gaming() { opt_id(&f.gaming_event_id) } else { None };
    if let Some(eid) = event_id {
        db::gaming_event(pool, eid).await?.ok_or(AppError::NotFound)?;
    }
    sqlx::query(
        "UPDATE transactions SET payee = ?, purpose = ?, reference = ?, notes = ?, gaming_event_id = ? WHERE id = ?",
    )
    .bind(f.payee.trim())
    .bind(f.purpose.trim())
    .bind(f.reference.trim())
    .bind(f.notes.trim())
    .bind(event_id)
    .bind(id)
    .execute(pool)
    .await?;
    if tx.source == "manual" {
        let Some(date) = f.date.as_deref().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) else {
            return Ok(redirect_err(&back, "Enter a valid date."));
        };
        let Some(amount) = f.amount.as_deref().and_then(money::parse).filter(|a| *a != 0) else {
            return Ok(redirect_err(&back, "Enter an amount, such as -45.00 for money out."));
        };
        let allocated: i64 =
            sqlx::query_scalar("SELECT COALESCE(SUM(amount_cents), 0) FROM allocations WHERE transaction_id = ?")
                .bind(id)
                .fetch_one(pool)
                .await?;
        if amount != tx.amount_cents && allocated != 0 {
            // Keep a single allocation in step with the new amount; clear splits.
            let allocs = ledger::allocations(pool, id).await?;
            let mut moved = tx.clone();
            moved.amount_cents = amount;
            let parts = match allocs.as_slice() {
                [only] => vec![Part {
                    line_item_id: only.line_item_id,
                    amount_cents: amount,
                    memo: only.memo.clone(),
                    fundraiser_id: only.fundraiser_id,
                }],
                _ => vec![],
            };
            ledger::set_allocations(pool, &moved, &account, &parts)
                .await
                .map_err(|e| AppError::BadRequest(e.to_string()))?;
        }
        sqlx::query("UPDATE transactions SET date = ?, description = ?, amount_cents = ?, cleared = ? WHERE id = ?")
            .bind(date)
            .bind(f.description.as_deref().unwrap_or("").trim())
            .bind(amount)
            .bind(f.cleared.is_some())
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(redirect_ok(&back, "Saved."))
}

#[derive(Deserialize)]
pub struct SplitForm {
    #[serde(default)]
    line_item_id: Vec<String>,
    #[serde(default)]
    amount: Vec<String>,
    #[serde(default)]
    memo: Vec<String>,
    /// One per row, blank for none. Absent when the PAC has no fundraisers.
    #[serde(default)]
    fundraiser_id: Vec<String>,
}

pub async fn set_allocations(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<SplitForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let tx = ledger::get(pool, id).await?.ok_or(AppError::NotFound)?;
    let account = db::account(pool, tx.account_id).await?.ok_or(AppError::NotFound)?;
    let back = format!("/transactions/{id}");
    let sign = if tx.amount_cents < 0 { -1 } else { 1 };
    let mut parts = Vec::new();
    for (i, li) in f.line_item_id.iter().enumerate() {
        let Ok(li) = li.trim().parse::<i64>() else { continue };
        let raw = f.amount.get(i).map(String::as_str).unwrap_or("").trim();
        // A blank amount on a single line means "the whole amount".
        let amount = if raw.is_empty() {
            let used: i64 = parts.iter().map(|p: &Part| p.amount_cents).sum();
            tx.amount_cents - used
        } else {
            match money::parse(raw) {
                Some(a) => sign * a.abs(),
                None => return Ok(redirect_err(&back, &format!("\"{raw}\" is not an amount."))),
            }
        };
        parts.push(Part {
            line_item_id: li,
            amount_cents: amount,
            memo: f.memo.get(i).cloned().unwrap_or_default().trim().to_string(),
            fundraiser_id: f.fundraiser_id.get(i).and_then(|v| v.trim().parse().ok()),
        });
    }
    match ledger::set_allocations(pool, &tx, &account, &parts).await {
        Ok(()) => {
            let total: i64 = parts.iter().map(|p| p.amount_cents).sum();
            if total == tx.amount_cents || parts.is_empty() {
                Ok(redirect_ok(&back, "Line items saved."))
            } else {
                Ok(redirect_ok(&back, &format!("Saved. {} is still unassigned.", Money(tx.amount_cents - total).abs())))
            }
        }
        Err(e) => Ok(redirect_err(&back, &e.to_string())),
    }
}

#[derive(Template)]
#[template(path = "transaction_new.html")]
struct NewPage {
    layout: Layout,
    accounts: Vec<Account>,
    line_items: Vec<LineItem>,
    events: Vec<GamingEvent>,
    fundraisers: Vec<Fundraiser>,
    today: NaiveDate,
}

impl NewPage {
    fn line_fundraiser(&self, id: &i64) -> String {
        line_fundraiser(&self.line_items, &self.fundraisers, *id)
    }
}

/// The name of line item `id`'s fundraiser, or "" when it has none.
fn line_fundraiser(line_items: &[LineItem], fundraisers: &[Fundraiser], id: i64) -> String {
    let fid = line_items.iter().find(|li| li.id == id).and_then(|li| li.fundraiser_id);
    fundraisers.iter().find(|f| Some(f.id) == fid).map(|f| f.name.clone()).unwrap_or_default()
}

pub async fn new_form(state: Ctx, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    render(NewPage {
        layout: Layout::new(&state, &user, "transactions", "Add transaction", &flash).await?,
        accounts: db::accounts(pool).await?.into_iter().filter(|a| a.active).collect(),
        line_items: db::line_items(pool).await?.into_iter().filter(|li| li.active).collect(),
        events: db::gaming_events(pool).await?,
        fundraisers: db::fundraisers(pool).await?.into_iter().filter(|f| f.active).collect(),
        today: chrono::Local::now().date_naive(),
    })
}

#[derive(Deserialize)]
pub struct NewForm {
    account_id: i64,
    date: String,
    direction: String,
    amount: String,
    payee: String,
    purpose: String,
    reference: String,
    line_item_id: Option<String>,
    gaming_event_id: Option<String>,
    fundraiser_id: Option<String>,
    notes: String,
    cleared: Option<String>,
}

pub async fn create(state: Ctx, user: UserExt, Form(f): Form<NewForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let back = "/transactions/new";
    let account = db::account(pool, f.account_id).await?.ok_or(AppError::NotFound)?;
    let Ok(date) = NaiveDate::parse_from_str(&f.date, "%Y-%m-%d") else {
        return Ok(redirect_err(back, "Enter a valid date."));
    };
    if date < account.opening_date {
        return Ok(redirect_err(back, "That date is before the account's opening balance date."));
    }
    let Some(amount) = money::parse(&f.amount).map(|a| a.abs()).filter(|a| *a != 0) else {
        return Ok(redirect_err(back, "Enter an amount."));
    };
    let amount = if f.direction == "in" { amount } else { -amount };
    if f.payee.trim().is_empty() {
        return Ok(redirect_err(back, "Enter who was paid or who paid."));
    }
    let id = sqlx::query(
        "INSERT INTO transactions (account_id, date, description, payee, amount_cents, reference, purpose, source, cleared,
           notes, gaming_event_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'manual', ?, ?, ?, ?)",
    )
    .bind(account.id)
    .bind(date)
    .bind(f.payee.trim())
    .bind(f.payee.trim())
    .bind(amount)
    .bind(f.reference.trim())
    .bind(f.purpose.trim())
    .bind(f.cleared.is_some())
    .bind(f.notes.trim())
    .bind(if account.is_gaming() { opt_id(&f.gaming_event_id) } else { None })
    .bind(db::now())
    .execute(pool)
    .await?
    .last_insert_rowid();
    if let Some(li) = opt_id(&f.line_item_id) {
        let tx = ledger::get(pool, id).await?.ok_or(AppError::NotFound)?;
        let fundraiser_id = if account.is_gaming() { None } else { opt_id(&f.fundraiser_id) };
        let part = Part { fundraiser_id, ..Part::whole(li, &tx) };
        if let Err(e) = ledger::set_allocations(pool, &tx, &account, &[part]).await {
            return Ok(redirect_err(&format!("/transactions/{id}"), &e.to_string()));
        }
    }
    Ok(redirect_ok(&format!("/transactions/{id}"), "Transaction added. Attach the receipt below."))
}

pub async fn delete(state: Ctx, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let attachments = ledger::attachments(pool, id).await?;
    sqlx::query("DELETE FROM transactions WHERE id = ?").bind(id).execute(pool).await?;
    for a in attachments {
        let _ = tokio::fs::remove_file(state.data_dir.join("receipts").join(&a.stored_name)).await;
    }
    Ok(redirect_ok("/transactions", "Transaction deleted."))
}

fn safe_filename(name: &str) -> String {
    let cleaned: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' }).collect();
    let trimmed = cleaned.trim_matches('_');
    let short: String = trimmed.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
    if short.is_empty() { "file".into() } else { short }
}

pub async fn upload(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    mut multipart: Multipart,
) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(refused) = super::no_uploads_in_demo(&state, &format!("/transactions/{id}")) {
        return Ok(refused);
    }
    let pool = &state.pool;
    let tx = ledger::get(pool, id).await?.ok_or(AppError::NotFound)?;
    let back = format!("/transactions/{id}");
    let mut saved = 0;
    while let Some(field) = multipart.next_field().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
        let Some(filename) = field.file_name().map(str::to_string) else { continue };
        if filename.is_empty() {
            continue;
        }
        let content_type = field
            .content_type()
            .map(str::to_string)
            .filter(|c| c != "application/octet-stream")
            .unwrap_or_else(|| mime_guess::from_path(&filename).first_or_octet_stream().to_string());
        let bytes = field.bytes().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
        if bytes.is_empty() {
            continue;
        }
        // On a hosted server, each PAC has a storage limit for receipts.
        if let Some(h) = &state.hosted {
            let used = crate::hosted::Hosted::storage_used(pool).await?;
            if used + bytes.len() as i64 > h.hosted.quota_bytes {
                return Ok(redirect_err(
                    &back,
                    &format!(
                        "This PAC has used its {} MB of receipt storage. Remove old files, or ask us for more room.",
                        h.hosted.quota_bytes / (1024 * 1024)
                    ),
                ));
            }
        }
        let sha = hex::encode(Sha256::digest(&bytes));
        let stored_name = format!("{}/{}-{}-{}", tx.date.format("%Y"), id, &sha[..8], safe_filename(&filename));
        let path = state.data_dir.join("receipts").join(&stored_name);
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(&path, &bytes).await?;
        sqlx::query(
            "INSERT INTO attachments (transaction_id, filename, stored_name, content_type, size_bytes, sha256, uploaded_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(&filename)
        .bind(&stored_name)
        .bind(&content_type)
        .bind(bytes.len() as i64)
        .bind(&sha)
        .bind(db::now())
        .execute(pool)
        .await?;
        saved += 1;
    }
    if saved == 0 {
        return Ok(redirect_err(&back, "Choose a file to attach."));
    }
    Ok(redirect_ok(&back, &format!("{saved} file(s) attached.")))
}

/// Serves a receipt. Only images and PDFs open in the browser; anything else
/// downloads, so an uploaded web page cannot run in the app.
pub async fn attachment(state: Ctx, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    let a: Attachment = sqlx::query_as("SELECT * FROM attachments WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let path = state.data_dir.join("receipts").join(&a.stored_name);
    let bytes = tokio::fs::read(&path).await.map_err(|_| AppError::NotFound)?;
    let inline = (a.content_type.starts_with("image/") && a.content_type != "image/svg+xml")
        || a.content_type == "application/pdf";
    let content_type = if inline { a.content_type.clone() } else { "application/octet-stream".into() };
    let disposition =
        format!("{}; filename=\"{}\"", if inline { "inline" } else { "attachment" }, safe_filename(&a.filename));
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, disposition),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        ],
        Body::from(bytes),
    )
        .into_response())
}

pub async fn delete_attachment(state: Ctx, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    let a: Attachment = sqlx::query_as("SELECT * FROM attachments WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    sqlx::query("DELETE FROM attachments WHERE id = ?").bind(id).execute(&state.pool).await?;
    let _ = tokio::fs::remove_file(Path::new(&state.data_dir).join("receipts").join(&a.stored_name)).await;
    Ok(redirect_ok(&format!("/transactions/{}", a.transaction_id), "Receipt removed."))
}

#[derive(Deserialize)]
pub struct RuleForm {
    pattern: String,
    line_item_id: i64,
    payee: String,
    fundraiser_id: Option<String>,
    this_account: Option<String>,
    apply_now: Option<String>,
}

pub async fn make_rule(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<RuleForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let tx = ledger::get(pool, id).await?.ok_or(AppError::NotFound)?;
    let back = format!("/transactions/{id}");
    let pattern = f.pattern.trim();
    if pattern.len() < 3 {
        return Ok(redirect_err(&back, "The rule text must be at least 3 characters."));
    }
    let account_id = f.this_account.is_some().then_some(tx.account_id);
    let fundraiser_id = match super::accounts::rule_fundraiser(pool, f.line_item_id, &f.fundraiser_id).await? {
        Ok(id) => id,
        Err(e) => return Ok(redirect_err(&back, &e)),
    };
    sqlx::query("INSERT INTO rules (pattern, account_id, line_item_id, payee, fundraiser_id) VALUES (?, ?, ?, ?, ?)")
        .bind(pattern)
        .bind(account_id)
        .bind(f.line_item_id)
        .bind(f.payee.trim())
        .bind(fundraiser_id)
        .execute(pool)
        .await?;
    let applied = if f.apply_now.is_some() { apply_rules_to_uncategorized(pool).await? } else { 0 };
    Ok(redirect_ok(&back, &format!("Rule saved. It assigned {applied} other uncategorized transaction(s).")))
}

/// Runs the rules over transactions that have no allocations yet.
pub async fn apply_rules_to_uncategorized(pool: &sqlx::SqlitePool) -> anyhow::Result<usize> {
    let mut applied = 0;
    for account in db::accounts(pool).await? {
        let rules = crate::import::rules_for(pool, &account).await?;
        if rules.is_empty() {
            continue;
        }
        let txs: Vec<Transaction> = sqlx::query_as(
            "SELECT * FROM transactions WHERE account_id = ? AND id NOT IN (SELECT transaction_id FROM allocations)",
        )
        .bind(account.id)
        .fetch_all(pool)
        .await?;
        for tx in txs {
            if let Some((rule, li)) = crate::import::match_rule(&rules, &tx.description) {
                let part = Part { fundraiser_id: rule.fundraiser_id, ..Part::whole(li.id, &tx) };
                ledger::set_allocations(pool, &tx, &account, &[part]).await?;
                if tx.payee.is_empty() && !rule.payee.is_empty() {
                    sqlx::query("UPDATE transactions SET payee = ? WHERE id = ?")
                        .bind(&rule.payee)
                        .bind(tx.id)
                        .execute(pool)
                        .await?;
                }
                applied += 1;
            }
        }
    }
    Ok(applied)
}

#[derive(Deserialize)]
pub struct ReimburseForm {
    #[serde(default)]
    expense_id: Vec<i64>,
}

pub async fn set_reimbursements(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<ReimburseForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let tx = ledger::get(pool, id).await?.ok_or(AppError::NotFound)?;
    let account = db::account(pool, tx.account_id).await?.ok_or(AppError::NotFound)?;
    let back = format!("/transactions/{id}");
    if !account.is_gaming() || tx.amount_cents >= 0 {
        return Ok(redirect_err(&back, "Only a payment out of a gaming account can repay general-account expenses."));
    }
    let mut dbtx = pool.begin().await?;
    sqlx::query("UPDATE transactions SET reimbursed_by_id = NULL WHERE reimbursed_by_id = ?")
        .bind(id)
        .execute(&mut *dbtx)
        .await?;
    for eid in &f.expense_id {
        sqlx::query(
            "UPDATE transactions SET reimbursed_by_id = ?
             WHERE id = ? AND amount_cents < 0 AND account_id IN (SELECT id FROM accounts WHERE kind = 'general')",
        )
        .bind(id)
        .bind(eid)
        .execute(&mut *dbtx)
        .await?;
    }
    dbtx.commit().await?;
    let total: i64 =
        sqlx::query_scalar("SELECT COALESCE(SUM(-amount_cents), 0) FROM transactions WHERE reimbursed_by_id = ?")
            .bind(id)
            .fetch_one(pool)
            .await?;
    if total == -tx.amount_cents || f.expense_id.is_empty() {
        Ok(redirect_ok(&back, "Reimbursement summary saved."))
    } else {
        Ok(redirect_err(
            &back,
            &format!(
                "Saved, but the expenses add up to {} and the transfer is {}. They must match exactly.",
                Money(total),
                tx.amount().abs()
            ),
        ))
    }
}
