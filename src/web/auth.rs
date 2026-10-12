//! Sign-in. A single-PAC server has its own users (with --auth) or no sign-in
//! at all (local mode, where everyone can edit). A hosted server signs people
//! in by email; see `web::hosted`.

use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use askama::Template;
use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::Form;
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use serde::Deserialize;
use sqlx::SqlitePool;

use super::{AppError, AppResult, AppState, Ctx, CurrentUser, render};
use crate::db::{self, Settings};
use crate::models::User;

const COOKIE: &str = "pac_session";

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let hash: PasswordHash =
        Argon2::default().hash_password(password.as_bytes()).map_err(|e| anyhow::anyhow!("hash password: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    match PasswordHash::new(stored) {
        Ok(hash) => Argon2::default().verify_password(password.as_bytes(), &hash).is_ok(),
        Err(_) => false,
    }
}

pub async fn add_user(
    pool: &SqlitePool,
    username: &str,
    display_name: &str,
    password: &str,
    role: &str,
) -> anyhow::Result<()> {
    if !matches!(role, "treasurer" | "viewer") {
        anyhow::bail!("role must be treasurer or viewer");
    }
    let hash = hash_password(password)?;
    sqlx::query(
        "INSERT INTO users (username, display_name, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (username) DO UPDATE SET display_name = excluded.display_name,
           password_hash = excluded.password_hash, role = excluded.role",
    )
    .bind(username)
    .bind(display_name)
    .bind(hash)
    .bind(role)
    .bind(db::now())
    .execute(pool)
    .await?;
    Ok(())
}

/// A form post must come from this site. Browsers send Origin on every POST,
/// so a post from another site cannot act for a signed-in person.
fn same_site_request(req: &Request) -> bool {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok());
    let origin =
        req.headers().get(header::ORIGIN).or_else(|| req.headers().get(header::REFERER)).and_then(|h| h.to_str().ok());
    match (origin, host) {
        (Some(origin), Some(host)) => {
            let rest = origin.split_once("://").map(|(_, r)| r).unwrap_or(origin);
            rest.split('/').next() == Some(host)
        }
        // Not a browser form post (for example, a script on this machine).
        (None, _) => true,
        (Some(_), None) => false,
    }
}

/// Runs before every route. It decides who is signed in and which PAC the
/// request works on, and puts both on the request.
pub async fn guard(State(app): State<AppState>, jar: PrivateCookieJar, mut req: Request, next: Next) -> Response {
    if (app.auth || app.hosted.is_some()) && !same_site_request(&req) {
        return AppError::Forbidden.into_response();
    }
    if let Some(hosted) = app.hosted.clone() {
        return super::hosted::guard(app, hosted, jar, req, next).await;
    }
    let single = app.single.clone().expect("a single-PAC server has its PAC");
    req.extensions_mut().insert(Ctx::new(&app, single.pool.clone(), single.data_dir.clone(), None));
    if !app.auth {
        req.extensions_mut().insert(CurrentUser(None));
        return next.run(req).await;
    }
    let path = req.uri().path();
    if path == "/login" || path.starts_with("/static/") {
        req.extensions_mut().insert(CurrentUser(None));
        return next.run(req).await;
    }
    let user: Option<User> = match jar.get(COOKIE).and_then(|c| c.value().parse::<i64>().ok()) {
        Some(id) => sqlx::query_as("SELECT * FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&single.pool)
            .await
            .unwrap_or(None),
        None => None,
    };
    let Some(user) = user else {
        return Redirect::to("/login").into_response();
    };
    if req.method() != Method::GET && path != "/logout" && !user.can_edit() {
        return AppError::Forbidden.into_response();
    }
    req.extensions_mut().insert(CurrentUser(Some(user)));
    next.run(req).await
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    pac_name: String,
    error: Option<String>,
    no_users: bool,
    /// The public sign-in, shown on a demo server.
    demo_login: Option<(&'static str, &'static str)>,
}

async fn login_view(app: &AppState, error: Option<String>) -> AppResult<Response> {
    let pool = &app.single.as_ref().expect("single-PAC server").pool;
    let settings = Settings::load(pool).await?;
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(pool).await?;
    render(LoginPage {
        pac_name: settings.pac_name,
        error,
        no_users: users == 0,
        demo_login: app.demo.map(|_| (crate::demo::DEMO_USERNAME, crate::demo::DEMO_PASSWORD)),
    })
}

pub async fn login_page(State(app): State<AppState>, jar: PrivateCookieJar) -> AppResult<Response> {
    if app.hosted.is_some() {
        return super::hosted::login_page(&app, jar, None).await;
    }
    if !app.auth {
        return Ok(Redirect::to("/").into_response());
    }
    login_view(&app, None).await
}

#[derive(Deserialize)]
pub struct LoginForm {
    #[serde(alias = "email")]
    username: String,
    password: String,
}

pub async fn login(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    headers: axum::http::HeaderMap,
    Form(form): Form<LoginForm>,
) -> AppResult<Response> {
    if let Some(hosted) = app.hosted.clone() {
        return super::hosted::login(&app, &hosted, jar, &headers, &form.username, &form.password).await;
    }
    let pool = &app.single.as_ref().expect("single-PAC server").pool;
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE username = ?")
        .bind(form.username.trim())
        .fetch_optional(pool)
        .await?;
    match user {
        Some(u) if verify_password(&form.password, &u.password_hash) => {
            let cookie = Cookie::build((COOKIE, u.id.to_string()))
                .path("/")
                .http_only(true)
                .same_site(SameSite::Lax)
                .max_age(time_days(30))
                .build();
            Ok((jar.add(cookie), Redirect::to("/")).into_response())
        }
        _ => login_view(&app, Some("The username or password is wrong.".into())).await,
    }
}

pub fn time_days(days: i64) -> time::Duration {
    time::Duration::days(days)
}

pub async fn logout(jar: PrivateCookieJar) -> impl IntoResponse {
    let jar = jar.remove(Cookie::build(COOKIE).path("/")).remove(Cookie::build(super::hosted::COOKIE).path("/"));
    (jar, Redirect::to("/login"))
}
