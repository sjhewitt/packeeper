//! Set-up pages: bank accounts, line items, and assignment rules.

use askama::Template;
use axum::extract::Path as UrlPath;
use axum::response::Response;
use axum_extra::extract::{Form, Query};
use chrono::NaiveDate;
use serde::Deserialize;

use super::{
    AppError, AppResult, Ctx, Flash, Layout, UserExt, opt_id, redirect_err, redirect_ok, render, require_edit,
};
use crate::db;
use crate::models::{Account, Fundraiser, GASR_EXPENSE_LINES, GASR_INCOME_LINES, LineItem, Rule, valid_gasr_line};
use crate::money::{self, Money};

#[derive(Template)]
#[template(path = "accounts.html")]
struct AccountsPage {
    layout: Layout,
    accounts: Vec<(Account, Money)>,
    today: NaiveDate,
}

pub async fn list(state: Ctx, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
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

pub async fn create(state: Ctx, user: UserExt, Form(f): Form<AccountForm>) -> AppResult<Response> {
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
    state: Ctx,
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
    state: Ctx,
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
    /// Each line item, the parts that use it, and its fundraiser's name.
    items: Vec<(LineItem, i64, String)>,
    income_lines: Vec<(&'static str, &'static str)>,
    expense_lines: Vec<(&'static str, &'static str)>,
    /// Active fundraisers, for a new general-fund line item.
    fundraisers: Vec<Fundraiser>,
}

pub async fn line_items(state: Ctx, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    let counts: Vec<(i64, i64)> =
        sqlx::query_as("SELECT line_item_id, COUNT(*) FROM allocations GROUP BY line_item_id")
            .fetch_all(&state.pool)
            .await?;
    let fundraisers = db::fundraisers(&state.pool).await?;
    let items = db::line_items(&state.pool)
        .await?
        .into_iter()
        .map(|li| {
            let n = counts.iter().find(|(id, _)| *id == li.id).map(|(_, n)| *n).unwrap_or(0);
            let fundraiser = fundraisers.iter().find(|f| Some(f.id) == li.fundraiser_id).map(|f| f.name.clone());
            (li, n, fundraiser.unwrap_or_default())
        })
        .collect();
    render(LineItemsPage {
        layout: Layout::new(&state, &user, "settings", "Line items", &flash).await?,
        items,
        income_lines: GASR_INCOME_LINES.to_vec(),
        expense_lines: GASR_EXPENSE_LINES.to_vec(),
        fundraisers: fundraisers.into_iter().filter(|f| f.active).collect(),
    })
}

/// The fundraiser chosen on a line item form. Only general-fund line items
/// have one, because fundraisers are for general-account money.
async fn chosen_fundraiser(
    pool: &sqlx::SqlitePool,
    fund: &str,
    raw: &Option<String>,
) -> anyhow::Result<Option<Fundraiser>> {
    match opt_id(raw) {
        Some(id) if fund == "general" => db::fundraiser(pool, id).await,
        _ => Ok(None),
    }
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
    fundraiser_id: Option<String>,
}

pub async fn create_line_item(state: Ctx, user: UserExt, Form(f): Form<LineItemForm>) -> AppResult<Response> {
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
    let fundraiser = chosen_fundraiser(&state.pool, fund, &f.fundraiser_id).await?;
    let next_sort: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), 0) + 10 FROM line_items").fetch_one(&state.pool).await?;
    let result = sqlx::query(
        "INSERT INTO line_items (name, fund, kind, gasr_line, receipt_required, description, sort_order, fundraiser_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(name)
    .bind(fund)
    .bind(kind)
    .bind(gasr)
    .bind(kind == "expense")
    .bind(f.description.trim())
    .bind(next_sort)
    .bind(fundraiser.map(|f| f.id))
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
    /// Fiscal years with a budget amount for this line item.
    budgets: i64,
    rules: i64,
    fundraisers: Vec<Fundraiser>,
}

impl LineItemEditPage {
    fn is_fundraiser(&self, id: &i64) -> bool {
        self.li.fundraiser_id == Some(*id)
    }

    /// The question before a delete, with what goes with the line item.
    fn delete_question(&self) -> String {
        let mut also = Vec::new();
        if self.budgets > 0 {
            also.push(format!("its budget amount in {} fiscal year(s)", self.budgets));
        }
        if self.rules > 0 {
            also.push(format!("{} rule(s) that assign it", self.rules));
        }
        if also.is_empty() {
            "Delete this line item?".to_string()
        } else {
            format!("Delete this line item? This also deletes {}.", also.join(" and "))
        }
    }
}

pub async fn edit_line_item(
    state: Ctx,
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
    let budgets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM budget_amounts WHERE line_item_id = ? AND amount_cents != 0")
            .bind(id)
            .fetch_one(&state.pool)
            .await?;
    let rules: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM rules WHERE line_item_id = ?").bind(id).fetch_one(&state.pool).await?;
    render(LineItemEditPage {
        layout: Layout::new(&state, &user, "settings", li.name.clone(), &flash).await?,
        li,
        used,
        gasr_lines,
        budgets,
        rules,
        fundraisers: db::fundraisers(&state.pool).await?,
    })
}

/// Deletes a line item that no transaction uses, with its budget amounts and
/// the rules that assign it. A line item in use is retired instead (untick
/// "Show in lists"), so the history stays.
pub async fn delete_line_item(state: Ctx, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    let mut tx = state.pool.begin().await?;
    let name: String = sqlx::query_scalar("SELECT name FROM line_items WHERE id = ?")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM allocations WHERE line_item_id = ?")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if used > 0 {
        return Ok(redirect_err(
            &format!("/line-items/{id}"),
            &format!("{used} transaction part(s) use this line item. Untick \"Show in lists\" to retire it instead."),
        ));
    }
    for sql in [
        "DELETE FROM budget_amounts WHERE line_item_id = ?",
        "DELETE FROM rules WHERE line_item_id = ?",
        "DELETE FROM line_items WHERE id = ?",
    ] {
        sqlx::query(sql).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(redirect_ok("/line-items", &format!("Line item \"{name}\" deleted.")))
}

pub async fn update_line_item(
    state: Ctx,
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
    let fundraiser = chosen_fundraiser(&state.pool, &li.fund, &f.fundraiser_id).await?;
    let result = sqlx::query(
        "UPDATE line_items SET name = ?, kind = ?, gasr_line = ?, description = ?, sort_order = ?, active = ?,
           receipt_required = ?, fundraiser_id = ?
         WHERE id = ?",
    )
    .bind(f.name.trim())
    .bind(&kind)
    .bind(gasr)
    .bind(f.description.trim())
    .bind(sort)
    .bind(f.active.is_some())
    .bind(f.receipt_required.is_some())
    .bind(fundraiser.as_ref().map(|f| f.id))
    .bind(id)
    .execute(&state.pool)
    .await;
    if result.is_err() {
        return Ok(redirect_err(&back, "That fund already has a line item with that name."));
    }
    // A new fundraiser also goes to the parts that already use the line item.
    let moved = match &fundraiser {
        Some(fr) if Some(fr.id) != li.fundraiser_id => {
            crate::ledger::give_line_fundraiser(&state.pool, id, li.fundraiser_id, fr.id).await?
        }
        _ => 0,
    };
    match (moved, &fundraiser) {
        (n, Some(fr)) if n > 0 => Ok(redirect_ok(
            "/line-items",
            &format!("Line item saved. {n} transaction part(s) now belong to {}.", fr.name),
        )),
        _ => Ok(redirect_ok("/line-items", "Line item saved.")),
    }
}

#[derive(Template)]
#[template(path = "rules.html")]
struct RulesPage {
    layout: Layout,
    /// Each rule with its account, line item, and fundraiser names.
    rules: Vec<(Rule, String, String, String)>,
    accounts: Vec<Account>,
    line_items: Vec<LineItem>,
    fundraisers: Vec<Fundraiser>,
}

pub async fn rules(state: Ctx, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    let rules: Vec<Rule> = sqlx::query_as("SELECT * FROM rules ORDER BY pattern").fetch_all(&state.pool).await?;
    let accounts = db::accounts(&state.pool).await?;
    let line_items = db::line_items(&state.pool).await?;
    let fundraisers = db::fundraisers(&state.pool).await?;
    let rules = rules
        .into_iter()
        .map(|r| {
            let acct = r
                .account_id
                .and_then(|id| accounts.iter().find(|a| a.id == id))
                .map(|a| a.name.clone())
                .unwrap_or_else(|| "Any account".into());
            let li = line_items.iter().find(|li| li.id == r.line_item_id).map(|li| li.name.clone()).unwrap_or_default();
            let fr = r
                .fundraiser_id
                .and_then(|id| fundraisers.iter().find(|f| f.id == id))
                .map(|f| f.name.clone())
                .unwrap_or_default();
            (r, acct, li, fr)
        })
        .collect();
    render(RulesPage {
        layout: Layout::new(&state, &user, "settings", "Rules", &flash).await?,
        rules,
        accounts,
        line_items,
        fundraisers: fundraisers.into_iter().filter(|f| f.active).collect(),
    })
}

/// The fundraiser a new rule sets, if any. Fundraisers go with general-fund
/// line items only.
pub(super) async fn rule_fundraiser(
    pool: &sqlx::SqlitePool,
    line_item_id: i64,
    raw: &Option<String>,
) -> AppResult<Result<Option<i64>, String>> {
    let Some(id) = opt_id(raw) else { return Ok(Ok(None)) };
    db::fundraiser(pool, id).await?.ok_or(AppError::NotFound)?;
    let li: LineItem =
        sqlx::query_as("SELECT * FROM line_items WHERE id = ?").bind(line_item_id).fetch_one(pool).await?;
    if li.fund() == crate::models::Fund::Gaming {
        return Ok(Err("Fundraisers are for general-account money. Choose a general line item.".into()));
    }
    Ok(Ok(Some(id)))
}

#[derive(Deserialize)]
pub struct RuleForm {
    pattern: String,
    account_id: Option<String>,
    line_item_id: i64,
    payee: String,
    fundraiser_id: Option<String>,
}

pub async fn create_rule(state: Ctx, user: UserExt, Form(f): Form<RuleForm>) -> AppResult<Response> {
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
    let fundraiser_id = match rule_fundraiser(&state.pool, f.line_item_id, &f.fundraiser_id).await? {
        Ok(id) => id,
        Err(e) => return Ok(redirect_err("/rules", &e)),
    };
    sqlx::query("INSERT INTO rules (pattern, account_id, line_item_id, payee, fundraiser_id) VALUES (?, ?, ?, ?, ?)")
        .bind(f.pattern.trim())
        .bind(account_id)
        .bind(f.line_item_id)
        .bind(f.payee.trim())
        .bind(fundraiser_id)
        .execute(&state.pool)
        .await?;
    let applied = super::transactions::apply_rules_to_uncategorized(&state.pool).await?;
    Ok(redirect_ok("/rules", &format!("Rule added. It assigned {applied} uncategorized transaction(s).")))
}

pub async fn delete_rule(state: Ctx, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    sqlx::query("DELETE FROM rules WHERE id = ?").bind(id).execute(&state.pool).await?;
    Ok(redirect_ok("/rules", "Rule deleted."))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use sqlx::SqlitePool;

    use crate::web::test_support::{example, send};

    async fn one(pool: &SqlitePool, sql: &str, id: i64) -> Option<i64> {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).bind(id).fetch_one(pool).await.unwrap()
    }

    /// Only a line item that no transaction uses can be deleted. Its budget
    /// amounts and rules go with it, and the starter line items do not come back.
    #[tokio::test]
    async fn deletes_only_an_unused_line_item() {
        let (app, pool, dir) = example("line-delete").await;
        let used: i64 =
            sqlx::query_scalar("SELECT line_item_id FROM allocations LIMIT 1").fetch_one(&pool).await.unwrap();
        let unused =
            sqlx::query("INSERT INTO line_items (name, fund, kind) VALUES ('Spare line', 'general', 'expense')")
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid();
        let year: i64 = sqlx::query_scalar("SELECT id FROM fiscal_years LIMIT 1").fetch_one(&pool).await.unwrap();
        sqlx::query("INSERT INTO budget_amounts (fiscal_year_id, line_item_id, amount_cents) VALUES (?, ?, 5000)")
            .bind(year)
            .bind(unused)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO rules (pattern, line_item_id) VALUES ('SPARE', ?)")
            .bind(unused)
            .execute(&pool)
            .await
            .unwrap();

        // The page offers Delete only for the unused line item, and says what goes with it.
        let (_, _, page) = send(&app, "GET", &format!("/line-items/{used}"), "").await;
        assert!(!page.contains("/delete"));
        let (_, _, page) = send(&app, "GET", &format!("/line-items/{unused}"), "").await;
        assert!(page.contains(&format!("/line-items/{unused}/delete")), "{page}");
        assert!(page.contains("budget amount in 1 fiscal year(s) and 1 rule(s)"), "{page}");

        let (status, location, _) = send(&app, "POST", &format!("/line-items/{used}/delete"), "").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(location.contains("err="));
        assert_eq!(one(&pool, "SELECT COUNT(*) FROM line_items WHERE id = ?", used).await, Some(1));

        let (status, location, _) = send(&app, "POST", &format!("/line-items/{unused}/delete"), "").await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert!(location.contains("msg="));
        for sql in [
            "SELECT COUNT(*) FROM line_items WHERE id = ?",
            "SELECT COUNT(*) FROM budget_amounts WHERE line_item_id = ?",
            "SELECT COUNT(*) FROM rules WHERE line_item_id = ?",
        ] {
            assert_eq!(one(&pool, sql, unused).await, Some(0), "{sql}");
        }

        // With every line item gone, opening the folder again adds none back.
        for table in ["allocations", "rules", "budget_amounts", "line_items"] {
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}"))).execute(&pool).await.unwrap();
        }
        crate::db::seed(&pool).await.unwrap();
        assert_eq!(one(&pool, "SELECT COUNT(*) FROM line_items WHERE id > ?", 0).await, Some(0));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A line item's fundraiser goes to the parts on it: the ones there
    /// already, new ones from the list, and imported ones. A fundraiser that
    /// someone chose for a part stays.
    #[tokio::test]
    async fn a_line_item_gives_its_fundraiser() {
        let (app, pool, dir) = example("line-fundraiser").await;
        // A general line item whose parts have no fundraiser, and another one.
        let lines: Vec<i64> = sqlx::query_scalar(
            "SELECT a.line_item_id FROM allocations a JOIN line_items l ON l.id = a.line_item_id
             WHERE l.fund = 'general' GROUP BY a.line_item_id HAVING SUM(a.fundraiser_id IS NOT NULL) = 0 LIMIT 2",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        let [line, other] = lines[..] else { panic!("the example needs two untagged general line items") };
        let parts = one(&pool, "SELECT COUNT(*) FROM allocations WHERE line_item_id = ?", line).await.unwrap();
        let mut fundraisers = Vec::new();
        for name in ["Linked fundraiser", "Chosen fundraiser"] {
            let id = sqlx::query("INSERT INTO fundraisers (name, created_at) VALUES (?, '2026-01-01')")
                .bind(name)
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid();
            fundraisers.push(id);
        }
        let [linked, chosen] = fundraisers[..] else { unreachable!() };

        // Linking the line item tags the parts already on it.
        let name: String =
            sqlx::query_scalar("SELECT name FROM line_items WHERE id = ?").bind(line).fetch_one(&pool).await.unwrap();
        let form = format!("name={}&description=&active=on&fundraiser_id={linked}", crate::web::urlencode(&name));
        let (_, location, _) = send(&app, "POST", &format!("/line-items/{line}"), &form).await;
        assert!(location.contains(&format!("{parts}%20transaction%20part")), "{location}");
        let tagged = "SELECT COUNT(*) FROM allocations WHERE line_item_id = ? AND fundraiser_id IS NOT NULL";
        assert_eq!(one(&pool, tagged, line).await, Some(parts));

        // A one-part transaction on the other line item.
        let tx: i64 = sqlx::query_scalar(
            "SELECT transaction_id FROM allocations GROUP BY transaction_id
             HAVING COUNT(*) = 1 AND MAX(line_item_id) = ? LIMIT 1",
        )
        .bind(other)
        .fetch_one(&pool)
        .await
        .unwrap();
        let fundraiser = "SELECT fundraiser_id FROM allocations WHERE transaction_id = ?";

        // Moving it to the linked line item gives the fundraiser; moving it back takes it away.
        send(&app, "POST", &format!("/transactions/{tx}/assign"), &format!("line_item_id={line}")).await;
        assert_eq!(one(&pool, fundraiser, tx).await, Some(linked));
        send(&app, "POST", &format!("/transactions/{tx}/assign"), &format!("line_item_id={other}")).await;
        assert_eq!(one(&pool, fundraiser, tx).await, None);

        // A fundraiser that someone chose stays when the line item changes.
        send(&app, "POST", &format!("/transactions/{tx}/fundraiser"), &format!("fundraiser_id={chosen}")).await;
        send(&app, "POST", &format!("/transactions/{tx}/assign"), &format!("line_item_id={line}")).await;
        assert_eq!(one(&pool, fundraiser, tx).await, Some(chosen));
        // "No fundraiser" on the linked line item means the line item's.
        send(&app, "POST", &format!("/transactions/{tx}/fundraiser"), "fundraiser_id=").await;
        assert_eq!(one(&pool, fundraiser, tx).await, Some(linked));

        // The split form shows the line item's fundraiser as the default choice.
        let (_, _, page) = send(&app, "GET", &format!("/transactions/{tx}"), "").await;
        assert!(page.contains("Linked fundraiser (from the line item)"), "{page}");

        // An import that a rule assigns to the line item gets the fundraiser.
        let account: crate::models::Account =
            sqlx::query_as("SELECT * FROM accounts WHERE id = (SELECT account_id FROM transactions WHERE id = ?)")
                .bind(tx)
                .fetch_one(&pool)
                .await
                .unwrap();
        sqlx::query("INSERT INTO rules (pattern, account_id, line_item_id) VALUES ('LINKED SALE', ?, ?)")
            .bind(account.id)
            .bind(line)
            .execute(&pool)
            .await
            .unwrap();
        let batch = sqlx::query(
            "INSERT INTO import_batches (account_id, filename, stored_name, format, created_at)
             VALUES (?, 'test.csv', 'test.csv', 'csv', '2026-01-01')",
        )
        .bind(account.id)
        .execute(&pool)
        .await
        .unwrap()
        .last_insert_rowid();
        let sign: i64 = sqlx::query_scalar("SELECT SIGN(amount_cents) FROM transactions WHERE id = ?")
            .bind(tx)
            .fetch_one(&pool)
            .await
            .unwrap();
        let row = crate::import::BankRow {
            date: chrono::Local::now().date_naive(),
            description: "LINKED SALE 1234".into(),
            amount_cents: sign * 1234,
            balance_cents: None,
            reference: String::new(),
            bank_ref: "LINKED-1".into(),
        };
        crate::import::apply(&pool, &account, batch, &[row], None).await.unwrap();
        let imported: Option<i64> = sqlx::query_scalar(
            "SELECT a.fundraiser_id FROM allocations a JOIN transactions t ON t.id = a.transaction_id
             WHERE t.bank_ref = 'LINKED-1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(imported, Some(linked));

        // A gaming line item cannot have a fundraiser.
        let (gaming, gaming_name): (i64, String) =
            sqlx::query_as("SELECT id, name FROM line_items WHERE fund = 'gaming' LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        let form =
            format!("name={}&description=&active=on&fundraiser_id={linked}", crate::web::urlencode(&gaming_name));
        send(&app, "POST", &format!("/line-items/{gaming}"), &form).await;
        assert_eq!(one(&pool, "SELECT fundraiser_id FROM line_items WHERE id = ?", gaming).await, None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
