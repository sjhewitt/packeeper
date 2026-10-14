//! Hot lunch for the PAC's executive: turn it on, set the menu and divisions,
//! open ordering periods, see what to buy each day, and set up payments.
//! Parents order on the /family pages (`web::family`).
//!
//! Every page needs the server's hot lunch flag and a hosted PAC. All but the
//! first page also need the PAC to turn hot lunch on.

use std::collections::HashMap;
use std::sync::Arc;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use axum_extra::extract::{Form, Query};
use chrono::NaiveDate;
use serde::Deserialize;

use super::{
    AppError, AppResult, AppState, Ctx, Flash, HostedCtx, Layout, UserExt, redirect_err, redirect_ok, render,
    require_edit,
};
use crate::hosted::{PacStripe, Person};
use crate::lunch::{self, DayCount, DayItem, Division, Item, LineView, Lunch, Offline, Order, Period, PeriodMoney};
use crate::money;

fn lunch_ctx(state: &Ctx) -> AppResult<(Arc<Lunch>, HostedCtx)> {
    match (state.lunch.clone(), state.hosted.clone()) {
        (Some(l), Some(h)) => Ok((l, h)),
        _ => Err(AppError::NotFound),
    }
}

/// The page, or a redirect to /lunch when the PAC has not turned hot lunch on.
async fn require_on(state: &Ctx) -> AppResult<Option<Response>> {
    lunch_ctx(state)?;
    if lunch::enabled(&state.pool).await? {
        Ok(None)
    } else {
        Ok(Some(redirect_err("/lunch", "Turn hot lunch on first.")))
    }
}

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// The API token of the PAC's own Helcim account, if the PAC connected one.
async fn helcim_token(l: &Lunch, h: &HostedCtx) -> anyhow::Result<Option<String>> {
    h.hosted.pac_helcim(h.pac.id).await?.map(|sealed| l.helcim.open(&sealed)).transpose()
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

// ----- Overview -----

#[derive(Template)]
#[template(path = "lunch/home.html")]
struct HomePage {
    layout: Layout,
    enabled: bool,
    stripe: Option<PacStripe>,
    /// The PAC takes cards through its own Helcim account.
    helcim: bool,
    offline: Offline,
    items: usize,
    divisions: usize,
    families: usize,
    periods: Vec<(Period, PeriodMoney)>,
    today: NaiveDate,
}

impl HomePage {
    fn can_pay(&self) -> bool {
        self.helcim
            || self.stripe.as_ref().is_some_and(|s| s.charges_enabled)
            || self.offline.etransfer()
            || self.offline.cash
    }
}

pub async fn home(state: Ctx, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    let (l, h) = lunch_ctx(&state)?;
    let pool = &state.pool;
    let mut periods = Vec::new();
    for p in lunch::periods(pool).await? {
        let money = lunch::period_money(&lunch::orders_for_period(pool, p.id).await?);
        periods.push((p, money));
    }
    render(HomePage {
        layout: Layout::new(&state, &user, "lunch", "Hot lunch", &flash).await?,
        enabled: lunch::enabled(pool).await?,
        stripe: h.hosted.pac_stripe(h.pac.id).await?.filter(|_| l.stripe.is_some()),
        helcim: h.hosted.pac_helcim(h.pac.id).await?.is_some(),
        offline: Offline::load(pool).await?,
        items: lunch::items(pool).await?.iter().filter(|i| i.active).count(),
        divisions: lunch::divisions(pool).await?.iter().filter(|d| d.active).count(),
        families: h.hosted.families(h.pac.id).await?.len(),
        periods,
        today: today(),
    })
}

#[derive(Deserialize)]
pub struct EnableForm {
    on: Option<String>,
}

pub async fn enable(state: Ctx, user: UserExt, Form(f): Form<EnableForm>) -> AppResult<Response> {
    require_edit(&user)?;
    lunch_ctx(&state)?;
    let on = f.on.as_deref() == Some("1");
    lunch::set_enabled(&state.pool, on).await?;
    if on {
        Ok(redirect_ok("/lunch", "Hot lunch is on. Set up the menu, the divisions, and payments, then open a period."))
    } else {
        Ok(redirect_ok("/settings", "Hot lunch is off. Parents cannot order, and the records stay."))
    }
}

// ----- Menu and divisions -----

#[derive(Template)]
#[template(path = "lunch/menu.html")]
struct MenuPage {
    layout: Layout,
    items: Vec<Item>,
    divisions: Vec<Division>,
}

pub async fn menu(state: Ctx, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    render(MenuPage {
        layout: Layout::new(&state, &user, "lunch", "Menu and divisions", &flash).await?,
        items: lunch::items(&state.pool).await?,
        divisions: lunch::divisions(&state.pool).await?,
    })
}

#[derive(Deserialize)]
pub struct ItemForm {
    name: String,
    #[serde(default)]
    description: String,
    price: String,
    active: Option<String>,
}

fn item_values(f: &ItemForm) -> Result<(String, i64), String> {
    let name = f.name.trim().to_string();
    if name.is_empty() {
        return Err("Enter a name for the item.".into());
    }
    match money::parse(&f.price) {
        Some(cents) if cents >= 0 => Ok((name, cents)),
        _ => Err("Enter a price, such as 5.50.".into()),
    }
}

pub async fn create_item(state: Ctx, user: UserExt, Form(f): Form<ItemForm>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (name, cents) = match item_values(&f) {
        Ok(v) => v,
        Err(e) => return Ok(redirect_err("/lunch/menu", &e)),
    };
    sqlx::query("INSERT INTO lunch_items (name, description, price_cents) VALUES (?, ?, ?)")
        .bind(&name)
        .bind(f.description.trim())
        .bind(cents)
        .execute(&state.pool)
        .await?;
    Ok(redirect_ok("/lunch/menu", &format!("{name} added to the menu.")))
}

/// Changes an item. A new price applies to days added later; days already set
/// up keep their prices, so orders stay as parents saw them.
pub async fn update_item(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<ItemForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (name, cents) = match item_values(&f) {
        Ok(v) => v,
        Err(e) => return Ok(redirect_err("/lunch/menu", &e)),
    };
    sqlx::query("UPDATE lunch_items SET name = ?, description = ?, price_cents = ?, active = ? WHERE id = ?")
        .bind(&name)
        .bind(f.description.trim())
        .bind(cents)
        .bind(f.active.is_some())
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(redirect_ok("/lunch/menu", &format!("{name} saved. The new price applies to days you add from now on.")))
}

#[derive(Deserialize)]
pub struct DivisionForm {
    name: String,
    #[serde(default)]
    sort_order: String,
    active: Option<String>,
}

pub async fn create_division(state: Ctx, user: UserExt, Form(f): Form<DivisionForm>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let name = f.name.trim();
    if name.is_empty() {
        return Ok(redirect_err("/lunch/menu", "Enter the division's name, such as Div 3 – Ms. Lee."));
    }
    let done = sqlx::query("INSERT OR IGNORE INTO lunch_divisions (name, sort_order) VALUES (?, ?)")
        .bind(name)
        .bind(f.sort_order.trim().parse::<i64>().unwrap_or(0))
        .execute(&state.pool)
        .await?;
    if done.rows_affected() == 0 {
        return Ok(redirect_err("/lunch/menu", &format!("There is already a division named {name}.")));
    }
    Ok(redirect_ok("/lunch/menu", &format!("{name} added.")))
}

pub async fn update_division(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<DivisionForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let name = f.name.trim();
    if name.is_empty() {
        return Ok(redirect_err("/lunch/menu", "Enter the division's name."));
    }
    let done = sqlx::query("UPDATE OR IGNORE lunch_divisions SET name = ?, sort_order = ?, active = ? WHERE id = ?")
        .bind(name)
        .bind(f.sort_order.trim().parse::<i64>().unwrap_or(0))
        .bind(f.active.is_some())
        .bind(id)
        .execute(&state.pool)
        .await?;
    if done.rows_affected() == 0 {
        return Ok(redirect_err("/lunch/menu", &format!("There is already a division named {name}.")));
    }
    Ok(redirect_ok("/lunch/menu", &format!("{name} saved.")))
}

// ----- Periods -----

#[derive(Deserialize)]
pub struct NewPeriodForm {
    name: String,
    order_deadline: String,
}

pub async fn create_period(state: Ctx, user: UserExt, Form(f): Form<NewPeriodForm>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let name = f.name.trim();
    let Some(deadline) = parse_date(&f.order_deadline) else {
        return Ok(redirect_err("/lunch", "Enter the last day to order."));
    };
    if name.is_empty() {
        return Ok(redirect_err("/lunch", "Enter a name for the period, such as January 2027."));
    }
    let id = sqlx::query("INSERT INTO lunch_periods (name, order_deadline, created_at) VALUES (?, ?, ?)")
        .bind(name)
        .bind(deadline)
        .bind(crate::db::now())
        .execute(&state.pool)
        .await?
        .last_insert_rowid();
    Ok(redirect_ok(&format!("/lunch/periods/{id}"), "Period added. Add its lunch days, then open it for orders."))
}

#[derive(Template)]
#[template(path = "lunch/period.html")]
struct PeriodPage {
    layout: Layout,
    period: Period,
    days: Vec<(lunch::Day, Vec<DayItem>)>,
    items: Vec<Item>,
    counts: Vec<DayCount>,
    orders: Vec<(Order, String)>,
    money: PeriodMoney,
}

impl PeriodPage {
    fn on_day(&self, menu: &[DayItem], item_id: &i64) -> bool {
        menu.iter().any(|m| m.item_id == *item_id)
    }

    fn is_status(&self, s: &&str) -> bool {
        self.period.status == *s
    }
}

/// "Name <email>" for each parent who ordered, from control.db.
async fn parent_labels(h: &HostedCtx, orders: &[Order]) -> AppResult<HashMap<i64, Person>> {
    let ids: Vec<i64> = orders.iter().map(|o| o.person_id).collect();
    Ok(h.hosted.people(&ids).await?)
}

pub async fn period(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Query(flash): Query<Flash>,
) -> AppResult<Response> {
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (_, h) = lunch_ctx(&state)?;
    let pool = &state.pool;
    let period = lunch::period(pool, id).await?.ok_or(AppError::NotFound)?;
    let orders = lunch::orders_for_period(pool, id).await?;
    let people = parent_labels(&h, &orders).await?;
    let money = lunch::period_money(&orders);
    let orders = orders
        .into_iter()
        .map(|o| {
            let who = people
                .get(&o.person_id)
                .map(|p| format!("{} <{}>", p.name, p.email))
                .unwrap_or_else(|| "a parent who left".into());
            (o, who)
        })
        .collect();
    render(PeriodPage {
        layout: Layout::new(&state, &user, "lunch", period.name.clone(), &flash).await?,
        days: lunch::days(pool, id).await?,
        items: lunch::items(pool).await?.into_iter().filter(|i| i.active).collect(),
        counts: lunch::day_counts(pool, id).await?,
        orders,
        money,
        period,
    })
}

#[derive(Deserialize)]
pub struct PeriodForm {
    name: String,
    order_deadline: String,
    #[serde(default)]
    notes: String,
    status: String,
}

pub async fn update_period(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<PeriodForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let back = format!("/lunch/periods/{id}");
    lunch::period(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    let Some(deadline) = parse_date(&f.order_deadline) else {
        return Ok(redirect_err(&back, "Enter the last day to order."));
    };
    if f.name.trim().is_empty() || !matches!(f.status.as_str(), "draft" | "open" | "closed") {
        return Ok(redirect_err(&back, "Enter a name and choose a status."));
    }
    if f.status == "open" {
        let days = lunch::days(&state.pool, id).await?;
        if days.iter().all(|(_, menu)| menu.is_empty()) {
            return Ok(redirect_err(&back, "Add lunch days with items before you open the period."));
        }
    }
    sqlx::query("UPDATE lunch_periods SET name = ?, order_deadline = ?, notes = ?, status = ? WHERE id = ?")
        .bind(f.name.trim())
        .bind(deadline)
        .bind(f.notes.trim())
        .bind(&f.status)
        .bind(id)
        .execute(&state.pool)
        .await?;
    Ok(redirect_ok(&back, "Period saved."))
}

#[derive(Deserialize)]
pub struct DaysForm {
    from: String,
    to: String,
    #[serde(default)]
    weekday: Vec<u32>,
    #[serde(default)]
    item_id: Vec<i64>,
}

pub async fn add_days(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<DaysForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let back = format!("/lunch/periods/{id}");
    lunch::period(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    let (Some(from), Some(to)) = (parse_date(&f.from), parse_date(&f.to)) else {
        return Ok(redirect_err(&back, "Enter the first and last dates."));
    };
    if to < from || (to - from).num_days() > 120 {
        return Ok(redirect_err(&back, "The last date must be on or after the first, and within four months."));
    }
    if f.weekday.is_empty() || f.item_id.is_empty() {
        return Ok(redirect_err(&back, "Choose the weekdays and the items to offer."));
    }
    let added = lunch::add_days(&state.pool, id, from, to, &f.weekday, &f.item_id).await?;
    Ok(redirect_ok(&back, &format!("{added} lunch day(s) added.")))
}

#[derive(Deserialize)]
pub struct DayItemsForm {
    #[serde(default)]
    item_id: Vec<i64>,
}

async fn day_period(state: &Ctx, day_id: i64) -> AppResult<i64> {
    sqlx::query_scalar("SELECT period_id FROM lunch_days WHERE id = ?")
        .bind(day_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn set_day(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(f): Form<DayItemsForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let period_id = day_period(&state, id).await?;
    lunch::set_day_items(&state.pool, id, &f.item_id).await?;
    Ok(redirect_ok(&format!("/lunch/periods/{period_id}"), "Menu saved. Items already ordered that day stay on it."))
}

pub async fn delete_day(state: Ctx, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let period_id = day_period(&state, id).await?;
    let back = format!("/lunch/periods/{period_id}");
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lunch_order_lines WHERE day_id = ?")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    if used > 0 {
        return Ok(redirect_err(&back, "Parents have ordered lunches that day. Cancel or refund those orders first."));
    }
    sqlx::query("DELETE FROM lunch_days WHERE id = ?").bind(id).execute(&state.pool).await?;
    Ok(redirect_ok(&back, "Lunch day removed."))
}

/// One division on a delivery sheet: its item totals and its lunches.
type SheetGroup = (String, Vec<(String, i64)>, Vec<LineView>);

#[derive(Template)]
#[template(path = "lunch/sheet.html")]
struct SheetPage {
    pac_name: String,
    period: Period,
    day: lunch::Day,
    count: DayCount,
    /// Each division: its item totals and its lunches, child by child.
    groups: Vec<SheetGroup>,
}

/// The delivery sheet for one day: each division's lunches, child by child.
pub async fn sheet(state: Ctx, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let pool = &state.pool;
    let period_id = day_period(&state, id).await?;
    let period = lunch::period(pool, period_id).await?.ok_or(AppError::NotFound)?;
    let count = lunch::day_counts(pool, period_id)
        .await?
        .into_iter()
        .find(|c| c.day.as_ref().is_some_and(|d| d.id == id))
        .unwrap_or_default();
    let day = count.day.clone().ok_or(AppError::NotFound)?;
    let lines = lunch::day_sheet(pool, id).await?;
    let groups = count
        .divisions
        .iter()
        .map(|(division, items)| {
            let own: Vec<LineView> = lines
                .iter()
                .filter(|l| l.division_name.as_deref().unwrap_or("No division") == division)
                .cloned()
                .collect();
            (division.clone(), items.clone(), own)
        })
        .collect();
    render(SheetPage { pac_name: crate::db::Settings::load(pool).await?.pac_name, period, day, count, groups })
}

// ----- Orders -----

#[derive(Template)]
#[template(path = "lunch/order.html")]
struct OrderPage {
    layout: Layout,
    order: Order,
    period: Period,
    parent: String,
    lines: Vec<LineView>,
}

pub async fn order(
    state: Ctx,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Query(flash): Query<Flash>,
) -> AppResult<Response> {
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    let pool = &state.pool;
    let mut order = lunch::order(pool, id).await?.ok_or(AppError::NotFound)?;
    // A Helcim payment that nobody reported yet.
    if order.status == "pending"
        && order.helcim_invoice.is_some()
        && let Some(token) = helcim_token(&l, &h).await?
    {
        match super::family::confirm_helcim(&h.hosted, &l.helcim, &h.pac, pool, &order, &token).await {
            Ok(true) => order = lunch::order(pool, id).await?.ok_or(AppError::NotFound)?,
            Ok(false) => {}
            Err(e) => tracing::warn!("could not check the Helcim payment of order {id}: {e:#}"),
        }
    }
    let period = lunch::period(pool, order.period_id).await?.ok_or(AppError::NotFound)?;
    let parent = match h.hosted.person(order.person_id).await? {
        Some(p) => format!("{} <{}>", p.name, p.email),
        None => "a parent who left".into(),
    };
    render(OrderPage {
        layout: Layout::new(&state, &user, "lunch", order.reference(), &flash).await?,
        lines: lunch::order_lines(pool, id).await?,
        order,
        period,
        parent,
    })
}

/// The treasurer marks an e-transfer or cash order paid.
pub async fn mark_paid(state: Ctx, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let back = format!("/lunch/orders/{id}");
    let order = lunch::order(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    if order.method == "card" {
        return Ok(redirect_err(&back, "Card payments are confirmed by the card company, not by hand."));
    }
    if lunch::mark_paid(&state.pool, id, None).await? {
        let (_, h) = lunch_ctx(&state)?;
        super::family::send_receipt(&h.hosted, &h.pac, &state.pool, &order).await?;
        Ok(redirect_ok(&back, &format!("{} is paid. The parent gets a receipt by email.", order.reference())))
    } else {
        Ok(redirect_err(&back, "Only an order that waits for payment can be marked paid."))
    }
}

pub async fn cancel(state: Ctx, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let back = format!("/lunch/orders/{id}");
    if lunch::close_order(&state.pool, id, "cancelled").await? {
        Ok(redirect_ok(&back, "Order cancelled."))
    } else {
        Ok(redirect_err(&back, "Only an order that is not paid can be cancelled. Refund a paid order."))
    }
}

/// Refunds a paid order: a card payment through Stripe or Helcim, or an
/// e-transfer or cash payment that the treasurer pays back outside PAC Keeper.
pub async fn refund(state: Ctx, user: UserExt, headers: HeaderMap, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    let back = format!("/lunch/orders/{id}");
    let order = lunch::order(&state.pool, id).await?.ok_or(AppError::NotFound)?;
    if order.status != "paid" {
        return Ok(redirect_err(&back, "Only a paid order can be refunded."));
    }
    if let Some(transaction) = order.helcim_transaction.as_deref() {
        let Some(token) = helcim_token(&l, &h).await? else {
            return Ok(redirect_err(&back, "Helcim is not connected. Refund this payment in Helcim."));
        };
        // Helcim asks for the address of whoever asks for the refund.
        let ip = match super::hosted::client_ip(&headers) {
            ip if ip.parse::<std::net::IpAddr>().is_ok() => ip,
            _ => "127.0.0.1".to_string(),
        };
        if let Err(e) = l.helcim.refund(&token, transaction, order.total_cents, &ip).await {
            tracing::error!("Helcim refund of order {id} failed: {e:#}");
            return Ok(redirect_err(&back, &format!("Helcim did not refund it: {e}")));
        }
    } else if order.method == "card" {
        let (Some(stripe), Some(ps), Some(pi)) =
            (l.stripe.as_ref(), h.hosted.pac_stripe(h.pac.id).await?, order.stripe_payment_intent.as_deref())
        else {
            return Ok(redirect_err(&back, "This card payment cannot be refunded from here. Refund it in Stripe."));
        };
        if let Err(e) = stripe.refund(&ps.destination(), pi).await {
            tracing::error!("refund of order {id} failed: {e:#}");
            return Ok(redirect_err(&back, &format!("Stripe did not refund it: {e}")));
        }
    }
    lunch::close_order(&state.pool, id, "refunded").await?;
    let msg = if order.method == "card" {
        "Refunded to the parent's card. The fees are not returned."
    } else {
        "Marked refunded. Pay the money back to the parent yourself."
    };
    Ok(redirect_ok(&back, msg))
}

// ----- Payments and families -----

#[derive(Template)]
#[template(path = "lunch/setup.html")]
struct SetupPage {
    layout: Layout,
    /// The server has the platform's Stripe keys.
    cards_available: bool,
    can_connect_own: bool,
    stripe: Option<PacStripe>,
    /// The PAC takes cards through its own Helcim account.
    helcim: bool,
    express_terms: String,
    /// The weekday of the weekly payout to an Express account.
    payout_day: &'static str,
    standard_terms: Option<String>,
    offline: Offline,
    join_url: String,
    families: Vec<Person>,
}

pub async fn setup(state: Ctx, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    let code = h.hosted.family_code(h.pac.id).await?;
    render(SetupPage {
        layout: Layout::new(&state, &user, "lunch", "Payments and families", &flash).await?,
        cards_available: l.stripe.is_some(),
        can_connect_own: l.stripe.as_ref().is_some_and(|s| s.client_id.is_some()),
        stripe: h.hosted.pac_stripe(h.pac.id).await?,
        helcim: h.hosted.pac_helcim(h.pac.id).await?.is_some(),
        express_terms: l.fees.express_terms(),
        payout_day: l.fees.payout_day_name(),
        standard_terms: l.fees.standard_terms(),
        offline: Offline::load(&state.pool).await?,
        join_url: format!("{}/join/{code}", h.hosted.base_url),
        families: h.hosted.families(h.pac.id).await?,
    })
}

#[derive(Deserialize)]
pub struct OfflineForm {
    #[serde(default)]
    etransfer_email: String,
    #[serde(default)]
    etransfer_note: String,
    cash: Option<String>,
}

pub async fn save_offline(state: Ctx, user: UserExt, Form(f): Form<OfflineForm>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let email = f.etransfer_email.trim();
    if !email.is_empty() && !crate::hosted::looks_like_email(email) {
        return Ok(redirect_err("/lunch/setup", "Enter the e-transfer address as an email address."));
    }
    Offline { etransfer_email: email.into(), etransfer_note: f.etransfer_note, cash: f.cash.is_some() }
        .save(&state.pool)
        .await?;
    Ok(redirect_ok("/lunch/setup", "Payment choices saved."))
}

/// Makes the PAC an Express account under the platform (once), then sends
/// the treasurer to Stripe to finish it.
pub async fn express(state: Ctx, user: UserExt) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    let Some(stripe) = l.stripe.as_ref() else {
        return Ok(redirect_err("/lunch/setup", "Card payments are not set up on this server."));
    };
    if h.hosted.pac_helcim(h.pac.id).await?.is_some() {
        return Ok(redirect_err("/lunch/setup", HELCIM_FIRST));
    }
    let account = match h.hosted.pac_stripe(h.pac.id).await? {
        Some(ps) if ps.is_express() => ps.account_id,
        Some(_) => {
            return Ok(redirect_err("/lunch/setup", "This PAC uses its own Stripe account. Disconnect it first."));
        }
        None => match stripe.create_express_account(&h.person.email, &h.pac.name, &l.fees.payout_day_key()).await {
            Ok(id) => {
                h.hosted.set_pac_stripe(h.pac.id, &id, "express", false).await?;
                id
            }
            Err(e) => return Ok(redirect_err("/lunch/setup", &format!("Stripe did not make the account: {e}"))),
        },
    };
    onboarding(stripe, &h, &account).await
}

async fn onboarding(stripe: &lunch::stripe::Stripe, h: &HostedCtx, account: &str) -> AppResult<Response> {
    let base = &h.hosted.base_url;
    match stripe
        .onboarding_link(
            account,
            &format!("{base}/lunch/setup/express/return"),
            &format!("{base}/lunch/setup/express/refresh"),
        )
        .await
    {
        Ok(url) => Ok(Redirect::to(&url).into_response()),
        Err(e) => Ok(redirect_err("/lunch/setup", &format!("Stripe did not open the set-up page: {e}"))),
    }
}

/// Stripe sends the treasurer back here. The account may still need more.
pub async fn express_return(state: Ctx) -> AppResult<Response> {
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    let (Some(stripe), Some(ps)) = (l.stripe.as_ref(), h.hosted.pac_stripe(h.pac.id).await?) else {
        return Ok(Redirect::to("/lunch/setup").into_response());
    };
    // Accounts made before weekly payouts get the weekly schedule here.
    if ps.is_express()
        && let Err(e) = stripe.set_weekly_payouts(&ps.account_id, &l.fees.payout_day_key()).await
    {
        tracing::warn!("could not set weekly payouts for {}: {e:#}", ps.account_id);
    }
    match stripe.account_status(&ps.account_id).await {
        Ok(status) => {
            h.hosted.set_pac_stripe(h.pac.id, &ps.account_id, &ps.kind, status.charges_enabled).await?;
            if status.charges_enabled {
                Ok(redirect_ok("/lunch/setup", "Card payments are on. Parents can now pay by card."))
            } else {
                Ok(redirect_err(
                    "/lunch/setup",
                    "Stripe needs more details before the PAC can take cards. Continue the set-up.",
                ))
            }
        }
        Err(e) => Ok(redirect_err("/lunch/setup", &format!("Stripe did not answer: {e}"))),
    }
}

/// A set-up link expired: make a new one.
pub async fn express_refresh(state: Ctx) -> AppResult<Response> {
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    match (l.stripe.as_ref(), h.hosted.pac_stripe(h.pac.id).await?) {
        (Some(stripe), Some(ps)) if ps.is_express() => onboarding(stripe, &h, &ps.account_id).await,
        _ => Ok(Redirect::to("/lunch/setup").into_response()),
    }
}

const OAUTH_COOKIE: &str = "pk_stripe_oauth";

/// Sends the treasurer to Stripe to connect the PAC's own account.
pub async fn standard(state: Ctx, user: UserExt, jar: PrivateCookieJar) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    if h.hosted.pac_helcim(h.pac.id).await?.is_some() {
        return Ok(redirect_err("/lunch/setup", HELCIM_FIRST));
    }
    let mut bytes = [0u8; 18];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut bytes);
    let token = hex::encode(bytes);
    let redirect_uri = format!("{}/lunch/setup/standard/return", h.hosted.base_url);
    let Some(url) = l.stripe.as_ref().and_then(|s| s.oauth_url(&token, &redirect_uri)) else {
        return Ok(redirect_err("/lunch/setup", "Connecting your own Stripe is not set up on this server."));
    };
    let cookie = Cookie::build((OAUTH_COOKIE, format!("{}:{token}", h.pac.id)))
        .path("/lunch/setup")
        .http_only(true)
        .secure(h.hosted.secure_cookies())
        .same_site(SameSite::Lax)
        .max_age(time::Duration::minutes(15))
        .build();
    Ok((jar.add(cookie), Redirect::to(&url)).into_response())
}

#[derive(Deserialize)]
pub struct OAuthReturn {
    code: Option<String>,
    state: Option<String>,
    error_description: Option<String>,
}

pub async fn standard_return(
    state: Ctx,
    user: UserExt,
    jar: PrivateCookieJar,
    Query(q): Query<OAuthReturn>,
) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    let expected = jar.get(OAUTH_COOKIE).map(|c| c.value().to_string());
    let jar = jar.remove(Cookie::build(OAUTH_COOKIE).path("/lunch/setup"));
    if let Some(e) = q.error_description {
        return Ok(
            (jar, redirect_err("/lunch/setup", &format!("Stripe did not connect the account: {e}"))).into_response()
        );
    }
    let given = format!("{}:{}", h.pac.id, q.state.unwrap_or_default());
    let (Some(stripe), Some(code), true) = (l.stripe.as_ref(), q.code, expected.as_deref() == Some(given.as_str()))
    else {
        return Ok(
            (jar, redirect_err("/lunch/setup", "That link is not valid. Start again with Connect.")).into_response()
        );
    };
    match stripe.oauth_account(&code).await {
        Ok(account) => {
            let enabled = stripe.account_status(&account).await.map(|s| s.charges_enabled).unwrap_or(false);
            h.hosted.set_pac_stripe(h.pac.id, &account, "standard", enabled).await?;
            Ok((jar, redirect_ok("/lunch/setup", "Your Stripe account is connected.")).into_response())
        }
        Err(e) => {
            Ok((jar, redirect_err("/lunch/setup", &format!("Stripe did not connect the account: {e}"))).into_response())
        }
    }
}

/// Stops card payments for the PAC. The Stripe account itself stays.
pub async fn remove_stripe(state: Ctx, user: UserExt) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (_, h) = lunch_ctx(&state)?;
    h.hosted.remove_pac_stripe(h.pac.id).await?;
    Ok(redirect_ok("/lunch/setup", "Card payments are off for this PAC. The Stripe account itself stays as it is."))
}

const HELCIM_FIRST: &str = "This PAC takes cards through Helcim. Stop that first.";

#[derive(Deserialize)]
pub struct HelcimForm {
    #[serde(default)]
    token: String,
}

/// Connects the PAC's own Helcim account by its API token. The server checks
/// the token with Helcim, then keeps it sealed.
pub async fn connect_helcim(state: Ctx, user: UserExt, Form(f): Form<HelcimForm>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (l, h) = lunch_ctx(&state)?;
    if h.hosted.pac_stripe(h.pac.id).await?.is_some() {
        return Ok(redirect_err("/lunch/setup", "This PAC takes cards through Stripe. Stop that first."));
    }
    let token = f.token.trim();
    if token.is_empty() {
        return Ok(redirect_err("/lunch/setup", "Paste the API token from Helcim."));
    }
    if let Err(e) = l.helcim.check_token(token).await {
        return Ok(redirect_err("/lunch/setup", &format!("Helcim did not accept the token: {e}")));
    }
    h.hosted.set_pac_helcim(h.pac.id, &l.helcim.seal(token)?).await?;
    Ok(redirect_ok("/lunch/setup", "Helcim is connected. Parents can now pay by card."))
}

/// Stops card payments through Helcim. PAC Keeper forgets the token.
pub async fn remove_helcim(state: Ctx, user: UserExt) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (_, h) = lunch_ctx(&state)?;
    h.hosted.remove_pac_helcim(h.pac.id).await?;
    Ok(redirect_ok(
        "/lunch/setup",
        "Card payments through Helcim are off. PAC Keeper no longer keeps the API token. Delete it in Helcim too.",
    ))
}

pub async fn new_code(state: Ctx, user: UserExt) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (_, h) = lunch_ctx(&state)?;
    h.hosted.new_family_code(h.pac.id).await?;
    Ok(redirect_ok("/lunch/setup", "New sign-up link made. The old link no longer works."))
}

pub async fn remove_family(state: Ctx, user: UserExt, UrlPath(person): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(r) = require_on(&state).await? {
        return Ok(r);
    }
    let (_, h) = lunch_ctx(&state)?;
    h.hosted.remove_family(person, h.pac.id).await?;
    Ok(redirect_ok("/lunch/setup", "The parent can no longer order. Their past orders stay."))
}

// ----- Stripe webhook -----

/// Stripe reports payments here. Only signed events count. An error answer
/// makes Stripe try again later.
pub async fn stripe_webhook(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let (Some(l), Some(hosted)) = (app.lunch.clone(), app.hosted.clone()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(stripe) = l.stripe.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let signature = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    let event = match stripe.verify_webhook(&body, signature, chrono::Utc::now().timestamp()) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("refused a Stripe webhook: {e:#}");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };
    let kind = event["type"].as_str().unwrap_or("");
    if !matches!(kind, "checkout.session.completed" | "checkout.session.async_payment_succeeded") {
        return StatusCode::OK.into_response();
    }
    let status = lunch::stripe::SessionStatus::from_json(&event["data"]["object"]);
    match super::family::confirm_from_stripe(&hosted, event["account"].as_str(), &status).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(e) => {
            tracing::error!("could not record a Stripe payment: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
