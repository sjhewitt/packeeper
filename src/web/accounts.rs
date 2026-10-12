//! Set-up pages: bank accounts, line items, and assignment rules.

use askama::Template;
use axum::extract::{Path as UrlPath, State};
use axum::response::Response;
use axum_extra::extract::{Form, Query};
use chrono::NaiveDate;
use serde::Deserialize;

use super::{
    AppError, AppResult, AppState, Flash, Layout, UserExt, opt_id, redirect_err, redirect_ok, render, require_edit,
};
use crate::db;
use crate::models::{Account, GASR_EXPENSE_LINES, GASR_INCOME_LINES, LineItem, Rule, valid_gasr_line};
use crate::money::{self, Money};

#[derive(Template)]
#[template(path = "accounts.html")]
struct AccountsPage {
    layout: Layout,
    accounts: Vec<(Account, Money)>,
    today: NaiveDate,
}

pub async fn list(State(state): State<AppState>, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    let today = chrono::Local::now().date_naive();
    let mut accounts = Vec::new();
    for a in db::accounts(&state.pool).await? {
        let bal = db::balance_at(&state.pool, &a, today, false).await?;
        accounts.push((a, Money(bal)));
    }
    render(AccountsPage { layout: Layout::new(&state, &user, "settings", "Accounts", &flash).await?, accounts, today })
}

#[derive(Deserialize)]
pub struct AccountForm {
    name: String,
    kind: Option<String>,
    institution: String,
    account_number: String,
    opening_date: String,
    opening_balance: String,
    notes: String,
    active: Option<String>,
    reset_mapping: Option<String>,
}

struct ParsedAccount {
    date: NaiveDate,
    balance: i64,
}

fn parse_account(f: &AccountForm) -> Result<ParsedAccount, String> {
    if f.name.trim().is_empty() {
        return Err("Enter a name for the account.".into());
    }
    let date = NaiveDate::parse_from_str(f.opening_date.trim(), "%Y-%m-%d")
        .map_err(|_| "Enter the opening balance date.".to_string())?;
    let balance = if f.opening_balance.trim().is_empty() {
        0
    } else {
        money::parse(&f.opening_balance).ok_or_else(|| format!("\"{}\" is not an amount.", f.opening_balance))?
    };
    Ok(ParsedAccount { date, balance })
}

pub async fn create(State(state): State<AppState>, user: UserExt, Form(f): Form<AccountForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let p = match parse_account(&f) {
        Ok(p) => p,
        Err(e) => return Ok(redirect_err("/accounts", &e)),
    };
    let kind = match f.kind.as_deref() {
        Some("gaming") => "gaming",
        _ => "general",
    };
    sqlx::query(
        "INSERT INTO accounts (name, kind, institution, account_number, opening_date, opening_balance_cents, notes)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(f.name.trim())
    .bind(kind)
    .bind(f.institution.trim())
    .bind(f.account_number.trim())
    .bind(p.date)
    .bind(p.balance)
    .bind(f.notes.trim())
    .execute(&state.pool)
    .await?;
    crate::fiscal::ensure(&state.pool, p.date).await?;
    Ok(redirect_ok("/accounts", &format!("Account \"{}\" added. Next, import a bank file for it.", f.name.trim())))
}

#[derive(Template)]
#[template(path = "account_edit.html")]
struct AccountEditPage {
    layout: Layout,
    a: Account,
    tx_count: i64,
}

pub async fn edit(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Query(flash): Query<Flash>,
) -> AppResult<Response> {
    let a = db::account(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    let tx_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE account_id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    render(AccountEditPage {
        layout: Layout::new(&state, &user, "settings", a.name.clone(), &flash).await?,
        a,
        tx_count,
    })
}

pub async fn update(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<AccountForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let back = format!("/accounts/{id}");
    let a = db::account(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    let p = match parse_account(&f) {
        Ok(p) => p,
        Err(e) => return Ok(redirect_err(&back, &e)),
    };
    // The kind decides which fund the transactions belong to. It cannot change
    // once transactions exist, or gaming and general money would mix.
    let tx_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE account_id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    let kind = match (f.kind.as_deref(), tx_count) {
        (Some(k @ ("general" | "gaming")), 0) => k.to_string(),
        _ => a.kind.clone(),
    };
    sqlx::query(
        "UPDATE accounts SET name = ?, kind = ?, institution = ?, account_number = ?, opening_date = ?,
           opening_balance_cents = ?, notes = ?, active = ?,
           csv_mapping = CASE WHEN ? THEN NULL ELSE csv_mapping END
         WHERE id = ?",
    )
    .bind(f.name.trim())
    .bind(kind)
    .bind(f.institution.trim())
    .bind(f.account_number.trim())
    .bind(p.date)
    .bind(p.balance)
    .bind(f.notes.trim())
    .bind(f.active.is_some())
    .bind(f.reset_mapping.is_some())
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(redirect_ok("/accounts", "Account saved."))
}

#[derive(Template)]
#[template(path = "line_items.html")]
struct LineItemsPage {
    layout: Layout,
    items: Vec<(LineItem, i64)>,
    income_lines: Vec<(&'static str, &'static str)>,
    expense_lines: Vec<(&'static str, &'static str)>,
}

pub async fn line_items(
    State(state): State<AppState>,
    user: UserExt,
    Query(flash): Query<Flash>,
) -> AppResult<Response> {
    let counts: Vec<(i64, i64)> =
        sqlx::query_as("SELECT line_item_id, COUNT(*) FROM allocations GROUP BY line_item_id")
            .fetch_all(&state.pool)
            .await?;
    let items = db::line_items(&state.pool)
        .await?
        .into_iter()
        .map(|li| {
            let n = counts.iter().find(|(id, _)| *id == li.id).map(|(_, n)| *n).unwrap_or(0);
            (li, n)
        })
        .collect();
    render(LineItemsPage {
        layout: Layout::new(&state, &user, "settings", "Line items", &flash).await?,
        items,
        income_lines: GASR_INCOME_LINES.to_vec(),
        expense_lines: GASR_EXPENSE_LINES.to_vec(),
    })
}

#[derive(Deserialize)]
pub struct LineItemForm {
    name: String,
    fund: Option<String>,
    kind: Option<String>,
    gasr_line: Option<String>,
    description: String,
    sort_order: Option<String>,
    active: Option<String>,
    receipt_required: Option<String>,
}

pub async fn create_line_item(
    State(state): State<AppState>,
    user: UserExt,
    Form(f): Form<LineItemForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let name = f.name.trim();
    if name.is_empty() {
        return Ok(redirect_err("/line-items", "Enter a name."));
    }
    let fund = if f.fund.as_deref() == Some("gaming") { "gaming" } else { "general" };
    let kind = match f.kind.as_deref() {
        Some("income") => "income",
        Some("transfer") => "transfer",
        _ => "expense",
    };
    let gasr = valid_gasr_line(fund, kind, f.gasr_line.as_deref());
    let next_sort: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), 0) + 10 FROM line_items").fetch_one(&state.pool).await?;
    let result = sqlx::query(
        "INSERT INTO line_items (name, fund, kind, gasr_line, receipt_required, description, sort_order) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(name)
    .bind(fund)
    .bind(kind)
    .bind(gasr)
    .bind(kind == "expense")
    .bind(f.description.trim())
    .bind(next_sort)
    .execute(&state.pool)
    .await;
    match result {
        Ok(_) => Ok(redirect_ok("/line-items", &format!("Line item \"{name}\" added."))),
        Err(_) => Ok(redirect_err("/line-items", "That fund already has a line item with that name.")),
    }
}

#[derive(Template)]
#[template(path = "line_item_edit.html")]
struct LineItemEditPage {
    layout: Layout,
    li: LineItem,
    used: i64,
    gasr_lines: Vec<(&'static str, &'static str)>,
}

pub async fn edit_line_item(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Query(flash): Query<Flash>,
) -> AppResult<Response> {
    let li: LineItem = sqlx::query_as("SELECT * FROM line_items WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM allocations WHERE line_item_id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    let gasr_lines = match (li.fund.as_str(), li.kind.as_str()) {
        ("gaming", "income") => GASR_INCOME_LINES.to_vec(),
        ("gaming", "expense") => GASR_EXPENSE_LINES.to_vec(),
        _ => Vec::new(),
    };
    render(LineItemEditPage {
        layout: Layout::new(&state, &user, "settings", li.name.clone(), &flash).await?,
        li,
        used,
        gasr_lines,
    })
}

pub async fn update_line_item(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<LineItemForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let back = format!("/line-items/{id}");
    let li: LineItem = sqlx::query_as("SELECT * FROM line_items WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    if f.name.trim().is_empty() {
        return Ok(redirect_err(&back, "Enter a name."));
    }
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM allocations WHERE line_item_id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    // Fund and kind are fixed once transactions use the line item.
    let kind = match (f.kind.as_deref(), used) {
        (Some(k @ ("income" | "expense" | "transfer")), 0) => k.to_string(),
        _ => li.kind.clone(),
    };
    let gasr = valid_gasr_line(&li.fund, &kind, f.gasr_line.as_deref());
    let sort = opt_id(&f.sort_order).unwrap_or(li.sort_order);
    let result = sqlx::query(
        "UPDATE line_items SET name = ?, kind = ?, gasr_line = ?, description = ?, sort_order = ?, active = ?, receipt_required = ? WHERE id = ?",
    )
    .bind(f.name.trim())
    .bind(&kind)
    .bind(gasr)
    .bind(f.description.trim())
    .bind(sort)
    .bind(f.active.is_some())
    .bind(f.receipt_required.is_some())
    .bind(id)
    .execute(&state.pool)
    .await;
    match result {
        Ok(_) => Ok(redirect_ok("/line-items", "Line item saved.")),
        Err(_) => Ok(redirect_err(&back, "That fund already has a line item with that name.")),
    }
}

#[derive(Template)]
#[template(path = "rules.html")]
struct RulesPage {
    layout: Layout,
    rules: Vec<(Rule, String, String)>,
    accounts: Vec<Account>,
    line_items: Vec<LineItem>,
}

pub async fn rules(State(state): State<AppState>, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    let rules: Vec<Rule> = sqlx::query_as("SELECT * FROM rules ORDER BY pattern").fetch_all(&state.pool).await?;
    let accounts = db::accounts(&state.pool).await?;
    let line_items = db::line_items(&state.pool).await?;
    let rules = rules
        .into_iter()
        .map(|r| {
            let acct = r
                .account_id
                .and_then(|id| accounts.iter().find(|a| a.id == id))
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "Any account".into());
            let li = line_items.iter().find(|li| li.id == r.line_item_id).map(|li| li.name.clone()).unwrap_or_default();
            (r, acct, li)
        })
        .collect();
    render(RulesPage {
        layout: Layout::new(&state, &user, "settings", "Rules", &flash).await?,
        rules,
        accounts,
        line_items,
    })
}

#[derive(Deserialize)]
pub struct RuleForm {
    pattern: String,
    account_id: Option<String>,
    line_item_id: i64,
    payee: String,
}

pub async fn create_rule(State(state): State<AppState>, user: UserExt, Form(f): Form<RuleForm>) -> AppResult<Response> {
    require_edit(&user)?;
    if f.pattern.trim().len() < 3 {
        return Ok(redirect_err("/rules", "The text to match must be at least 3 characters."));
    }
    let account_id = opt_id(&f.account_id);
    if let Some(aid) = account_id {
        let a = db::account(&state.pool, aid).await?.ok_or(AppError::NotFound)?;
        let li: LineItem =
            sqlx::query_as("SELECT * FROM line_items WHERE id = ?").bind(f.line_item_id).fetch_one(&state.pool).await?;
        if li.fund() != a.fund() {
            return Ok(redirect_err("/rules", "The line item must be in the same fund as the account."));
        }
    }
    sqlx::query("INSERT INTO rules (pattern, account_id, line_item_id, payee) VALUES (?, ?, ?, ?)")
        .bind(f.pattern.trim())
        .bind(account_id)
        .bind(f.line_item_id)
        .bind(f.payee.trim())
        .execute(&state.pool)
        .await?;
    let applied = super::transactions::apply_rules_to_uncategorized(&state.pool).await?;
    Ok(redirect_ok("/rules", &format!("Rule added. It assigned {applied} uncategorized transaction(s).")))
}

pub async fn delete_rule(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
) -> AppResult<Response> {
    require_edit(&user)?;
    sqlx::query("DELETE FROM rules WHERE id = ?").bind(id).execute(&state.pool).await?;
    Ok(redirect_ok("/rules", "Rule deleted."))
}
