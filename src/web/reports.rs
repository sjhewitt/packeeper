use askama::Template;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::Query;
use chrono::NaiveDate;
use serde::Deserialize;

use super::{AppError, AppResult, AppState, Flash, Layout, UserExt, opt_id, render};
use crate::fiscal;
use crate::models::FiscalYear;
use crate::reports::gaming::{self, GamingReport};
use crate::reports::{pdf, proposal};

#[derive(Template)]
#[template(path = "reports.html")]
struct ReportsPage {
    layout: Layout,
    years: Vec<FiscalYear>,
    current: FiscalYear,
    months: Vec<(String, String)>,
    last_month: String,
    has_gaming: bool,
    next_id: i64,
}

pub async fn page(State(state): State<AppState>, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    let pool = &state.pool;
    let current = fiscal::current(pool).await?;
    let mut years = fiscal::list(pool).await?;
    // Offer next year for the budget proposal.
    let next = fiscal::next(pool, &current).await?;
    let next_id = next.id;
    if !years.iter().any(|y| y.id == next.id) {
        years.insert(0, next);
    }
    let today = chrono::Local::now().date_naive();
    let mut months = Vec::new();
    for fy in years.iter().filter(|y| y.start_date <= today) {
        for m in fiscal::months(fy.start_date, fy.end_date.min(today)).into_iter().rev() {
            months.push((m.format("%Y-%m").to_string(), m.format("%B %Y").to_string()));
        }
    }
    months.sort_by(|a, b| b.0.cmp(&a.0));
    months.dedup();
    let last_month = months.get(1).or(months.first()).map(|m| m.0.clone()).unwrap_or_default();
    let has_gaming = crate::db::accounts(pool).await?.iter().any(|a| a.is_gaming());
    render(ReportsPage {
        layout: Layout::new(&state, &user, "reports", "Reports", &flash).await?,
        years,
        current,
        months,
        last_month,
        has_gaming,
        next_id,
    })
}

fn pdf_response(bytes: Vec<u8>, filename: &str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (header::CONTENT_DISPOSITION, format!("inline; filename=\"{filename}\"")),
        ],
        bytes,
    )
        .into_response()
}

async fn render_pdf(state: &AppState, template: &'static str, data: serde_json::Value) -> AppResult<Vec<u8>> {
    let renderer = state.renderer.clone();
    let bytes = tokio::task::spawn_blocking(move || renderer.render(template, &data)).await??;
    Ok(bytes)
}

#[derive(Deserialize)]
pub struct MonthQuery {
    month: Option<String>,
}

pub async fn monthly_pdf(State(state): State<AppState>, Query(q): Query<MonthQuery>) -> AppResult<Response> {
    let month = q
        .month
        .as_deref()
        .and_then(|m| NaiveDate::parse_from_str(&format!("{m}-01"), "%Y-%m-%d").ok())
        .ok_or_else(|| AppError::BadRequest("Choose a month.".into()))?;
    let data = pdf::monthly(&state.pool, month).await?;
    let bytes = render_pdf(&state, "monthly.typ", data).await?;
    Ok(pdf_response(bytes, &format!("treasurers-report-{}.pdf", month.format("%Y-%m"))))
}

#[derive(Deserialize)]
pub struct FyQuery {
    fy: Option<String>,
}

async fn year(state: &AppState, q: &FyQuery) -> AppResult<FiscalYear> {
    match opt_id(&q.fy) {
        Some(id) => fiscal::get(&state.pool, id).await?.ok_or(AppError::NotFound),
        None => Ok(fiscal::current(&state.pool).await?),
    }
}

fn file_label(fy: &FiscalYear) -> String {
    fy.label.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

pub async fn annual_pdf(State(state): State<AppState>, Query(q): Query<FyQuery>) -> AppResult<Response> {
    let fy = year(&state, &q).await?;
    let data = pdf::annual(&state.pool, &fy).await?;
    let bytes = render_pdf(&state, "annual.typ", data).await?;
    Ok(pdf_response(bytes, &format!("annual-report-{}.pdf", file_label(&fy))))
}

/// `fy` is the year the budget is for; it is based on the year before.
pub async fn budget_pdf(State(state): State<AppState>, Query(q): Query<FyQuery>) -> AppResult<Response> {
    let target = year(&state, &q).await?;
    let base = fiscal::previous(&state.pool, &target).await?.ok_or_else(|| {
        AppError::BadRequest(format!("There is no fiscal year before {} to base the budget on.", target.label))
    })?;
    let p = proposal::build(&state.pool, &base, &target).await?;
    let data = pdf::budget(&state.pool, &p).await?;
    let bytes = render_pdf(&state, "budget.typ", data).await?;
    Ok(pdf_response(bytes, &format!("budget-proposal-{}.pdf", file_label(&target))))
}

pub async fn gaming_pdf(State(state): State<AppState>, Query(q): Query<FyQuery>) -> AppResult<Response> {
    let fy = year(&state, &q).await?;
    let report = gaming::build(&state.pool, &fy).await?;
    let data = pdf::gasr(&state.pool, &report).await?;
    let bytes = render_pdf(&state, "gaming.typ", data).await?;
    Ok(pdf_response(bytes, &format!("gaming-account-summary-{}.pdf", file_label(&fy))))
}

#[derive(Template)]
#[template(path = "gaming.html")]
struct GamingPage {
    layout: Layout,
    years: Vec<FiscalYear>,
    g: GamingReport,
    has_gaming: bool,
}

#[derive(Deserialize)]
pub struct GamingQuery {
    fy: Option<String>,
    #[serde(flatten)]
    flash: Flash,
}

pub async fn gaming_page(
    State(state): State<AppState>,
    user: UserExt,
    Query(q): Query<GamingQuery>,
) -> AppResult<Response> {
    let pool = &state.pool;
    let fy = match opt_id(&q.fy) {
        Some(id) => fiscal::get(pool, id).await?.ok_or(AppError::NotFound)?,
        None => {
            // Default to last year while its report is due, else this year.
            let current = fiscal::current(pool).await?;
            let today = chrono::Local::now().date_naive();
            match fiscal::previous(pool, &current).await? {
                Some(prev) if prev.end_date + chrono::Days::new(gaming::DUE_WITHIN_DAYS) >= today => prev,
                _ => current,
            }
        }
    };
    let g = gaming::build(pool, &fy).await?;
    render(GamingPage {
        layout: Layout::new(&state, &user, "gaming", "Gaming account", &q.flash).await?,
        years: fiscal::list(pool).await?,
        has_gaming: crate::db::accounts(pool).await?.iter().any(|a| a.is_gaming()),
        g,
    })
}
