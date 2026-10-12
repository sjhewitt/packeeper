//! Licensed gaming events: raffles, bingo, and other events that hold a
//! gaming event licence. Their revenue and costs go on the Gaming Account
//! Summary Report.

use std::collections::HashMap;

use askama::Template;
use axum::extract::{Path as UrlPath, State};
use axum::response::Response;
use axum_extra::extract::{Form, Query};
use chrono::NaiveDate;
use serde::Deserialize;

use super::{AppError, AppResult, AppState, Flash, Layout, UserExt, redirect_err, redirect_ok, render, require_edit};
use crate::db;
use crate::ledger::{self, Filter, TxRow};
use crate::models::{EVENT_TYPES, GamingEvent, LICENCE_CLASSES};
use crate::reports::gaming::{self, EventTotals};

#[derive(Template)]
#[template(path = "events.html")]
struct EventsPage {
    layout: Layout,
    events: Vec<(GamingEvent, EventTotals)>,
    event_types: Vec<(&'static str, &'static str)>,
    classes: Vec<&'static str>,
    today: NaiveDate,
}

pub async fn list(State(state): State<AppState>, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    let mut totals: HashMap<i64, EventTotals> = gaming::event_totals(&state.pool).await?;
    let events = db::gaming_events(&state.pool)
        .await?
        .into_iter()
        .map(|e| {
            let t = totals.remove(&e.id).unwrap_or_default();
            (e, t)
        })
        .collect();
    render(EventsPage {
        layout: Layout::new(&state, &user, "gaming", "Licensed gaming events", &flash).await?,
        events,
        event_types: EVENT_TYPES.to_vec(),
        classes: LICENCE_CLASSES.to_vec(),
        today: chrono::Local::now().date_naive(),
    })
}

#[derive(Deserialize)]
pub struct EventForm {
    name: String,
    licence_number: String,
    licence_class: String,
    event_type: String,
    other_type: String,
    start_date: String,
    end_date: String,
    notes: String,
}

struct Valid {
    start: NaiveDate,
    end: NaiveDate,
}

fn validate(f: &EventForm) -> Result<Valid, String> {
    if f.name.trim().is_empty() {
        return Err("Enter a name for the event.".into());
    }
    if f.licence_number.trim().is_empty() {
        return Err("Enter the gaming event licence number.".into());
    }
    if !LICENCE_CLASSES.contains(&f.licence_class.as_str()) {
        return Err("Choose the licence class: A, B, C, or D.".into());
    }
    if !EVENT_TYPES.iter().any(|(t, _)| *t == f.event_type) {
        return Err("Choose the kind of event.".into());
    }
    if f.event_type == "other" && f.other_type.trim().is_empty() {
        return Err("Say what kind of event it is, such as \"wheel of fortune\".".into());
    }
    let parse = |s: &str| NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok();
    let (Some(start), Some(end)) = (parse(&f.start_date), parse(&f.end_date)) else {
        return Err("Enter the licence start and end dates.".into());
    };
    if end < start {
        return Err("The end date must be on or after the start date.".into());
    }
    Ok(Valid { start, end })
}

pub async fn create(State(state): State<AppState>, user: UserExt, Form(f): Form<EventForm>) -> AppResult<Response> {
    require_edit(&user)?;
    let v = match validate(&f) {
        Ok(v) => v,
        Err(e) => return Ok(redirect_err("/gaming/events", &e)),
    };
    let id = sqlx::query(
        "INSERT INTO gaming_events (name, licence_number, licence_class, event_type, other_type, start_date, end_date, notes, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(f.name.trim())
    .bind(f.licence_number.trim())
    .bind(&f.licence_class)
    .bind(&f.event_type)
    .bind(if f.event_type == "other" { f.other_type.trim() } else { "" })
    .bind(v.start)
    .bind(v.end)
    .bind(f.notes.trim())
    .bind(db::now())
    .execute(&state.pool)
    .await?
    .last_insert_rowid();
    Ok(redirect_ok(
        &format!("/gaming/events/{id}"),
        "Event added. Link its deposits, prizes, and expenses from each transaction.",
    ))
}

#[derive(Template)]
#[template(path = "event.html")]
struct EventPage {
    layout: Layout,
    e: GamingEvent,
    totals: EventTotals,
    rows: Vec<TxRow>,
    event_types: Vec<(&'static str, &'static str)>,
    classes: Vec<&'static str>,
}

pub async fn edit(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Query(flash): Query<Flash>,
) -> AppResult<Response> {
    let e = db::gaming_event(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    let totals = gaming::event_totals(&state.pool).await?.remove(&id).unwrap_or_default();
    let mut rows = ledger::list(&state.pool, &Filter { gaming_event_id: Some(id), ..Default::default() }).await?;
    rows.sort_by_key(|r| (r.tx.date, r.tx.id));
    render(EventPage {
        layout: Layout::new(&state, &user, "gaming", e.name.clone(), &flash).await?,
        e,
        totals,
        rows,
        event_types: EVENT_TYPES.to_vec(),
        classes: LICENCE_CLASSES.to_vec(),
    })
}

pub async fn update(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<EventForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let back = format!("/gaming/events/{id}");
    db::gaming_event(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    let v = match validate(&f) {
        Ok(v) => v,
        Err(e) => return Ok(redirect_err(&back, &e)),
    };
    sqlx::query(
        "UPDATE gaming_events SET name = ?, licence_number = ?, licence_class = ?, event_type = ?, other_type = ?,
           start_date = ?, end_date = ?, notes = ? WHERE id = ?",
    )
    .bind(f.name.trim())
    .bind(f.licence_number.trim())
    .bind(&f.licence_class)
    .bind(&f.event_type)
    .bind(if f.event_type == "other" { f.other_type.trim() } else { "" })
    .bind(v.start)
    .bind(v.end)
    .bind(f.notes.trim())
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(redirect_ok(&back, "Event saved."))
}

/// Deletes an event that no transaction uses.
pub async fn delete(State(state): State<AppState>, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE gaming_event_id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    if used > 0 {
        return Ok(redirect_err(
            &format!("/gaming/events/{id}"),
            &format!("{used} transaction(s) belong to this event. Change them first."),
        ));
    }
    sqlx::query("DELETE FROM gaming_events WHERE id = ?").bind(id).execute(&state.pool).await?;
    Ok(redirect_ok("/gaming/events", "Event deleted."))
}
