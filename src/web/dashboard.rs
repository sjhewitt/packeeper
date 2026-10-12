use askama::Template;
use axum::extract::State;
use axum::response::Response;
use axum_extra::extract::Query;

use super::{AppResult, AppState, Flash, Layout, UserExt, render};
use crate::fiscal;
use crate::ledger::{self, Filter, TxRow};
use crate::models::FiscalYear;
use crate::money::Money;
use crate::reports::gaming::{self, GrantRow};
use crate::reports::{self, AccountSummary, FundSummary};

#[derive(Template)]
#[template(path = "dashboard.html")]
struct Page {
    layout: Layout,
    fy: FiscalYear,
    accounts: Vec<AccountSummary>,
    funds: Vec<FundSummary>,
    recent: Vec<TxRow>,
    grants: Vec<GrantRow>,
    gasr_due: Option<(String, chrono::NaiveDate)>,
    has_gaming: bool,
}

pub async fn page(State(state): State<AppState>, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    let pool = &state.pool;
    let today = chrono::Local::now().date_naive();
    let fy = fiscal::current(pool).await?;
    let accounts = reports::account_summaries(pool, fy.start_date, today).await?;
    let funds = reports::fund_summaries(pool, &fy, fy.start_date, today).await?;
    let recent = ledger::list(pool, &Filter { limit: Some(12), ..Default::default() }).await?;
    let has_gaming = accounts.iter().any(|a| a.account.is_gaming());

    let gaming_balance: Money = accounts.iter().filter(|a| a.account.is_gaming()).map(|a| a.closing).sum();
    let grants = if has_gaming {
        gaming::grant_schedule(pool, today, gaming_balance)
            .await?
            .into_iter()
            .filter(|g| g.unspent.cents() > 0)
            .collect()
    } else {
        Vec::new()
    };
    // The report for last year is due 90 days after it ended.
    let gasr_due = match fiscal::previous(pool, &fy).await? {
        Some(prev) if has_gaming => {
            let due = prev.end_date + chrono::Days::new(gaming::DUE_WITHIN_DAYS);
            (due >= today).then_some((prev.label, due))
        }
        _ => None,
    };

    render(Page {
        layout: Layout::new(&state, &user, "dashboard", "Dashboard", &flash).await?,
        fy,
        accounts,
        funds,
        recent,
        grants,
        gasr_due,
        has_gaming,
    })
}
