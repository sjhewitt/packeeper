//! Meeting dates, for the treasurer's report since the last meeting. See
//! `crate::meetings`.

use askama::Template;
use axum::extract::Path as UrlPath;
use axum::response::Response;
use axum_extra::extract::{Form, Query};
use chrono::{NaiveDate, Weekday};
use serde::Deserialize;

use super::{
    AppError, AppResult, Ctx, Flash, Layout, UserExt, opt_id, redirect_err, redirect_ok, render, require_edit,
};
use crate::fiscal;
use crate::meetings::{self, Meeting, Period, Rule};
use crate::models::FiscalYear;

#[derive(Template)]
#[template(path = "meetings.html")]
struct MeetingsPage {
    layout: Layout,
    years: Vec<FiscalYear>,
    fy: FiscalYear,
    rule: Option<Rule>,
    rows: Vec<(Meeting, Option<Period>)>,
    weeks: Vec<(u32, &'static str)>,
    weekdays: Vec<&'static str>,
    months: Vec<(u32, &'static str)>,
}

impl MeetingsPage {
    /// The rule to show in the form: the saved one, or the 2nd Wednesday
    /// without July and August.
    fn shown(&self) -> Rule {
        self.rule.clone().unwrap_or(Rule { week: 2, weekday: Weekday::Wed, skip_months: vec![7, 8] })
    }

    fn is_week(&self, n: &u32) -> bool {
        self.shown().week == *n
    }

    fn is_weekday(&self, name: &&str) -> bool {
        meetings::weekday_name(self.shown().weekday) == *name
    }

    fn skips(&self, month: &u32) -> bool {
        self.shown().skips(month)
    }
}

#[derive(Deserialize)]
pub struct YearQuery {
    fy: Option<String>,
    #[serde(flatten)]
    flash: Flash,
}

/// The year in the address, or the current one.
async fn year(state: &Ctx, fy: &Option<String>) -> AppResult<FiscalYear> {
    match opt_id(fy) {
        Some(id) => fiscal::get(&state.pool, id).await?.ok_or(AppError::NotFound),
        None => Ok(fiscal::current(&state.pool).await?),
    }
}

pub async fn page(state: Ctx, user: UserExt, Query(q): Query<YearQuery>) -> AppResult<Response> {
    let pool = &state.pool;
    let fy = year(&state, &q.fy).await?;
    let current = fiscal::current(pool).await?;
    let mut years = fiscal::list(pool).await?;
    let next = fiscal::next(pool, &current).await?;
    if !years.iter().any(|y| y.id == next.id) {
        years.insert(0, next);
    }
    let periods = meetings::periods(pool, &fy).await?;
    let rows = meetings::in_year(pool, &fy)
        .await?
        .into_iter()
        .map(|m| {
            let p = periods.iter().find(|p| p.meeting == m.date).cloned();
            (m, p)
        })
        .collect();
    render(MeetingsPage {
        layout: Layout::new(&state, &user, "settings", "Meeting dates", &q.flash).await?,
        rule: Rule::load(pool).await?,
        rows,
        years,
        fy,
        weeks: meetings::WEEKS.to_vec(),
        weekdays: meetings::WEEKDAYS.iter().map(|(_, n)| *n).collect(),
        months: (1..=12).map(|m| (m, meetings::month_name(m))).collect(),
    })
}

#[derive(Deserialize)]
pub struct RuleForm {
    fy: Option<String>,
    week: Option<String>,
    weekday: Option<String>,
    #[serde(default)]
    skip: Vec<String>,
}

/// Saves the rule. A year with no meetings yet gets the rule's dates.
pub async fn save_rule(state: Ctx, user: UserExt, Form(f): Form<RuleForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let fy = year(&state, &f.fy).await?;
    let back = format!("/meetings?fy={}", fy.id);
    let week = f.week.as_deref().and_then(|w| w.parse::<u32>().ok()).filter(|w| (1..=5).contains(w));
    let weekday = f.weekday.as_deref().and_then(|d| d.parse::<Weekday>().ok());
    let (Some(week), Some(weekday)) = (week, weekday) else {
        return Ok(redirect_err(&back, "Choose the week and the day of the meetings."));
    };
    let mut skip_months: Vec<u32> =
        f.skip.iter().filter_map(|m| m.parse().ok()).filter(|m| (1..=12).contains(m)).collect();
    skip_months.sort();
    skip_months.dedup();
    let rule = Rule { week, weekday, skip_months };
    rule.save(&state.pool).await?;
    if meetings::in_year(&state.pool, &fy).await?.is_empty() {
        meetings::fill(&state.pool, &fy, &rule, false).await?;
    }
    Ok(redirect_ok(&back, &format!("Saved: {}.", rule.describe())))
}

#[derive(Deserialize)]
pub struct FillForm {
    fy: Option<String>,
}

/// Replaces a year's meeting dates with the rule's.
pub async fn fill(state: Ctx, user: UserExt, Form(f): Form<FillForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let fy = year(&state, &f.fy).await?;
    let back = format!("/meetings?fy={}", fy.id);
    let Some(rule) = Rule::load(&state.pool).await? else {
        return Ok(redirect_err(&back, "Save a rule first."));
    };
    meetings::fill(&state.pool, &fy, &rule, true).await?;
    Ok(redirect_ok(&back, &format!("The {} meetings now follow the rule.", fy.label)))
}

#[derive(Deserialize)]
pub struct MeetingForm {
    fy: Option<String>,
    date: Option<String>,
    #[serde(default)]
    note: String,
}

fn parse_date(s: &Option<String>) -> Option<NaiveDate> {
    s.as_deref().and_then(|d| NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").ok())
}

pub async fn add(state: Ctx, user: UserExt, Form(f): Form<MeetingForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let fy = year(&state, &f.fy).await?;
    let back = format!("/meetings?fy={}", fy.id);
    let Some(date) = parse_date(&f.date) else {
        return Ok(redirect_err(&back, "Enter the date of the meeting."));
    };
    if meetings::add(&state.pool, date, &f.note).await? {
        Ok(redirect_ok(&back, &format!("Meeting on {} added.", date.format("%B %-d, %Y"))))
    } else {
        Ok(redirect_err(&back, "There is a meeting on that day already."))
    }
}

pub async fn update(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<MeetingForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let fy = year(&state, &f.fy).await?;
    let back = format!("/meetings?fy={}", fy.id);
    let Some(date) = parse_date(&f.date) else {
        return Ok(redirect_err(&back, "Enter the date of the meeting."));
    };
    if meetings::update(&state.pool, id, date, &f.note).await? {
        Ok(redirect_ok(&back, "Meeting saved."))
    } else {
        Ok(redirect_err(&back, "There is a meeting on that day already."))
    }
}

pub async fn delete(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<FillForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let fy = year(&state, &f.fy).await?;
    meetings::remove(&state.pool, id).await?;
    Ok(redirect_ok(
        &format!("/meetings?fy={}", fy.id),
        "Meeting removed. The next report starts at the meeting before it.",
    ))
}
