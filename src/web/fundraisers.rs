//! Fundraisers that come back every year, such as Purdy's chocolates or hot
//! lunch. The list shows each one's status in a fiscal year; a fundraiser's
//! page shows its goal, its transactions, and every year side by side.

use askama::Template;
use axum::extract::Path as UrlPath;
use axum::response::Response;
use axum_extra::extract::{Form, Query};
use serde::Deserialize;

use super::{
    AppError, AppResult, Ctx, Flash, Layout, UserExt, opt_id, redirect_err, redirect_ok, render, require_edit,
};
use crate::db;
use crate::fiscal;
use crate::models::{FiscalYear, Fundraiser};
use crate::money::{self, Money};
use crate::reports::fundraisers::{self, PartRow, Status, Totals};

#[derive(Debug, Default, Deserialize)]
pub struct YearQuery {
    fy: Option<String>,
    #[serde(flatten)]
    flash: Flash,
}

/// The fiscal year in the query, or the current one.
async fn chosen_year(state: &Ctx, raw: &Option<String>) -> AppResult<FiscalYear> {
    match opt_id(raw) {
        Some(id) => fiscal::get(&state.pool, id).await?.ok_or(AppError::NotFound),
        None => Ok(fiscal::current(&state.pool).await?),
    }
}

#[derive(Template)]
#[template(path = "fundraisers.html")]
struct ListPage {
    layout: Layout,
    years: Vec<FiscalYear>,
    fy: FiscalYear,
    rows: Vec<Status>,
    total: Totals,
    /// Inactive fundraisers with nothing in this year.
    inactive: Vec<Fundraiser>,
}

impl ListPage {
    fn goal_total(&self) -> Money {
        self.rows.iter().filter_map(|r| r.goal).sum()
    }

    fn any_goal(&self) -> bool {
        self.rows.iter().any(|r| r.goal.is_some())
    }

    /// There is a fiscal year before this one to compare with.
    fn has_last_year(&self) -> bool {
        self.rows.iter().any(|r| r.last_year.is_some())
    }
}

pub async fn list(state: Ctx, user: UserExt, Query(q): Query<YearQuery>) -> AppResult<Response> {
    let fy = chosen_year(&state, &q.fy).await?;
    let rows = fundraisers::year(&state.pool, &fy, None, fy.end_date).await?;
    let inactive = db::fundraisers(&state.pool)
        .await?
        .into_iter()
        .filter(|f| !f.active && !rows.iter().any(|r| r.fundraiser.id == f.id))
        .collect();
    render(ListPage {
        layout: Layout::new(&state, &user, "fundraisers", "Fundraisers", &q.flash).await?,
        years: fiscal::list(&state.pool).await?,
        total: fundraisers::sum(&rows),
        fy,
        rows,
        inactive,
    })
}

#[derive(Deserialize)]
pub struct FundraiserForm {
    name: String,
    description: String,
    active: Option<String>,
}

/// The name, or a message when it is blank or another fundraiser has it.
async fn check_name(state: &Ctx, name: &str, id: Option<i64>) -> AppResult<Result<(), String>> {
    if name.is_empty() {
        return Ok(Err("Enter a name for the fundraiser.".into()));
    }
    let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fundraisers WHERE name = ? COLLATE NOCASE AND id != ?")
        .bind(name)
        .bind(id.unwrap_or(0))
        .fetch_one(&state.pool)
        .await?;
    if taken > 0 {
        return Ok(Err(format!("There is already a fundraiser named \"{name}\". Use it every year.")));
    }
    Ok(Ok(()))
}

pub async fn create(state: Ctx, user: UserExt, Form(f): Form<FundraiserForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let name = f.name.trim();
    if let Err(e) = check_name(&state, name, None).await? {
        return Ok(redirect_err("/fundraisers", &e));
    }
    let id = sqlx::query("INSERT INTO fundraisers (name, description, created_at) VALUES (?, ?, ?)")
        .bind(name)
        .bind(f.description.trim())
        .bind(db::now())
        .execute(&state.pool)
        .await?
        .last_insert_rowid();
    Ok(redirect_ok(
        &format!("/fundraisers/{id}"),
        "Fundraiser added. Choose it on each transaction that belongs to it, or on part of a split transaction.",
    ))
}

#[derive(Template)]
#[template(path = "fundraiser.html")]
struct DetailPage {
    layout: Layout,
    f: Fundraiser,
    years: Vec<FiscalYear>,
    fy: FiscalYear,
    status: Status,
    parts: Vec<PartRow>,
    by_line: Vec<(String, Money)>,
    history: Vec<(FiscalYear, Totals, Option<Money>)>,
    checks: Vec<String>,
    /// Transaction parts in any year belong to it, so it cannot be deleted.
    used: bool,
}

impl DetailPage {
    fn goal_input(&self) -> String {
        self.status.goal.map(|g| g.plain()).unwrap_or_default()
    }
}

pub async fn detail(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Query(q): Query<YearQuery>,
) -> AppResult<Response> {
    let pool = &state.pool;
    let f = db::fundraiser(pool, id).await?.ok_or(AppError::NotFound)?;
    let fy = chosen_year(&state, &q.fy).await?;
    let parts = fundraisers::parts(pool, id, fy.start_date, fy.end_date).await?;
    let used: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM allocations WHERE fundraiser_id = ?").bind(id).fetch_one(pool).await?;
    render(DetailPage {
        layout: Layout::new(&state, &user, "fundraisers", f.name.clone(), &q.flash).await?,
        years: fiscal::list(pool).await?,
        status: fundraisers::one(pool, &f, &fy).await?,
        by_line: fundraisers::by_line(&parts),
        checks: fundraisers::checks(&parts),
        history: fundraisers::history(pool, id).await?,
        used: used > 0,
        parts,
        fy,
        f,
    })
}

pub async fn update(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<FundraiserForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let back = format!("/fundraisers/{id}");
    db::fundraiser(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    let name = f.name.trim();
    if let Err(e) = check_name(&state, name, Some(id)).await? {
        return Ok(redirect_err(&back, &e));
    }
    sqlx::query("UPDATE fundraisers SET name = ?, description = ?, active = ? WHERE id = ?")
        .bind(name)
        .bind(f.description.trim())
        .bind(f.active.is_some())
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(redirect_ok(&back, "Fundraiser saved."))
}

#[derive(Deserialize)]
pub struct GoalForm {
    fy: i64,
    goal: String,
}

/// Sets the net goal for one year. A blank goal removes it.
pub async fn set_goal(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<GoalForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    db::fundraiser(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    let fy = fiscal::get(&state.pool, f.fy).await?.ok_or(AppError::NotFound)?;
    let back = format!("/fundraisers/{id}?fy={}", fy.id);
    let raw = f.goal.trim();
    let goal = if raw.is_empty() {
        None
    } else {
        match money::parse(raw) {
            Some(cents) if cents >= 0 => Some(cents),
            _ => {
                return Ok(redirect_err(&back, &format!("\"{raw}\" is not an amount. Enter the net amount to raise.")));
            }
        }
    };
    fundraisers::set_goal(&state.pool, id, fy.id, goal).await?;
    let msg = if goal.is_some() { "Goal saved." } else { "Goal removed." };
    Ok(redirect_ok(&back, msg))
}

/// Deletes a fundraiser that no transaction uses. A fundraiser that has
/// transactions stays; mark it inactive to hide it.
pub async fn delete(state: Ctx, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM allocations WHERE fundraiser_id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    if used > 0 {
        return Ok(redirect_err(
            &format!("/fundraisers/{id}"),
            &format!("{used} transaction part(s) belong to this fundraiser. Mark it inactive instead."),
        ));
    }
    sqlx::query("DELETE FROM fundraisers WHERE id = ?").bind(id).execute(&state.pool).await?;
    Ok(redirect_ok("/fundraisers", "Fundraiser deleted."))
}
