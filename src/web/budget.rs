use std::collections::HashMap;

use askama::Template;
use axum::extract::{Path as UrlPath, State};
use axum::response::Response;
use axum_extra::extract::{Form, Query};
use chrono::NaiveDate;
use serde::Deserialize;

use super::{
    AppError, AppResult, AppState, Flash, Layout, UserExt, opt_id, redirect_err, redirect_ok, render, require_edit,
};
use crate::db::Settings;
use crate::fiscal;
use crate::models::FiscalYear;
use crate::money;
use crate::reports::proposal::{self, Fill, Proposal};
use crate::reports::{self, FundSummary};

#[derive(Debug, Default, Deserialize)]
pub struct YearQuery {
    fy: Option<String>,
    base: Option<String>,
    #[serde(flatten)]
    flash: Flash,
}

#[derive(Template)]
#[template(path = "budget.html")]
struct BudgetPage {
    layout: Layout,
    fy: FiscalYear,
    years: Vec<FiscalYear>,
    funds: Vec<FundSummary>,
    notes: HashMap<i64, String>,
}

impl BudgetPage {
    fn note(&self, id: &Option<i64>) -> String {
        id.and_then(|id| self.notes.get(&id).cloned()).unwrap_or_default()
    }
}

async fn pick_year(state: &AppState, id: Option<i64>) -> AppResult<FiscalYear> {
    match id {
        Some(id) => fiscal::get(&state.pool, id).await?.ok_or(AppError::NotFound),
        None => Ok(fiscal::current(&state.pool).await?),
    }
}

pub async fn page(State(state): State<AppState>, user: UserExt, Query(q): Query<YearQuery>) -> AppResult<Response> {
    let pool = &state.pool;
    let fy = pick_year(&state, opt_id(&q.fy)).await?;
    let funds = reports::fund_summaries(pool, &fy, fy.start_date, fy.end_date).await?;
    let notes = reports::budgets(pool, fy.id).await?.into_iter().map(|(id, (_, n))| (id, n)).collect();
    render(BudgetPage {
        layout: Layout::new(&state, &user, "budget", format!("Budget {}", fy.label), &q.flash).await?,
        fy,
        years: fiscal::list(pool).await?,
        funds,
        notes,
    })
}

#[derive(Deserialize)]
pub struct AmountsForm {
    #[serde(default)]
    line_item_id: Vec<i64>,
    #[serde(default)]
    amount: Vec<String>,
    #[serde(default)]
    notes: Vec<String>,
    back: Option<String>,
}

/// Saves every budget amount on the page at once.
pub async fn set_amounts(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(fy_id): UrlPath<i64>,
    Form(f): Form<AmountsForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let fy = fiscal::get(pool, fy_id).await?.ok_or(AppError::NotFound)?;
    let back = f.back.clone().filter(|b| b.starts_with('/')).unwrap_or_else(|| format!("/budget?fy={}", fy.id));
    let mut values = Vec::new();
    for (i, li) in f.line_item_id.iter().enumerate() {
        let raw = f.amount.get(i).map(String::as_str).unwrap_or("").trim();
        let amount = if raw.is_empty() {
            0
        } else {
            match money::parse(raw) {
                Some(a) => a.abs(),
                None => return Ok(redirect_err(&back, &format!("\"{raw}\" is not an amount."))),
            }
        };
        values.push((*li, amount, f.notes.get(i).cloned().unwrap_or_default()));
    }
    let mut tx = pool.begin().await?;
    for (li, amount, notes) in values {
        sqlx::query(
            "INSERT INTO budget_amounts (fiscal_year_id, line_item_id, amount_cents, notes) VALUES (?, ?, ?, ?)
             ON CONFLICT (fiscal_year_id, line_item_id) DO UPDATE SET amount_cents = excluded.amount_cents, notes = excluded.notes",
        )
        .bind(fy.id)
        .bind(li)
        .bind(amount)
        .bind(notes.trim())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(redirect_ok(&back, &format!("Budget for {} saved.", fy.label)))
}

#[derive(Deserialize)]
pub struct YearForm {
    label: String,
    start_date: String,
    end_date: String,
    budget_status: String,
    budget_approved_on: String,
    notes: String,
    back: Option<String>,
}

pub async fn update_year(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(fy_id): UrlPath<i64>,
    Form(f): Form<YearForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let fy = fiscal::get(pool, fy_id).await?.ok_or(AppError::NotFound)?;
    let back = f.back.clone().filter(|b| b.starts_with('/')).unwrap_or_else(|| format!("/budget?fy={}", fy.id));
    let parse = |s: &str| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok();
    let (Some(start), Some(end)) = (parse(&f.start_date), parse(&f.end_date)) else {
        return Ok(redirect_err(&back, "Enter valid start and end dates."));
    };
    if start >= end {
        return Ok(redirect_err(&back, "The year must end after it starts."));
    }
    let overlap: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM fiscal_years WHERE id != ? AND start_date <= ? AND end_date >= ?")
            .bind(fy.id)
            .bind(end)
            .bind(start)
            .fetch_one(pool)
            .await?;
    if overlap > 0 {
        return Ok(redirect_err(&back, "Those dates overlap another fiscal year."));
    }
    if !matches!(f.budget_status.as_str(), "draft" | "proposed" | "approved") {
        return Ok(redirect_err(&back, "Unknown budget status."));
    }
    let label = if f.label.trim().is_empty() { fiscal::label(start, end) } else { f.label.trim().to_string() };
    let result = sqlx::query(
        "UPDATE fiscal_years SET label = ?, start_date = ?, end_date = ?, budget_status = ?, budget_approved_on = ?, notes = ? WHERE id = ?",
    )
    .bind(label)
    .bind(start)
    .bind(end)
    .bind(&f.budget_status)
    .bind(parse(&f.budget_approved_on))
    .bind(f.notes.trim())
    .bind(fy.id)
    .execute(pool)
    .await;
    match result {
        Ok(_) => Ok(redirect_ok(&back, "Fiscal year saved.")),
        Err(_) => Ok(redirect_err(&back, "Another fiscal year already has that name.")),
    }
}

pub async fn create_next_year(
    State(state): State<AppState>,
    user: UserExt,
    Form(q): Form<YearQuery>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let base = pick_year(&state, opt_id(&q.fy)).await?;
    let next = fiscal::next(&state.pool, &base).await?;
    Ok(redirect_ok(
        &format!("/budget/proposal?base={}", base.id),
        &format!("Fiscal year {} is ready for a budget.", next.label),
    ))
}

#[derive(Template)]
#[template(path = "proposal.html")]
struct ProposalPage {
    layout: Layout,
    p: Proposal,
    years: Vec<FiscalYear>,
    settings: Settings,
}

pub async fn proposal(State(state): State<AppState>, user: UserExt, Query(q): Query<YearQuery>) -> AppResult<Response> {
    let pool = &state.pool;
    let base = pick_year(&state, opt_id(&q.base)).await?;
    let target = fiscal::next(pool, &base).await?;
    let p = proposal::build(pool, &base, &target).await?;
    render(ProposalPage {
        layout: Layout::new(&state, &user, "budget", format!("Proposed budget {}", target.label), &q.flash).await?,
        p,
        years: fiscal::list(pool).await?,
        settings: Settings::load(pool).await?,
    })
}

#[derive(Deserialize)]
pub struct FillForm {
    base: i64,
    how: String,
}

pub async fn fill(State(state): State<AppState>, user: UserExt, Form(f): Form<FillForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let base = fiscal::get(pool, f.base).await?.ok_or(AppError::NotFound)?;
    let target = fiscal::next(pool, &base).await?;
    let (how, label) = match f.how.as_str() {
        "budget" => (Fill::Budget, "last year's budget"),
        "actual" => (Fill::Actual, "last year's actuals"),
        _ => (Fill::ActualRounded, "last year's actuals, rounded up to $50"),
    };
    proposal::fill(pool, &base, &target, how).await?;
    Ok(redirect_ok(
        &format!("/budget/proposal?base={}", base.id),
        &format!("Proposed amounts filled from {label}. Adjust them, then save."),
    ))
}
