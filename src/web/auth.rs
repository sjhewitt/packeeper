//! Sign-in for hosted mode. In local mode (the default) there is no sign-in
//! and everyone can edit.

use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use askama::Template;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::Form;
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use serde::Deserialize;
use sqlx::SqlitePool;

use super::{AppError, AppResult, AppState, CurrentUser, render};
use crate::db::{self, Settings};
use crate::models::User;

const COOKIE: &str = "pac_session";

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let hash: PasswordHash =
        Argon2::default().hash_password(password.as_bytes()).map_err(|e| anyhow::anyhow!("hash password: {e}"))?;
    Ok(hash.to_string())
}

fn verify_password(password: &str, stored: &str) -> bool {
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

/// Puts the current user on every request. When auth is on, sends visitors
/// without a session to the sign-in page and blocks edits by viewers.
pub async fn guard(State(state): State<AppState>, jar: PrivateCookieJar, mut req: Request, next: Next) -> Response {
    if !state.auth {
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
            .fetch_optional(&state.pool)
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

async fn login_view(state: &AppState, error: Option<String>) -> AppResult<Response> {
    let settings = Settings::load(&state.pool).await?;
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&state.pool).await?;
    render(LoginPage {
        pac_name: settings.pac_name,
        error,
        no_users: users == 0,
        demo_login: state.demo.map(|_| (crate::demo::DEMO_USERNAME, crate::demo::DEMO_PASSWORD)),
    })
}

pub async fn login_page(State(state): State<AppState>) -> AppResult<Response> {
    if !state.auth {
        return Ok(Redirect::to("/").into_response());
    }
    login_view(&state, None).await
}

#[derive(Deserialize)]
pub struct LoginForm {
    username: String,
    password: String,
}

pub async fn login(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Form(form): Form<LoginForm>,
) -> AppResult<Response> {
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE username = ?")
        .bind(form.username.trim())
        .fetch_optional(&state.pool)
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
        _ => login_view(&state, Some("The username or password is wrong.".into())).await,
    }
}

fn time_days(days: i64) -> time::Duration {
    time::Duration::days(days)
}

pub async fn logout(jar: PrivateCookieJar) -> impl IntoResponse {
    (jar.remove(Cookie::build(COOKIE).path("/")), Redirect::to("/login"))
}
