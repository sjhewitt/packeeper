//! Hot lunch for parents: join a PAC by its sign-up link, add children, order
//! lunches for a period, and pay by card (Stripe Checkout or the PAC's own
//! Helcim account), e-transfer, or cash. Parents never see the PAC's books: the guard sends them only to
//! these pages (see `hosted::guard` and `FamilyCtx`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use askama::Template;
use axum::extract::{FromRequestParts, Path as UrlPath, State};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::PrivateCookieJar;
use axum_extra::extract::{Form, Query};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::SqlitePool;

use super::hosted::{MIN_PASSWORD, client_ip, hosted_or_404, notice, send_mail, send_verification, with_session};
use super::{AppError, AppResult, AppState, Flash, redirect_err, redirect_ok, render};
use crate::hosted::{Hosted, Pac, PacStripe, Person, looks_like_email};
use crate::lunch::helcim::Helcim;
use crate::lunch::stripe::{CheckoutRequest, SessionStatus};
use crate::lunch::{self, Child, Choice, DayItem, Division, LineView, Lunch, Offline, Order, Period};
use crate::money::Money;

/// What a parent page works with. The guard puts it on the request after it
/// checks that the person is a parent of the session's PAC, the PAC is
/// active, and the PAC has hot lunch on.
#[derive(Clone)]
pub struct FamilyCtx {
    pub hosted: Arc<Hosted>,
    pub lunch: Arc<Lunch>,
    pub pac: Pac,
    pub person: Person,
    pub pool: SqlitePool,
}

impl<S: Send + Sync> FromRequestParts<S> for FamilyCtx {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<FamilyCtx, AppError> {
        parts.extensions.get::<FamilyCtx>().cloned().ok_or(AppError::NotFound)
    }
}

fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
}

/// How a PAC takes cards.
enum Cards {
    /// The server has Stripe, and the PAC's account takes charges.
    Stripe(PacStripe),
    /// The PAC's own Helcim account, with its API token.
    Helcim(String),
}

async fn cards(hosted: &Hosted, lunch: &Lunch, pac_id: i64) -> anyhow::Result<Option<Cards>> {
    if let Some(sealed) = hosted.pac_helcim(pac_id).await? {
        return match lunch.helcim.open(&sealed) {
            Ok(token) => Ok(Some(Cards::Helcim(token))),
            Err(e) => {
                tracing::warn!("PAC {pac_id}: {e:#}");
                Ok(None)
            }
        };
    }
    Ok(match hosted.pac_stripe(pac_id).await? {
        Some(ps) if lunch.stripe.is_some() && ps.charges_enabled => Some(Cards::Stripe(ps)),
        _ => None,
    })
}

async fn takes_cards(f: &FamilyCtx) -> AppResult<bool> {
    Ok(cards(&f.hosted, &f.lunch, f.pac.id).await?.is_some())
}

// ----- Joining -----

#[derive(Template)]
#[template(path = "family/join.html")]
struct JoinPage {
    pac: Pac,
    code: String,
    person: Option<Person>,
    error: Option<String>,
    name: String,
    email: String,
}

/// The PAC behind a sign-up link, if the link works and the PAC takes orders.
async fn join_pac(app: &AppState, code: &str) -> AppResult<(Arc<Hosted>, Pac)> {
    let hosted = hosted_or_404(app)?;
    app.lunch.as_ref().ok_or(AppError::NotFound)?;
    let pac = hosted.pac_by_family_code(code).await?.filter(|p| p.is_active()).ok_or(AppError::NotFound)?;
    if !lunch::enabled(&hosted.pac_pool(&pac).await?).await? {
        return Err(AppError::NotFound);
    }
    Ok((hosted, pac))
}

pub async fn join_page(
    State(app): State<AppState>,
    UrlPath(code): UrlPath<String>,
    person: Option<axum::Extension<Person>>,
) -> AppResult<Response> {
    let (_, pac) = join_pac(&app, &code).await?;
    render(JoinPage { pac, code, person: person.map(|p| p.0), error: None, name: String::new(), email: String::new() })
}

#[derive(Deserialize, Default)]
pub struct JoinForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
}

pub async fn join(
    State(app): State<AppState>,
    UrlPath(code): UrlPath<String>,
    jar: PrivateCookieJar,
    headers: HeaderMap,
    person: Option<axum::Extension<Person>>,
    Form(mut f): Form<JoinForm>,
) -> AppResult<Response> {
    let (hosted, pac) = join_pac(&app, &code).await?;
    // Signed in already: add the PAC to the account.
    if let Some(axum::Extension(person)) = person {
        hosted.add_family(person.id, pac.id).await?;
        let jar = with_session(jar, &hosted, &person, Some(pac.id));
        return Ok((jar, Redirect::to("/family")).into_response());
    }
    f.password.truncate(200);
    let error = |f: JoinForm, e: &str| {
        render(JoinPage {
            pac: pac.clone(),
            code: code.clone(),
            person: None,
            error: Some(e.into()),
            name: f.name,
            email: f.email,
        })
    };
    if !hosted.allow(&format!("signup:{}", client_ip(&headers)), 5, Duration::from_secs(3600)).await {
        return error(f, "Too many sign-ups from this network. Try again in an hour.");
    }
    if f.name.trim().is_empty() || !looks_like_email(&f.email) {
        return error(f, "Enter your name and a valid email address.");
    }
    if f.password.chars().count() < MIN_PASSWORD {
        return error(f, &format!("Use a password of at least {MIN_PASSWORD} characters."));
    }
    let email = f.email.trim().to_lowercase();
    if let Some(p) = hosted.person_by_email(&email).await?
        && p.password_hash.is_some()
    {
        return error(f, "There is already an account with this email. Sign in, then open this link again.");
    }
    let person = hosted.ensure_person(&email, f.name.trim()).await?;
    sqlx::query("UPDATE people SET name = ? WHERE id = ?")
        .bind(f.name.trim())
        .bind(person.id)
        .execute(&hosted.control)
        .await?;
    hosted.set_password(person.id, &f.password).await?;
    hosted.add_family(person.id, pac.id).await?;
    if !send_verification(&hosted, &person.email, f.name.trim(), person.id).await? {
        return notice(
            "We could not send the email",
            &format!(
                "We made your account, but the email to confirm {email} could not be sent. In a few minutes, sign in and choose \"Send the confirmation link again\"."
            ),
            Some(("/login", "Sign in")),
        );
    }
    notice(
        "Check your email",
        &format!(
            "We sent a link to {email}. Follow it to confirm your address, then sign in to order hot lunch from {}.",
            pac.name
        ),
        Some(("/login", "Sign in")),
    )
}

/// Opens a PAC's hot lunch pages from the list of the person's PACs.
pub async fn open(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    person: Option<axum::Extension<Person>>,
    UrlPath(pac_id): UrlPath<i64>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    let Some(axum::Extension(person)) = person else {
        return Ok(Redirect::to("/login").into_response());
    };
    match hosted.family_pac(person.id, pac_id).await? {
        Some(pac) if pac.is_active() => {
            Ok((with_session(jar, &hosted, &person, Some(pac.id)), Redirect::to("/family")).into_response())
        }
        _ => Err(AppError::NotFound),
    }
}

// ----- Home -----

#[derive(Template)]
#[template(path = "family/home.html")]
struct HomePage {
    f: FamilyCtx,
    children: Vec<Child>,
    divisions: Vec<Division>,
    periods: Vec<Period>,
    orders: Vec<(Order, String)>,
    flash: Flash,
}

pub async fn home(f: FamilyCtx, Query(flash): Query<Flash>) -> AppResult<Response> {
    let today = today();
    let periods: Vec<Period> = lunch::periods(&f.pool).await?.into_iter().filter(|p| p.takes_orders(today)).collect();
    let all_periods: HashMap<i64, String> =
        lunch::periods(&f.pool).await?.into_iter().map(|p| (p.id, p.name)).collect();
    let orders = lunch::orders_for_person(&f.pool, f.person.id)
        .await?
        .into_iter()
        .map(|o| {
            let name = all_periods.get(&o.period_id).cloned().unwrap_or_default();
            (o, name)
        })
        .collect();
    render(HomePage {
        children: lunch::children(&f.pool, f.person.id).await?,
        divisions: lunch::divisions(&f.pool).await?.into_iter().filter(|d| d.active).collect(),
        periods,
        orders,
        flash,
        f,
    })
}

#[derive(Deserialize)]
pub struct ChildForm {
    name: String,
    division_id: Option<String>,
    #[serde(default)]
    notes: String,
    active: Option<String>,
}

async fn division_of(pool: &SqlitePool, raw: &Option<String>) -> AppResult<Option<i64>> {
    let Some(id) = super::opt_id(raw) else { return Ok(None) };
    let found: Option<i64> = sqlx::query_scalar("SELECT id FROM lunch_divisions WHERE id = ? AND active = 1")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(found)
}

pub async fn add_child(f: FamilyCtx, Form(c): Form<ChildForm>) -> AppResult<Response> {
    let name = c.name.trim();
    if name.is_empty() {
        return Ok(redirect_err("/family", "Enter your child's name."));
    }
    let division = division_of(&f.pool, &c.division_id).await?;
    if division.is_none() {
        return Ok(redirect_err("/family", "Choose your child's division, so the lunch reaches the right class."));
    }
    sqlx::query("INSERT INTO lunch_children (person_id, name, division_id, notes, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(f.person.id)
        .bind(name)
        .bind(division)
        .bind(c.notes.trim())
        .bind(crate::db::now())
        .execute(&f.pool)
        .await?;
    Ok(redirect_ok("/family", &format!("{name} added.")))
}

pub async fn update_child(f: FamilyCtx, UrlPath(id): UrlPath<i64>, Form(c): Form<ChildForm>) -> AppResult<Response> {
    let name = c.name.trim();
    if name.is_empty() {
        return Ok(redirect_err("/family", "Enter your child's name."));
    }
    let division = division_of(&f.pool, &c.division_id).await?;
    // Only the parent's own child.
    let done = sqlx::query(
        "UPDATE lunch_children SET name = ?, division_id = COALESCE(?, division_id), notes = ?, active = ? WHERE id = ? AND person_id = ?",
    )
    .bind(name)
    .bind(division)
    .bind(c.notes.trim())
    .bind(c.active.is_some())
    .bind(id)
    .bind(f.person.id)
    .execute(&f.pool)
    .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(redirect_ok("/family", &format!("{name} saved.")))
}

// ----- Ordering -----

#[derive(Template)]
#[template(path = "family/order_form.html")]
struct OrderForm {
    f: FamilyCtx,
    period: Period,
    days: Vec<(lunch::Day, Vec<DayItem>)>,
    children: Vec<Child>,
    offline: Offline,
    cards: bool,
    /// Where the parent types the card, such as "on Stripe's secure page".
    card_page: &'static str,
    smallest_card: Money,
    error: Option<String>,
    picked: HashMap<String, String>,
}

impl OrderForm {
    fn field(&self, child: &i64, day: &i64) -> String {
        format!("c{child}_d{day}")
    }

    fn is_picked(&self, child: &i64, day: &i64, item: &i64) -> bool {
        self.picked.get(&self.field(child, day)).is_some_and(|v| *v == item.to_string())
    }
}

async fn order_form(
    f: FamilyCtx,
    period: Period,
    error: Option<String>,
    picked: HashMap<String, String>,
) -> AppResult<Response> {
    let way = cards(&f.hosted, &f.lunch, f.pac.id).await?;
    let (card_page, smallest_card) = match way {
        Some(Cards::Helcim(_)) => ("in Helcim's secure form", Money(HELCIM_SMALLEST)),
        _ => ("on Stripe's secure page", Money(f.lunch.fees.smallest_card_amount(Some(today())))),
    };
    render(OrderForm {
        days: lunch::days(&f.pool, period.id).await?.into_iter().filter(|(_, menu)| !menu.is_empty()).collect(),
        children: lunch::children(&f.pool, f.person.id).await?.into_iter().filter(|c| c.active).collect(),
        offline: Offline::load(&f.pool).await?,
        cards: way.is_some(),
        card_page,
        smallest_card,
        period,
        error,
        picked,
        f,
    })
}

async fn open_period(f: &FamilyCtx, id: i64) -> AppResult<Period> {
    lunch::period(&f.pool, id).await?.filter(|p| p.takes_orders(today())).ok_or(AppError::NotFound)
}

pub async fn order_page(f: FamilyCtx, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    let period = open_period(&f, id).await?;
    order_form(f, period, None, HashMap::new()).await
}

/// The form has one field per child and day, "c{child}_d{day}", holding the
/// chosen item, and "method" for how to pay.
pub async fn place_order(
    f: FamilyCtx,
    UrlPath(id): UrlPath<i64>,
    Form(fields): Form<HashMap<String, String>>,
) -> AppResult<Response> {
    let period = open_period(&f, id).await?;
    let method = fields.get("method").cloned().unwrap_or_default();
    let offline = Offline::load(&f.pool).await?;
    let allowed = match method.as_str() {
        "card" => takes_cards(&f).await?,
        "etransfer" => offline.etransfer(),
        "cash" => offline.cash,
        _ => false,
    };
    let mut choices = Vec::new();
    for (key, value) in &fields {
        let Some((child, day)) = key.strip_prefix('c').and_then(|k| k.split_once("_d")) else { continue };
        let (Ok(child_id), Ok(day_id), Ok(item_id)) = (child.parse(), day.parse(), value.parse()) else { continue };
        choices.push(Choice { child_id, day_id, item_id, quantity: 1 });
    }
    choices.sort_by_key(|c| (c.day_id, c.child_id));
    if !allowed {
        return order_form(f, period, Some("Choose how to pay.".into()), fields).await;
    }
    let order = match lunch::create_order(&f.pool, f.person.id, &period, &method, &choices, today()).await? {
        Ok(order) => order,
        Err(e) => return order_form(f, period, Some(e), fields).await,
    };
    if method == "card" {
        return pay(&f, &order, &period).await;
    }
    Ok(redirect_ok(&format!("/family/orders/{}", order.id), "Order saved. Pay as shown below."))
}

/// The smallest card order through Helcim: PAC Keeper takes no fee there.
const HELCIM_SMALLEST: i64 = 50;

/// Sends the parent to pay a pending card order: to Stripe Checkout, or to
/// the page with Helcim's form.
async fn pay(f: &FamilyCtx, order: &Order, period: &Period) -> AppResult<Response> {
    let back = format!("/family/orders/{}", order.id);
    match cards(&f.hosted, &f.lunch, f.pac.id).await? {
        Some(Cards::Stripe(ps)) => pay_stripe(f, order, period, ps).await,
        Some(Cards::Helcim(_)) => Ok(Redirect::to(&format!("/family/orders/{}/card", order.id)).into_response()),
        None => Ok(redirect_err(&back, "Card payments are not available. Pay another way or ask the PAC.")),
    }
}

async fn pay_stripe(f: &FamilyCtx, order: &Order, period: &Period, ps: PacStripe) -> AppResult<Response> {
    let back = format!("/family/orders/{}", order.id);
    let Some(stripe) = f.lunch.stripe.as_ref() else {
        return Ok(redirect_err(&back, "Card payments are not available. Pay another way or ask the PAC."));
    };
    let (fee, monthly) = if ps.is_express() {
        let monthly = lunch::monthly_fee_due(&f.pool, today()).await?;
        (f.lunch.fees.express_fee(order.total_cents, monthly.then(today)), monthly)
    } else {
        (f.lunch.fees.standard_fee(order.total_cents), false)
    };
    if order.total_cents < 50 || fee >= order.total_cents {
        return Ok(redirect_err(
            &back,
            &format!(
                "Card payments need an order of at least {}. Add lunches or pay another way.",
                Money(f.lunch.fees.smallest_card_amount(monthly.then(today)))
            ),
        ));
    }
    let base = &f.hosted.base_url;
    let children: Vec<String> = {
        let mut names: Vec<String> =
            lunch::order_lines(&f.pool, order.id).await?.into_iter().map(|l| l.child_name).collect();
        names.sort();
        names.dedup();
        names
    };
    let product = format!("Hot lunch: {}", period.name);
    let description = format!("{} order {} for {}", f.pac.name, order.reference(), children.join(", "));
    let request = CheckoutRequest {
        destination: &ps.destination(),
        amount_cents: order.total_cents,
        application_fee_cents: fee,
        product_name: &product,
        description: &description,
        customer_email: &f.person.email,
        success_url: &format!("{base}/family/orders/{}?session_id={{CHECKOUT_SESSION_ID}}", order.id),
        cancel_url: &format!("{base}/family/orders/{}", order.id),
        pac_id: f.pac.id,
        order_id: order.id,
    };
    match stripe.create_checkout(&request).await {
        Ok(session) => {
            lunch::set_checkout(&f.pool, order.id, &session.id, fee, monthly).await?;
            Ok(Redirect::to(&session.url).into_response())
        }
        Err(e) => {
            tracing::error!("checkout for order {} failed: {e:#}", order.id);
            Ok(redirect_err(&back, "Stripe did not open the payment page. Try again in a few minutes."))
        }
    }
}

#[derive(Template)]
#[template(path = "family/order.html")]
struct OrderPage {
    f: FamilyCtx,
    order: Order,
    period: Period,
    lines: Vec<LineView>,
    offline: Offline,
    cards: bool,
    flash: Flash,
}

#[derive(Deserialize)]
pub struct OrderQuery {
    session_id: Option<String>,
    #[serde(flatten)]
    flash: Flash,
}

async fn own_order(f: &FamilyCtx, id: i64) -> AppResult<Order> {
    lunch::order(&f.pool, id).await?.filter(|o| o.person_id == f.person.id).ok_or(AppError::NotFound)
}

#[derive(Template)]
#[template(path = "family/card.html")]
struct CardPage {
    f: FamilyCtx,
    order: Order,
    checkout_token: String,
}

/// The page with Helcim's card form for a pending card order. Each visit
/// opens a new checkout with a new invoice number, after it checks that the
/// last checkout was not paid already.
pub async fn card(f: FamilyCtx, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    let order = own_order(&f, id).await?;
    let back = format!("/family/orders/{id}");
    if order.status != "pending" || order.method != "card" {
        return Ok(Redirect::to(&back).into_response());
    }
    let Some(Cards::Helcim(token)) = cards(&f.hosted, &f.lunch, f.pac.id).await? else {
        return Ok(redirect_err(&back, "Card payments are not available. Pay another way or ask the PAC."));
    };
    if confirm_helcim(&f.hosted, &f.lunch.helcim, &f.pac, &f.pool, &order, &token).await? {
        return Ok(redirect_ok(&back, "This order is paid. Thank you."));
    }
    if order.total_cents < HELCIM_SMALLEST {
        return Ok(redirect_err(
            &back,
            &format!(
                "Card payments need an order of at least {}. Add lunches or pay another way.",
                Money(HELCIM_SMALLEST)
            ),
        ));
    }
    let period = lunch::period(&f.pool, order.period_id).await?.ok_or(AppError::NotFound)?;
    let mut nonce = [0u8; 4];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut nonce);
    let invoice = format!("PK{}-{}-{}", f.pac.id, order.id, hex::encode(nonce));
    let description = format!("{}: hot lunch, {} ({})", f.pac.name, period.name, order.reference());
    let request = lunch::helcim::CheckoutRequest {
        amount_cents: order.total_cents,
        invoice_number: &invoice,
        contact_name: &f.person.name,
        description: &description,
    };
    match f.lunch.helcim.open_checkout(&token, &request).await {
        Ok(checkout_token) => {
            lunch::set_helcim_invoice(&f.pool, order.id, &invoice).await?;
            render(CardPage { f, order, checkout_token })
        }
        Err(e) => {
            tracing::error!("Helcim checkout for order {id} failed: {e:#}");
            Ok(redirect_err(&back, &format!("Helcim did not open the payment form: {e}")))
        }
    }
}

/// The card page posts here when Helcim's form reports a payment. The server
/// asks Helcim itself; the report from the browser only starts the check.
pub async fn card_paid(f: FamilyCtx, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    let order = own_order(&f, id).await?;
    let back = format!("/family/orders/{id}");
    if let Some(Cards::Helcim(token)) = cards(&f.hosted, &f.lunch, f.pac.id).await? {
        confirm_helcim(&f.hosted, &f.lunch.helcim, &f.pac, &f.pool, &order, &token).await?;
    }
    match own_order(&f, id).await?.status.as_str() {
        "paid" => Ok(redirect_ok(&back, "Paid. Thank you.")),
        _ => Ok(redirect_err(
            &back,
            "Helcim has no payment for this order yet. If you paid, the order shows as paid here within a few minutes.",
        )),
    }
}

pub async fn order(f: FamilyCtx, UrlPath(id): UrlPath<i64>, Query(q): Query<OrderQuery>) -> AppResult<Response> {
    let mut order = own_order(&f, id).await?;
    // A Helcim payment that the card page did not report.
    if order.status == "pending"
        && order.helcim_invoice.is_some()
        && let Some(Cards::Helcim(token)) = cards(&f.hosted, &f.lunch, f.pac.id).await?
    {
        match confirm_helcim(&f.hosted, &f.lunch.helcim, &f.pac, &f.pool, &order, &token).await {
            Ok(true) => order = own_order(&f, id).await?,
            Ok(false) => {}
            Err(e) => tracing::warn!("could not check the Helcim payment of order {id}: {e:#}"),
        }
    }
    // Back from Stripe: confirm the payment now, before the webhook arrives.
    if let (Some(session), "pending", Some(stripe)) =
        (q.session_id.as_deref(), order.status.as_str(), f.lunch.stripe.as_ref())
        && order.stripe_session_id.as_deref() == Some(session)
        && let Some(ps) = f.hosted.pac_stripe(f.pac.id).await?
    {
        match stripe.checkout_status(&ps.destination(), session).await {
            Ok(status) => {
                confirm(&f.hosted, &f.pac, &f.pool, &order, &status).await?;
                order = own_order(&f, id).await?;
            }
            Err(e) => tracing::warn!("could not check the payment of order {id}: {e:#}"),
        }
    }
    let period = lunch::period(&f.pool, order.period_id).await?.ok_or(AppError::NotFound)?;
    render(OrderPage {
        lines: lunch::order_lines(&f.pool, id).await?,
        offline: Offline::load(&f.pool).await?,
        cards: takes_cards(&f).await?,
        period,
        order,
        flash: q.flash,
        f,
    })
}

pub async fn pay_again(f: FamilyCtx, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    let order = own_order(&f, id).await?;
    let period = lunch::period(&f.pool, order.period_id).await?.ok_or(AppError::NotFound)?;
    if order.status != "pending" || order.method != "card" {
        return Ok(redirect_err(&format!("/family/orders/{id}"), "This order does not wait for a card payment."));
    }
    pay(&f, &order, &period).await
}

pub async fn cancel(f: FamilyCtx, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    own_order(&f, id).await?;
    if lunch::close_order(&f.pool, id, "cancelled").await? {
        Ok(redirect_ok("/family", "Order cancelled."))
    } else {
        Ok(redirect_err(
            &format!("/family/orders/{id}"),
            "A paid order cannot be cancelled here. Ask the PAC for a refund.",
        ))
    }
}

// ----- Payment confirmation -----

/// Records a Stripe payment for an order, once, and emails the parent. The
/// payment must match the order: same session, amount, and currency.
pub(super) async fn confirm(
    hosted: &Hosted,
    pac: &Pac,
    pool: &SqlitePool,
    order: &Order,
    s: &SessionStatus,
) -> anyhow::Result<bool> {
    if !s.paid
        || order.stripe_session_id.as_deref() != Some(s.id.as_str())
        || s.amount_total != order.total_cents
        || s.currency != "cad"
    {
        return Ok(false);
    }
    if !lunch::mark_paid(pool, order.id, s.payment_intent.as_deref()).await? {
        return Ok(false);
    }
    send_receipt(hosted, pac, pool, order).await?;
    Ok(true)
}

/// Emails the parent that an order is paid, with its lunches.
pub(super) async fn send_receipt(hosted: &Hosted, pac: &Pac, pool: &SqlitePool, order: &Order) -> anyhow::Result<()> {
    if let Some(parent) = hosted.person(order.person_id).await? {
        let lines = lunch::order_lines(pool, order.id).await?;
        let list: Vec<String> =
            lines.iter().map(|l| format!("  {}  {}: {}", l.day_label(), l.child_name, l.item_name)).collect();
        send_mail(
            hosted,
            &parent.email,
            &format!("Hot lunch order {} is paid", order.reference()),
            &format!(
                "Hello {},\n\nThank you. {} received {} for hot lunch order {}:\n\n{}\n\nSee your orders at {}/family\n\nPAC Keeper\n",
                parent.name,
                pac.name,
                order.total(),
                order.reference(),
                list.join("\n"),
                hosted.base_url
            ),
        )
        .await;
    }
    Ok(())
}

/// Asks Helcim about the order's last checkout. An approved purchase of the
/// order's amount in CAD, with the checkout's invoice number, pays the order
/// once, and the parent gets a receipt.
pub(super) async fn confirm_helcim(
    hosted: &Hosted,
    helcim: &Helcim,
    pac: &Pac,
    pool: &SqlitePool,
    order: &Order,
    token: &str,
) -> anyhow::Result<bool> {
    let Some(invoice) = order.helcim_invoice.as_deref() else { return Ok(false) };
    if order.status != "pending" || order.method != "card" {
        return Ok(false);
    }
    let payments = helcim.payments(token, invoice).await?;
    let Some(payment) = payments.iter().find(|p| p.pays(order.total_cents)) else {
        if payments.iter().any(|p| p.approved) {
            tracing::warn!("order {}: Helcim has approved payments that do not match the order", order.id);
        }
        return Ok(false);
    };
    if !lunch::mark_paid_helcim(pool, order.id, &payment.transaction_id).await? {
        return Ok(false);
    }
    send_receipt(hosted, pac, pool, order).await?;
    Ok(true)
}

/// Every 5 minutes, asks Helcim about card orders of the last 3 days that
/// still wait, in case a parent closed the page before the payment was
/// reported.
pub fn spawn_helcim_checks(hosted: Arc<Hosted>, lunch: Arc<Lunch>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5 * 60));
        loop {
            tick.tick().await;
            if let Err(e) = check_helcim_orders(&hosted, &lunch).await {
                tracing::warn!("could not check Helcim payments: {e:#}");
            }
        }
    });
}

pub(super) async fn check_helcim_orders(hosted: &Hosted, lunch: &Lunch) -> anyhow::Result<()> {
    let since = (chrono::Local::now() - chrono::Duration::days(3)).format("%Y-%m-%d %H:%M:%S").to_string();
    for pac in hosted.helcim_pacs().await? {
        let Some(Cards::Helcim(token)) = cards(hosted, lunch, pac.id).await? else { continue };
        let pool = hosted.pac_pool(&pac).await?;
        for order in lunch::waiting_for_helcim(&pool, &since).await? {
            if let Err(e) = confirm_helcim(hosted, &lunch.helcim, &pac, &pool, &order, &token).await {
                tracing::warn!("could not check the Helcim payment of order {} (PAC {}): {e:#}", order.id, pac.id);
            }
        }
    }
    Ok(())
}

/// A payment reported by Stripe's webhook. `account` is the connected
/// account for a PAC's own Stripe; it must be that PAC's account.
pub(super) async fn confirm_from_stripe(
    hosted: &Hosted,
    account: Option<&str>,
    s: &SessionStatus,
) -> anyhow::Result<()> {
    let (Some(pac_id), Some(order_id)) = (s.pac_id, s.order_id) else {
        return Ok(()); // Not a hot lunch payment.
    };
    let Some(pac) = hosted.pac(pac_id).await? else { return Ok(()) };
    if let Some(account) = account {
        let ours = hosted.pac_stripe(pac_id).await?.is_some_and(|ps| ps.account_id == account);
        if !ours {
            tracing::warn!("a Stripe event for PAC {pac_id} came from another account");
            return Ok(());
        }
    }
    let pool = hosted.pac_pool(&pac).await?;
    let Some(order) = lunch::order(&pool, order_id).await? else { return Ok(()) };
    confirm(hosted, &pac, &pool, &order, s).await?;
    Ok(())
}
