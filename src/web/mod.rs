//! The web interface: server-rendered pages with htmx for small updates.

mod accounts;
mod auth;
mod budget;
mod dashboard;
mod events;
mod family;
mod fundraisers;
mod hosted;
mod import;
mod lunch;
#[cfg(test)]
mod lunch_tests;
mod reports;
mod settings;
mod transactions;

use std::path::PathBuf;
use std::sync::Arc;

use askama::Template;
use axum::extract::{DefaultBodyLimit, FromRef, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{StatusCode, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use axum_extra::extract::cookie::Key;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::db::Settings;
use crate::ledger::{self, Counts};
use crate::models::User;
use crate::reports::pdf::Renderer;

/// What the whole server shares.
#[derive(Clone)]
pub struct AppState {
    /// The one PAC of a single-PAC server. None on a hosted server.
    pub single: Option<Single>,
    /// The PACs of a hosted server. None on a single-PAC server.
    pub hosted: Option<Arc<crate::hosted::Hosted>>,
    pub renderer: Arc<Renderer>,
    /// PDF rendering is CPU-heavy; this caps how many run at once.
    pub pdf_permits: Arc<tokio::sync::Semaphore>,
    pub auth: bool,
    /// Minutes between resets when this is a public demo.
    pub demo: Option<u32>,
    /// The server listens on this computer only. It then answers only requests
    /// addressed to this computer, so a website cannot reach it by DNS rebinding.
    pub loopback_only: bool,
    /// In the desktop app: the secret that the app window holds.
    pub desktop_secret: Option<String>,
    /// Hot lunch ordering, when the hosted server runs with --hot-lunch.
    pub lunch: Option<Arc<crate::lunch::Lunch>>,
    pub key: Key,
}

#[derive(Clone)]
pub struct Single {
    pub pool: SqlitePool,
    pub data_dir: PathBuf,
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Key {
        state.key.clone()
    }
}

/// What one request works with: the PAC's database and folder, and the shared
/// services. The sign-in guard puts it on each request for a PAC page.
#[derive(Clone)]
pub struct Ctx {
    pub pool: SqlitePool,
    pub data_dir: PathBuf,
    pub renderer: Arc<Renderer>,
    pub pdf_permits: Arc<tokio::sync::Semaphore>,
    /// Sign-in is on (always true on a hosted server).
    pub auth: bool,
    pub demo: Option<u32>,
    /// Inside the desktop app: files open in the computer's own apps.
    pub desktop: bool,
    pub hosted: Option<HostedCtx>,
    /// Hot lunch, on a hosted server with --hot-lunch. Each PAC also turns it on.
    pub lunch: Option<Arc<crate::lunch::Lunch>>,
}

/// On a hosted server: the PAC and the person of this request.
#[derive(Clone)]
pub struct HostedCtx {
    pub hosted: Arc<crate::hosted::Hosted>,
    pub pac: crate::hosted::Pac,
    pub person: crate::hosted::Person,
    pub pac_count: usize,
}

impl Ctx {
    fn new(app: &AppState, pool: SqlitePool, data_dir: PathBuf, hosted: Option<HostedCtx>) -> Ctx {
        Ctx {
            pool,
            data_dir,
            renderer: app.renderer.clone(),
            pdf_permits: app.pdf_permits.clone(),
            auth: app.auth || hosted.is_some(),
            demo: app.demo,
            desktop: app.desktop_secret.is_some(),
            lunch: app.lunch.clone().filter(|_| hosted.is_some()),
            hosted,
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Ctx {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Ctx, AppError> {
        // A route without a PAC context is a guard bug; never fall back to another PAC.
        parts.extensions.get::<Ctx>().cloned().ok_or(AppError::NotFound)
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard::page))
        .route("/transactions", get(transactions::list).post(transactions::create))
        .route("/transactions/new", get(transactions::new_form))
        .route("/transactions/{id}", get(transactions::detail).post(transactions::update))
        .route("/transactions/{id}/assign", post(transactions::assign))
        .route("/transactions/{id}/fundraiser", post(transactions::set_fundraiser))
        .route("/transactions/{id}/allocations", post(transactions::set_allocations))
        .route("/transactions/{id}/attachments", post(transactions::upload))
        .route("/transactions/{id}/rule", post(transactions::make_rule))
        .route("/transactions/{id}/reimburses", post(transactions::set_reimbursements))
        .route("/transactions/{id}/delete", post(transactions::delete))
        .route("/attachments/{id}", get(transactions::attachment))
        .route("/attachments/{id}/delete", post(transactions::delete_attachment))
        .route("/import", get(import::page).post(import::upload))
        .route("/import/sample", post(import::sample))
        .route("/import/{id}", get(import::preview))
        .route("/import/{id}/table", post(import::preview_table))
        .route("/import/{id}/commit", post(import::commit))
        .route("/import/{id}/undo", post(import::undo))
        .route("/budget", get(budget::page))
        .route("/budget/{fy}/amounts", post(budget::set_amounts))
        .route("/budget/{fy}/year", post(budget::update_year))
        .route("/budget/proposal", get(budget::proposal))
        .route("/budget/proposal/fill", post(budget::fill))
        .route("/budget/next-year", post(budget::create_next_year))
        .route("/reports", get(reports::page))
        .route("/reports/monthly.pdf", get(reports::monthly_pdf))
        .route("/reports/annual.pdf", get(reports::annual_pdf))
        .route("/reports/budget.pdf", get(reports::budget_pdf))
        .route("/reports/gaming.pdf", get(reports::gaming_pdf))
        .route("/gaming", get(reports::gaming_page))
        .route("/gaming/events", get(events::list).post(events::create))
        .route("/gaming/events/{id}", get(events::edit).post(events::update))
        .route("/gaming/events/{id}/delete", post(events::delete))
        .route("/fundraisers", get(fundraisers::list).post(fundraisers::create))
        .route("/fundraisers/{id}", get(fundraisers::detail).post(fundraisers::update))
        .route("/fundraisers/{id}/goal", post(fundraisers::set_goal))
        .route("/fundraisers/{id}/delete", post(fundraisers::delete))
        .route("/reports/fundraiser.pdf", get(reports::fundraiser_pdf))
        // Hot lunch (hosted, --hot-lunch): the PAC's pages, then the parents' pages.
        .route("/lunch", get(lunch::home))
        .route("/lunch/enable", post(lunch::enable))
        .route("/lunch/menu", get(lunch::menu))
        .route("/lunch/items", post(lunch::create_item))
        .route("/lunch/items/{id}", post(lunch::update_item))
        .route("/lunch/divisions", post(lunch::create_division))
        .route("/lunch/divisions/{id}", post(lunch::update_division))
        .route("/lunch/periods", post(lunch::create_period))
        .route("/lunch/periods/{id}", get(lunch::period).post(lunch::update_period))
        .route("/lunch/periods/{id}/days", post(lunch::add_days))
        .route("/lunch/days/{id}", post(lunch::set_day))
        .route("/lunch/days/{id}/delete", post(lunch::delete_day))
        .route("/lunch/days/{id}/sheet", get(lunch::sheet))
        .route("/lunch/orders/{id}", get(lunch::order))
        .route("/lunch/orders/{id}/paid", post(lunch::mark_paid))
        .route("/lunch/orders/{id}/cancel", post(lunch::cancel))
        .route("/lunch/orders/{id}/refund", post(lunch::refund))
        .route("/lunch/setup", get(lunch::setup))
        .route("/lunch/setup/offline", post(lunch::save_offline))
        .route("/lunch/setup/express", post(lunch::express))
        .route("/lunch/setup/express/return", get(lunch::express_return))
        .route("/lunch/setup/express/refresh", get(lunch::express_refresh))
        .route("/lunch/setup/standard", post(lunch::standard))
        .route("/lunch/setup/standard/return", get(lunch::standard_return))
        .route("/lunch/setup/stripe/remove", post(lunch::remove_stripe))
        .route("/lunch/setup/code", post(lunch::new_code))
        .route("/lunch/families/{person}/remove", post(lunch::remove_family))
        .route("/stripe/webhook", post(lunch::stripe_webhook))
        .route("/join/{code}", get(family::join_page).post(family::join))
        .route("/pacs/{id}/family", post(family::open))
        .route("/family", get(family::home))
        .route("/family/children", post(family::add_child))
        .route("/family/children/{id}", post(family::update_child))
        .route("/family/periods/{id}", get(family::order_page).post(family::place_order))
        .route("/family/orders/{id}", get(family::order))
        .route("/family/orders/{id}/pay", post(family::pay_again))
        .route("/family/orders/{id}/cancel", post(family::cancel))
        .route("/settings", get(settings::page).post(settings::save))
        .route("/settings/backup.zip", get(settings::backup))
        .route("/accounts", get(accounts::list).post(accounts::create))
        .route("/accounts/{id}", get(accounts::edit).post(accounts::update))
        .route("/line-items", get(accounts::line_items).post(accounts::create_line_item))
        .route("/line-items/{id}", get(accounts::edit_line_item).post(accounts::update_line_item))
        .route("/rules", get(accounts::rules).post(accounts::create_rule))
        .route("/rules/{id}/delete", post(accounts::delete_rule))
        .route("/login", get(auth::login_page).post(auth::login))
        .route("/logout", post(auth::logout))
        // Hosted server only: sign-up, accounts, members, and the operator.
        .route("/signup", get(hosted::signup_page).post(hosted::signup))
        .route("/verify/resend", post(hosted::resend_verification))
        .route("/verify/{token}", get(hosted::verify))
        .route("/forgot", get(hosted::forgot_page).post(hosted::forgot))
        .route("/reset/{token}", get(hosted::reset_page).post(hosted::reset))
        .route("/invite/{token}", get(hosted::invite_page).post(hosted::accept_invite))
        .route("/pacs", get(hosted::pacs_page))
        .route("/pacs/request", get(hosted::request_page).post(hosted::request_pac))
        .route("/pacs/{id}/open", post(hosted::open_pac))
        .route("/account", get(hosted::account_page).post(hosted::update_account))
        .route("/members", get(hosted::members_page).post(hosted::invite))
        .route("/members/{person}/role", post(hosted::change_role))
        .route("/members/{person}/remove", post(hosted::remove_member))
        .route("/operator", get(hosted::operator_page))
        .route("/operator/pacs/{id}/{action}", post(hosted::operator_action))
        .route("/static/app.css", get(|| async { static_file("text/css", include_str!("../../static/app.css")) }))
        .route("/static/app.js", get(|| async { static_file("text/javascript", include_str!("../../static/app.js")) }))
        .route(
            "/static/logo.svg",
            get(|| async { static_file("image/svg+xml", include_str!("../../static/logo.svg")) }),
        )
        .route(
            "/static/htmx.min.js",
            get(|| async { static_file("text/javascript", include_str!("../../static/htmx.min.js")) }),
        )
        .fallback(|| async { AppError::NotFound })
        .layer(middleware::from_fn_with_state(state.clone(), auth::guard))
        .layer(DefaultBodyLimit::max(40 * 1024 * 1024))
        .with_state(state)
}

fn static_file(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "public, max-age=3600")], body).into_response()
}

/// Who is signed in. `None` when auth is off (local mode): everyone can edit.
#[derive(Clone)]
pub struct CurrentUser(pub Option<User>);

impl CurrentUser {
    pub fn can_edit(&self) -> bool {
        self.0.as_ref().is_none_or(|u| u.can_edit())
    }
}

/// Data the page frame needs.
pub struct Layout {
    pub title: String,
    pub nav: &'static str,
    pub pac_name: String,
    pub counts: Counts,
    pub can_edit: bool,
    pub user_name: Option<String>,
    pub flash_ok: Option<String>,
    pub flash_err: Option<String>,
    /// Minutes between resets when this is a public demo.
    pub demo: Option<u32>,
    /// On a hosted server: how many PACs the person belongs to, and whether
    /// they run the server.
    pub hosted: Option<(usize, bool)>,
    /// Inside the desktop app. Links to files then open in the same window,
    /// where the app hands them to the computer's own viewer.
    pub desktop: bool,
    /// Hot lunch is on for this PAC: the nav shows it.
    pub lunch: bool,
}

/// The product name and version, shown in the page footer.
pub const APP_NAME: &str = "PAC Keeper";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_URL: &str = "https://packeeper.ca";

impl Layout {
    pub fn app_name(&self) -> &'static str {
        APP_NAME
    }

    pub fn app_version(&self) -> &'static str {
        APP_VERSION
    }

    pub fn app_url(&self) -> &'static str {
        APP_URL
    }

    /// `target="_blank"` for links to files, except in the desktop app.
    pub fn new_tab(&self) -> &'static str {
        if self.desktop { "" } else { "_blank" }
    }

    pub fn is_hosted(&self) -> bool {
        self.hosted.is_some()
    }

    pub fn many_pacs(&self) -> bool {
        self.hosted.is_some_and(|(n, _)| n > 1)
    }

    pub fn is_operator(&self) -> bool {
        self.hosted.is_some_and(|(_, op)| op)
    }

    pub async fn new(
        state: &Ctx,
        user: &CurrentUser,
        nav: &'static str,
        title: impl Into<String>,
        flash: &Flash,
    ) -> AppResult<Layout> {
        let settings = Settings::load(&state.pool).await?;
        Ok(Layout {
            title: title.into(),
            nav,
            pac_name: settings.pac_name,
            counts: ledger::counts(&state.pool).await?,
            can_edit: user.can_edit(),
            user_name: user
                .0
                .as_ref()
                .map(|u| if u.display_name.is_empty() { u.username.clone() } else { u.display_name.clone() }),
            flash_ok: flash.msg.clone().filter(|s| !s.is_empty()),
            flash_err: flash.err.clone().filter(|s| !s.is_empty()),
            demo: state.demo,
            hosted: state.hosted.as_ref().map(|h| (h.pac_count, h.person.is_operator)),
            desktop: state.desktop,
            lunch: state.lunch.is_some() && crate::lunch::enabled(&state.pool).await?,
        })
    }
}

/// Refuses file uploads on a public demo: anyone could store anything there.
pub fn no_uploads_in_demo(state: &Ctx, back: &str) -> Option<Response> {
    state.demo.map(|_| redirect_err(back, "File uploads are off in the public demo."))
}

/// One-off messages carried in the query string after a redirect.
#[derive(Debug, Default, Deserialize)]
pub struct Flash {
    pub msg: Option<String>,
    pub err: Option<String>,
}

pub fn redirect_ok(to: &str, msg: &str) -> Response {
    Redirect::to(&with_param(to, "msg", msg)).into_response()
}

pub fn redirect_err(to: &str, err: &str) -> Response {
    Redirect::to(&with_param(to, "err", err)).into_response()
}

fn with_param(to: &str, key: &str, value: &str) -> String {
    let sep = if to.contains('?') { '&' } else { '?' };
    format!("{to}{sep}{key}={}", urlencode(value))
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn render(t: impl Template) -> AppResult<Response> {
    Ok(Html(t.render().map_err(anyhow::Error::from)?).into_response())
}

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    NotFound,
    Forbidden,
    BadRequest(String),
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError::Internal(e.into())
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    title: String,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, title, message) = match self {
            AppError::NotFound => {
                (StatusCode::NOT_FOUND, "Not found", "That page or record does not exist.".to_string())
            }
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                "Not allowed",
                "Your account can view, but not change, the records.".to_string(),
            ),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, "Cannot do that", m),
            AppError::Internal(e) => {
                tracing::error!("{e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong", format!("{e:#}"))
            }
        };
        let page = ErrorPage { title: title.to_string(), message };
        let body = page.render().unwrap_or_else(|_| "error".to_string());
        (status, Html(body)).into_response()
    }
}

/// Rejects edits from users who can only view.
pub fn require_edit(user: &CurrentUser) -> AppResult<()> {
    if user.can_edit() { Ok(()) } else { Err(AppError::Forbidden) }
}

/// Parses an optional id from a form or query value; "" is None.
pub fn opt_id(s: &Option<String>) -> Option<i64> {
    s.as_deref().and_then(|v| v.trim().parse().ok())
}

pub type UserExt = Extension<CurrentUser>;

pub use auth::{DESKTOP_COOKIE, add_user, hash_password, verify_password};
