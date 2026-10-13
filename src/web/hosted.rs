//! The pages of a hosted server that are not about one PAC's books: sign-up,
//! sign-in by email, choosing a PAC, members and invitations, password resets,
//! and the operator's approval page. Also the sign-in guard for hosted servers.

use std::sync::Arc;
use std::time::Duration;

use askama::Template;
use axum::extract::{FromRequestParts, Path as UrlPath, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::Form;
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar, SameSite};
use serde::Deserialize;

use super::auth::time_days;
use super::{AppError, AppResult, AppState, Ctx, CurrentUser, Flash, HostedCtx, Layout, UserExt, render};
use crate::hosted::{Hosted, Membership, Pac, Person, looks_like_email};
use crate::models::User;

pub const COOKIE: &str = "pk_session";
const MIN_PASSWORD: usize = 10;

/// The session cookie: person, chosen PAC (0 for none), and session version.
struct Session {
    person_id: i64,
    pac_id: Option<i64>,
    version: i64,
}

fn read_session(jar: &PrivateCookieJar) -> Option<Session> {
    let value = jar.get(COOKIE)?.value().to_string();
    let mut parts = value.split(':');
    let person_id = parts.next()?.parse().ok()?;
    let pac_id: i64 = parts.next()?.parse().ok()?;
    let version = parts.next()?.parse().ok()?;
    Some(Session { person_id, pac_id: (pac_id > 0).then_some(pac_id), version })
}

fn with_session(jar: PrivateCookieJar, hosted: &Hosted, person: &Person, pac_id: Option<i64>) -> PrivateCookieJar {
    let cookie = Cookie::build((COOKIE, format!("{}:{}:{}", person.id, pac_id.unwrap_or(0), person.session_version)))
        .path("/")
        .http_only(true)
        .secure(hosted.secure_cookies())
        .same_site(SameSite::Lax)
        .max_age(time_days(30))
        .build();
    jar.add(cookie)
}

/// The signed-in person, on pages outside a PAC.
pub struct SignedIn(pub Person);

impl<S: Send + Sync> FromRequestParts<S> for SignedIn {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<SignedIn, Response> {
        match parts.extensions.get::<Person>() {
            Some(p) => Ok(SignedIn(p.clone())),
            None => Err(Redirect::to("/login").into_response()),
        }
    }
}

fn is_public(path: &str) -> bool {
    matches!(path, "/login" | "/signup" | "/forgot" | "/verify/resend")
        || ["/verify/", "/reset/", "/invite/", "/static/"].iter().any(|p| path.starts_with(p))
}

fn is_account_page(path: &str) -> bool {
    matches!(path, "/pacs" | "/account" | "/logout" | "/operator")
        || path.starts_with("/pacs/")
        || path.starts_with("/operator/")
}

/// Signs people in by session cookie, then picks the PAC for PAC pages.
pub async fn guard(
    app: AppState,
    hosted: Arc<Hosted>,
    jar: PrivateCookieJar,
    mut req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let session = read_session(&jar);
    let person = match &session {
        Some(s) => hosted
            .person(s.person_id)
            .await
            .ok()
            .flatten()
            .filter(|p| p.session_version == s.version && p.password_hash.is_some()),
        None => None,
    };
    req.extensions_mut().insert(CurrentUser(None));
    if let Some(p) = &person {
        req.extensions_mut().insert(p.clone());
    }
    if is_public(&path) {
        return next.run(req).await;
    }
    let Some(person) = person else {
        return Redirect::to("/login").into_response();
    };
    if is_account_page(&path) {
        return next.run(req).await;
    }

    // A PAC page: the session must name an active PAC the person belongs to.
    let Some(pac_id) = session.and_then(|s| s.pac_id) else {
        return Redirect::to("/pacs").into_response();
    };
    let membership = match hosted.membership(person.id, pac_id).await {
        Ok(Some(m)) if m.pac.is_active() => m,
        Ok(_) => return Redirect::to("/pacs").into_response(),
        Err(e) => return AppError::from(e).into_response(),
    };
    let pool = match hosted.pac_pool(&membership.pac).await {
        Ok(pool) => pool,
        Err(e) => return AppError::from(e).into_response(),
    };
    let pac_count =
        hosted.memberships(person.id).await.map(|ms| ms.iter().filter(|m| m.pac.is_active()).count()).unwrap_or(1);
    let user = User {
        id: person.id,
        username: person.email.clone(),
        display_name: person.name.clone(),
        password_hash: String::new(),
        role: membership.role.clone(),
        created_at: String::new(),
    };
    if req.method() != Method::GET && !user.can_edit() {
        return AppError::Forbidden.into_response();
    }
    let data_dir = hosted.pac_dir(&membership.pac);
    let ctx = HostedCtx { hosted: hosted.clone(), pac: membership.pac, person, pac_count };
    req.extensions_mut().insert(CurrentUser(Some(user)));
    req.extensions_mut().insert(Ctx::new(&app, pool, data_dir, Some(ctx)));
    next.run(req).await
}

fn hosted_or_404(app: &AppState) -> AppResult<Arc<Hosted>> {
    app.hosted.clone().ok_or(AppError::NotFound)
}

/// The visitor's address, for rate limits. The reverse proxy adds the address
/// it saw to the end of X-Forwarded-For; earlier entries come from the visitor
/// and can be made up.
fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .rfind(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "unknown".into())
}

async fn operator_emails(hosted: &Hosted) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT email FROM people WHERE is_operator = 1").fetch_all(&hosted.control).await?)
}

/// Sends one email. A failure does not undo the action that asked for the
/// email: it goes to the log, and the caller tells the person what happened.
async fn send_mail(hosted: &Hosted, to: &str, subject: &str, body: &str) -> bool {
    match hosted.mailer.send(to, subject, body).await {
        Ok(()) => true,
        Err(e) => {
            tracing::error!("could not send the email \"{subject}\": {e:#}");
            false
        }
    }
}

/// Tells the operators about PACs that wait for approval. The request is on
/// /operator even when the email fails.
async fn notify_operators(hosted: &Hosted, pac: &Pac, requester: &Person) -> anyhow::Result<()> {
    let body = format!(
        "{} <{}> asks to use PAC Keeper for:\n\n  {}\n  {}\n  {}\n\n{}\n\nApprove or reject it here: {}/operator\n",
        requester.name, requester.email, pac.name, pac.school, pac.district, pac.message, hosted.base_url
    );
    for to in operator_emails(hosted).await? {
        send_mail(hosted, &to, &format!("New PAC request: {}", pac.name), &body).await;
    }
    Ok(())
}

// ----- Public page templates -----

#[derive(Template)]
#[template(path = "hosted/notice.html")]
struct Notice {
    title: String,
    message: String,
    link: Option<(String, String)>,
}

fn notice(title: &str, message: &str, link: Option<(&str, &str)>) -> AppResult<Response> {
    render(Notice {
        title: title.into(),
        message: message.into(),
        link: link.map(|(href, label)| (href.to_string(), label.to_string())),
    })
}

#[derive(Template)]
#[template(path = "hosted/login.html")]
struct LoginPage {
    error: Option<String>,
    email: String,
    unverified: bool,
}

pub async fn login_page(_app: &AppState, _jar: PrivateCookieJar, error: Option<String>) -> AppResult<Response> {
    render(LoginPage { error, email: String::new(), unverified: false })
}

pub async fn login(
    _app: &AppState,
    hosted: &Hosted,
    jar: PrivateCookieJar,
    headers: &HeaderMap,
    email: &str,
    password: &str,
) -> AppResult<Response> {
    let email = email.trim().to_lowercase();
    let window = Duration::from_secs(15 * 60);
    if !hosted.allow(&format!("login:{email}"), 10, window).await
        || !hosted.allow(&format!("login-ip:{}", client_ip(headers)), 50, window).await
    {
        return render(LoginPage {
            error: Some("Too many sign-in attempts. Wait 15 minutes, or reset your password.".into()),
            email,
            unverified: false,
        });
    }
    let person = hosted.person_by_email(&email).await?;
    let ok = person
        .as_ref()
        .and_then(|p| p.password_hash.as_deref())
        .is_some_and(|hash| super::verify_password(password, hash));
    let Some(person) = person.filter(|_| ok) else {
        return render(LoginPage { error: Some("The email or password is wrong.".into()), email, unverified: false });
    };
    if !person.is_verified() {
        return render(LoginPage {
            error: Some("Confirm your email address first. We sent you a link.".into()),
            email,
            unverified: true,
        });
    }
    let active: Vec<Membership> =
        hosted.memberships(person.id).await?.into_iter().filter(|m| m.pac.is_active()).collect();
    if active.len() == 1 {
        let jar = with_session(jar, hosted, &person, Some(active[0].pac.id));
        return Ok((jar, Redirect::to("/")).into_response());
    }
    Ok((with_session(jar, hosted, &person, None), Redirect::to("/pacs")).into_response())
}

// ----- Sign-up -----

#[derive(Template)]
#[template(path = "hosted/signup.html")]
struct SignupPage {
    error: Option<String>,
    f: SignupForm,
}

#[derive(Default, Deserialize)]
pub struct SignupForm {
    #[serde(default)]
    pac_name: String,
    #[serde(default)]
    school: String,
    #[serde(default)]
    district: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    message: String,
}

pub async fn signup_page(State(app): State<AppState>) -> AppResult<Response> {
    hosted_or_404(&app)?;
    render(SignupPage { error: None, f: SignupForm::default() })
}

pub async fn signup(
    State(app): State<AppState>,
    headers: HeaderMap,
    Form(mut f): Form<SignupForm>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    f.password.truncate(200);
    let error = |f: SignupForm, e: &str| render(SignupPage { error: Some(e.into()), f });
    if !hosted.allow(&format!("signup:{}", client_ip(&headers)), 5, Duration::from_secs(3600)).await {
        return error(f, "Too many sign-ups from this network. Try again in an hour.");
    }
    if f.pac_name.trim().is_empty() || f.school.trim().is_empty() || f.name.trim().is_empty() {
        return error(f, "Enter the PAC name, the school, and your name.");
    }
    if !looks_like_email(&f.email) {
        return error(f, "Enter a valid email address.");
    }
    if f.password.chars().count() < MIN_PASSWORD {
        return error(f, &format!("Use a password of at least {MIN_PASSWORD} characters."));
    }
    let email = f.email.trim().to_lowercase();
    if let Some(p) = hosted.person_by_email(&email).await?
        && p.password_hash.is_some()
    {
        return error(
            f,
            "There is already an account with this email. Sign in, then request a PAC from the list of your PACs.",
        );
    }
    let person = hosted.ensure_person(&email, &f.name).await?;
    sqlx::query("UPDATE people SET name = ? WHERE id = ?")
        .bind(f.name.trim())
        .bind(person.id)
        .execute(&hosted.control)
        .await?;
    hosted.set_password(person.id, &f.password).await?;
    hosted.request_pac(person.id, &f.pac_name, &f.school, &f.district, &f.message).await?;
    if !send_verification(&hosted, &person.email, &f.name, person.id).await? {
        return notice(
            "We could not send the email",
            &format!(
                "We made your account and saved the request for {}, but the email to confirm {email} could not be sent. \
                 In a few minutes, sign in and choose \"Send the confirmation link again\".",
                f.pac_name.trim()
            ),
            Some(("/login", "Sign in")),
        );
    }
    notice(
        "Check your email",
        &format!(
            "We sent a link to {email}. Follow it to confirm your address. Then we review the request for {} and email you when it is ready.",
            f.pac_name.trim()
        ),
        None,
    )
}

/// Sends a new confirmation link. `false` means the email could not be sent.
async fn send_verification(hosted: &Hosted, email: &str, name: &str, person_id: i64) -> anyhow::Result<bool> {
    let token = hosted.create_token("verify", person_id, None, None, chrono::Duration::hours(48)).await?;
    Ok(send_mail(
        hosted,
        email,
        "Confirm your email for PAC Keeper",
        &format!(
            "Hello {name},\n\nConfirm your email address for PAC Keeper with this link:\n\n{}/verify/{token}\n\n\
             The link works for 48 hours. If you did not sign up, ignore this email.\n\nPAC Keeper\n",
            hosted.base_url
        ),
    )
    .await)
}

pub async fn verify(State(app): State<AppState>, UrlPath(token): UrlPath<String>) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    let Some(t) = hosted.take_token(&token, "verify").await? else {
        return notice(
            "This link does not work",
            "It may have expired or been used already. Sign in to get a new one.",
            Some(("/login", "Sign in")),
        );
    };
    hosted.mark_verified(t.person_id).await?;
    let person = hosted.person(t.person_id).await?.ok_or(AppError::NotFound)?;
    let pending: Vec<Membership> =
        hosted.memberships(person.id).await?.into_iter().filter(|m| m.pac.status == "requested").collect();
    for m in &pending {
        notify_operators(&hosted, &m.pac, &person).await?;
    }
    if pending.is_empty() {
        return notice("Email confirmed", "Your email address is confirmed.", Some(("/login", "Sign in")));
    }
    notice(
        "Email confirmed",
        "Thank you. We review each PAC before it opens, and we will email you when yours is ready.",
        Some(("/login", "Sign in")),
    )
}

#[derive(Deserialize)]
pub struct EmailForm {
    email: String,
}

pub async fn resend_verification(
    State(app): State<AppState>,
    headers: HeaderMap,
    Form(f): Form<EmailForm>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    let email = f.email.trim().to_lowercase();
    if hosted.allow(&format!("resend:{email}"), 3, Duration::from_secs(3600)).await
        && hosted.allow(&format!("resend-ip:{}", client_ip(&headers)), 10, Duration::from_secs(3600)).await
        && let Some(p) = hosted.person_by_email(&email).await?
        && !p.is_verified()
    {
        // A failure is logged. The answer stays the same, so that it does not
        // tell whether the address has an account.
        send_verification(&hosted, &p.email, &p.name, p.id).await?;
    }
    notice(
        "Check your email",
        "If that address needs confirming, we sent a new link to it.",
        Some(("/login", "Sign in")),
    )
}

// ----- Forgotten passwords -----

#[derive(Template)]
#[template(path = "hosted/forgot.html")]
struct ForgotPage {}

pub async fn forgot_page(State(app): State<AppState>) -> AppResult<Response> {
    hosted_or_404(&app)?;
    render(ForgotPage {})
}

pub async fn forgot(State(app): State<AppState>, headers: HeaderMap, Form(f): Form<EmailForm>) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    let email = f.email.trim().to_lowercase();
    if hosted.allow(&format!("forgot:{email}"), 3, Duration::from_secs(3600)).await
        && hosted.allow(&format!("forgot-ip:{}", client_ip(&headers)), 10, Duration::from_secs(3600)).await
        && let Some(p) = hosted.person_by_email(&email).await?
        && p.password_hash.is_some()
    {
        let token = hosted.create_token("reset", p.id, None, None, chrono::Duration::hours(2)).await?;
        send_mail(
            &hosted,
            &p.email,
            "Reset your PAC Keeper password",
            &format!(
                "Hello {},\n\nSet a new password with this link:\n\n{}/reset/{token}\n\n\
                 The link works for 2 hours. If you did not ask for it, ignore this email; your password stays the same.\n\nPAC Keeper\n",
                p.name, hosted.base_url
            ),
        )
        .await;
    }
    // The same answer whether or not the address has an account, and whether
    // or not the email went out (a failure is in the log).
    notice(
        "Check your email",
        "If there is an account for that address, we sent it a link to set a new password.",
        None,
    )
}

#[derive(Template)]
#[template(path = "hosted/reset.html")]
struct ResetPage {
    token: String,
    error: Option<String>,
}

pub async fn reset_page(State(app): State<AppState>, UrlPath(token): UrlPath<String>) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    if hosted.peek_token(&token, "reset").await?.is_none() {
        return notice(
            "This link does not work",
            "It may have expired or been used already.",
            Some(("/forgot", "Ask for a new link")),
        );
    }
    render(ResetPage { token, error: None })
}

#[derive(Deserialize)]
pub struct PasswordForm {
    password: String,
}

pub async fn reset(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    UrlPath(token): UrlPath<String>,
    Form(f): Form<PasswordForm>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    if f.password.chars().count() < MIN_PASSWORD {
        return render(ResetPage {
            token,
            error: Some(format!("Use a password of at least {MIN_PASSWORD} characters.")),
        });
    }
    let Some(t) = hosted.take_token(&token, "reset").await? else {
        return notice(
            "This link does not work",
            "It may have expired or been used already.",
            Some(("/forgot", "Ask for a new link")),
        );
    };
    hosted.set_password(t.person_id, &f.password).await?;
    // Following the emailed link proves the address.
    hosted.mark_verified(t.person_id).await?;
    let person = hosted.person(t.person_id).await?.ok_or(AppError::NotFound)?;
    Ok((with_session(jar, &hosted, &person, None), Redirect::to("/pacs")).into_response())
}

// ----- Choosing a PAC, and asking for another -----

#[derive(Template)]
#[template(path = "hosted/pacs.html")]
struct PacsPage {
    person: Person,
    memberships: Vec<Membership>,
}

pub async fn pacs_page(State(app): State<AppState>, SignedIn(person): SignedIn) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    render(PacsPage { memberships: hosted.memberships(person.id).await?, person })
}

pub async fn open_pac(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    SignedIn(person): SignedIn,
    UrlPath(id): UrlPath<i64>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    match hosted.membership(person.id, id).await? {
        Some(m) if m.pac.is_active() => {
            Ok((with_session(jar, &hosted, &person, Some(id)), Redirect::to("/")).into_response())
        }
        _ => Err(AppError::NotFound),
    }
}

#[derive(Template)]
#[template(path = "hosted/request.html")]
struct RequestPage {
    error: Option<String>,
}

pub async fn request_page(State(app): State<AppState>, SignedIn(_): SignedIn) -> AppResult<Response> {
    hosted_or_404(&app)?;
    render(RequestPage { error: None })
}

#[derive(Deserialize)]
pub struct RequestForm {
    pac_name: String,
    school: String,
    #[serde(default)]
    district: String,
    #[serde(default)]
    message: String,
}

pub async fn request_pac(
    State(app): State<AppState>,
    SignedIn(person): SignedIn,
    Form(f): Form<RequestForm>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    if f.pac_name.trim().is_empty() || f.school.trim().is_empty() {
        return render(RequestPage { error: Some("Enter the PAC name and the school.".into()) });
    }
    if !hosted.allow(&format!("request:{}", person.id), 5, Duration::from_secs(86_400)).await {
        return render(RequestPage { error: Some("You have sent several requests today. Try again tomorrow.".into()) });
    }
    let id = hosted.request_pac(person.id, &f.pac_name, &f.school, &f.district, &f.message).await?;
    if let Some(pac) = hosted.pac(id).await? {
        notify_operators(&hosted, &pac, &person).await?;
    }
    Ok(Redirect::to("/pacs").into_response())
}

// ----- The person's own account -----

#[derive(Template)]
#[template(path = "hosted/account.html")]
struct AccountPage {
    person: Person,
    msg: Option<String>,
    err: Option<String>,
}

pub async fn account_page(
    State(app): State<AppState>,
    SignedIn(person): SignedIn,
    axum_extra::extract::Query(flash): axum_extra::extract::Query<Flash>,
) -> AppResult<Response> {
    hosted_or_404(&app)?;
    render(AccountPage { person, msg: flash.msg, err: flash.err })
}

#[derive(Deserialize)]
pub struct AccountForm {
    name: String,
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_password: String,
}

pub async fn update_account(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    SignedIn(person): SignedIn,
    Form(f): Form<AccountForm>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    if f.name.trim().is_empty() {
        return Ok(super::redirect_err("/account", "Enter your name."));
    }
    sqlx::query("UPDATE people SET name = ? WHERE id = ?")
        .bind(f.name.trim())
        .bind(person.id)
        .execute(&hosted.control)
        .await?;
    if f.new_password.is_empty() {
        return Ok(super::redirect_ok("/account", "Saved."));
    }
    let current_ok = person.password_hash.as_deref().is_some_and(|h| super::verify_password(&f.current_password, h));
    if !current_ok {
        return Ok(super::redirect_err("/account", "Your current password is wrong."));
    }
    if f.new_password.chars().count() < MIN_PASSWORD {
        return Ok(super::redirect_err(
            "/account",
            &format!("Use a new password of at least {MIN_PASSWORD} characters."),
        ));
    }
    hosted.set_password(person.id, &f.new_password).await?;
    // The new password signs out other sessions; keep this one.
    let person = hosted.person(person.id).await?.ok_or(AppError::NotFound)?;
    let pac = read_session(&jar).and_then(|s| s.pac_id);
    let jar = with_session(jar, &hosted, &person, pac);
    Ok((jar, super::redirect_ok("/account", "Password changed. Other devices are signed out.")).into_response())
}

// ----- Members of a PAC -----

pub struct MemberRow {
    pub person_id: i64,
    pub name: String,
    pub email: String,
    pub role: String,
    pub joined: String,
    pub has_password: bool,
}

#[derive(Template)]
#[template(path = "members.html")]
struct MembersPage {
    layout: Layout,
    members: Vec<MemberRow>,
    me: i64,
}

fn hosted_ctx(state: &Ctx) -> AppResult<&HostedCtx> {
    state.hosted.as_ref().ok_or(AppError::NotFound)
}

async fn member_rows(h: &HostedCtx) -> anyhow::Result<Vec<MemberRow>> {
    let rows: Vec<(i64, String, String, String, String, bool)> = sqlx::query_as(
        "SELECT p.id, p.name, p.email, m.role, m.created_at, p.password_hash IS NOT NULL
         FROM memberships m JOIN people p ON p.id = m.person_id
         WHERE m.pac_id = ? ORDER BY m.role DESC, p.name",
    )
    .bind(h.pac.id)
    .fetch_all(&h.hosted.control)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(person_id, name, email, role, joined, has_password)| MemberRow {
            person_id,
            name,
            email,
            role,
            joined: joined.chars().take(10).collect(),
            has_password,
        })
        .collect())
}

pub async fn members_page(
    state: Ctx,
    user: UserExt,
    axum_extra::extract::Query(flash): axum_extra::extract::Query<Flash>,
) -> AppResult<Response> {
    let h = hosted_ctx(&state)?;
    render(MembersPage {
        members: member_rows(h).await?,
        me: h.person.id,
        layout: Layout::new(&state, &user, "settings", "Members", &flash).await?,
    })
}

#[derive(Deserialize)]
pub struct InviteForm {
    email: String,
    role: String,
}

pub async fn invite(state: Ctx, Form(f): Form<InviteForm>) -> AppResult<Response> {
    let h = hosted_ctx(&state)?;
    let email = f.email.trim().to_lowercase();
    if !looks_like_email(&email) {
        return Ok(super::redirect_err("/members", "Enter a valid email address."));
    }
    let role = if f.role == "treasurer" { "treasurer" } else { "viewer" };
    if !h.hosted.allow(&format!("invite:{}", h.pac.id), 30, Duration::from_secs(86_400)).await {
        return Ok(super::redirect_err("/members", "This PAC has sent many invitations today. Try again tomorrow."));
    }
    let person = h.hosted.ensure_person(&email, "").await?;
    if h.hosted.membership(person.id, h.pac.id).await?.is_some() {
        return Ok(super::redirect_err("/members", "That person is already a member."));
    }
    let token =
        h.hosted.create_token("invite", person.id, Some(h.pac.id), Some(role), chrono::Duration::days(7)).await?;
    let role_text = if role == "treasurer" {
        "a treasurer, who can change the records"
    } else {
        "a viewer, who can see the books and reports"
    };
    let sent = send_mail(
        &h.hosted,
        &email,
        &format!("Join {} on PAC Keeper", h.pac.name),
        &format!(
            "Hello,\n\n{} invites you to the books of {} on PAC Keeper, as {role_text}.\n\nAccept with this link:\n\n{}/invite/{token}\n\n\
             The link works for 7 days.\n\nPAC Keeper\n",
            h.person.name, h.pac.name, h.hosted.base_url
        ),
    )
    .await;
    if !sent {
        return Ok(super::redirect_err(
            "/members",
            &format!("The invitation email to {email} could not be sent. Try again in a few minutes."),
        ));
    }
    Ok(super::redirect_ok("/members", &format!("Invitation sent to {email}.")))
}

async fn treasurer_count(h: &HostedCtx) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM memberships WHERE pac_id = ? AND role = 'treasurer'")
        .bind(h.pac.id)
        .fetch_one(&h.hosted.control)
        .await?)
}

#[derive(Deserialize)]
pub struct RoleForm {
    role: String,
}

pub async fn change_role(state: Ctx, UrlPath(person_id): UrlPath<i64>, Form(f): Form<RoleForm>) -> AppResult<Response> {
    let h = hosted_ctx(&state)?;
    let role = if f.role == "treasurer" { "treasurer" } else { "viewer" };
    let current = h.hosted.membership(person_id, h.pac.id).await?.ok_or(AppError::NotFound)?;
    if current.role == "treasurer" && role == "viewer" && treasurer_count(h).await? <= 1 {
        return Ok(super::redirect_err(
            "/members",
            "A PAC needs at least one treasurer. Make someone else a treasurer first.",
        ));
    }
    sqlx::query("UPDATE memberships SET role = ? WHERE pac_id = ? AND person_id = ?")
        .bind(role)
        .bind(h.pac.id)
        .bind(person_id)
        .execute(&h.hosted.control)
        .await?;
    Ok(super::redirect_ok("/members", "Role changed."))
}

pub async fn remove_member(state: Ctx, UrlPath(person_id): UrlPath<i64>) -> AppResult<Response> {
    let h = hosted_ctx(&state)?;
    let current = h.hosted.membership(person_id, h.pac.id).await?.ok_or(AppError::NotFound)?;
    if current.role == "treasurer" && treasurer_count(h).await? <= 1 {
        return Ok(super::redirect_err(
            "/members",
            "A PAC needs at least one treasurer. Make someone else a treasurer first.",
        ));
    }
    sqlx::query("DELETE FROM memberships WHERE pac_id = ? AND person_id = ?")
        .bind(h.pac.id)
        .bind(person_id)
        .execute(&h.hosted.control)
        .await?;
    // Unused invitations for this PAC go too.
    sqlx::query("DELETE FROM tokens WHERE purpose = 'invite' AND pac_id = ? AND person_id = ? AND used_at IS NULL")
        .bind(h.pac.id)
        .bind(person_id)
        .execute(&h.hosted.control)
        .await?;
    if person_id == h.person.id {
        return Ok(Redirect::to("/pacs").into_response());
    }
    Ok(super::redirect_ok("/members", "Member removed."))
}

// ----- Accepting an invitation -----

#[derive(Template)]
#[template(path = "hosted/invite.html")]
struct InvitePage {
    token: String,
    pac_name: String,
    role: String,
    email: String,
    needs_password: bool,
    error: Option<String>,
}

async fn invite_view(hosted: &Hosted, token: String, error: Option<String>) -> AppResult<Response> {
    let Some(t) = hosted.peek_token(&token, "invite").await? else {
        return notice(
            "This invitation does not work",
            "It may have expired or been used already. Ask a treasurer of the PAC to invite you again.",
            None,
        );
    };
    let pac = hosted.pac(t.pac_id.unwrap_or(0)).await?.ok_or(AppError::NotFound)?;
    let person = hosted.person(t.person_id).await?.ok_or(AppError::NotFound)?;
    render(InvitePage {
        token,
        pac_name: pac.name,
        role: t.role.unwrap_or_else(|| "viewer".into()),
        email: person.email,
        needs_password: person.password_hash.is_none(),
        error,
    })
}

pub async fn invite_page(State(app): State<AppState>, UrlPath(token): UrlPath<String>) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    invite_view(&hosted, token, None).await
}

#[derive(Deserialize)]
pub struct AcceptForm {
    #[serde(default)]
    name: String,
    password: String,
}

pub async fn accept_invite(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    UrlPath(token): UrlPath<String>,
    Form(f): Form<AcceptForm>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    let Some(t) = hosted.peek_token(&token, "invite").await? else {
        return invite_view(&hosted, token, None).await;
    };
    let person = hosted.person(t.person_id).await?.ok_or(AppError::NotFound)?;
    match person.password_hash.as_deref() {
        // A new person sets their name and password.
        None => {
            if f.name.trim().is_empty() {
                return invite_view(&hosted, token, Some("Enter your name.".into())).await;
            }
            if f.password.chars().count() < MIN_PASSWORD {
                return invite_view(
                    &hosted,
                    token,
                    Some(format!("Use a password of at least {MIN_PASSWORD} characters.")),
                )
                .await;
            }
            sqlx::query("UPDATE people SET name = ? WHERE id = ?")
                .bind(f.name.trim())
                .bind(person.id)
                .execute(&hosted.control)
                .await?;
            hosted.set_password(person.id, &f.password).await?;
        }
        // Someone with an account proves it with their password.
        Some(hash) => {
            if !hosted.allow(&format!("login:{}", person.email), 10, Duration::from_secs(900)).await
                || !super::verify_password(&f.password, hash)
            {
                return invite_view(&hosted, token, Some("The password is wrong.".into())).await;
            }
        }
    }
    let Some(t) = hosted.take_token(&token, "invite").await? else {
        return invite_view(&hosted, token, None).await;
    };
    // The emailed link proves the address.
    hosted.mark_verified(person.id).await?;
    let pac_id = t.pac_id.ok_or(AppError::NotFound)?;
    sqlx::query(
        "INSERT INTO memberships (pac_id, person_id, role, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (pac_id, person_id) DO UPDATE SET role = excluded.role",
    )
    .bind(pac_id)
    .bind(person.id)
    .bind(t.role.as_deref().unwrap_or("viewer"))
    .bind(crate::db::now())
    .execute(&hosted.control)
    .await?;
    let person = hosted.person(person.id).await?.ok_or(AppError::NotFound)?;
    Ok((with_session(jar, &hosted, &person, Some(pac_id)), Redirect::to("/")).into_response())
}

// ----- The operator -----

pub struct OperatorRow {
    pub pac: Pac,
    pub requester: Option<Person>,
    pub members: i64,
    pub storage: String,
}

#[derive(Template)]
#[template(path = "hosted/operator.html")]
struct OperatorPage {
    requested: Vec<OperatorRow>,
    active: Vec<OperatorRow>,
    other: Vec<OperatorRow>,
    msg: Option<String>,
}

fn folder_size(dir: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    entries
        .flatten()
        .map(|e| if e.path().is_dir() { folder_size(&e.path()) } else { e.metadata().map(|m| m.len()).unwrap_or(0) })
        .sum()
}

fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1u64 << 20) as f64),
        b => format!("{} KB", b.div_ceil(1024)),
    }
}

fn require_operator(person: &Person) -> AppResult<()> {
    if person.is_operator { Ok(()) } else { Err(AppError::Forbidden) }
}

pub async fn operator_page(
    State(app): State<AppState>,
    SignedIn(person): SignedIn,
    axum_extra::extract::Query(flash): axum_extra::extract::Query<Flash>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    require_operator(&person)?;
    let pacs: Vec<Pac> =
        sqlx::query_as("SELECT * FROM pacs ORDER BY created_at DESC").fetch_all(&hosted.control).await?;
    let (mut requested, mut active, mut other) = (Vec::new(), Vec::new(), Vec::new());
    for pac in pacs {
        let requester = match pac.requested_by {
            Some(id) => hosted.person(id).await?,
            None => None,
        };
        let members: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memberships WHERE pac_id = ?")
            .bind(pac.id)
            .fetch_one(&hosted.control)
            .await?;
        let storage = size_label(folder_size(&hosted.pac_dir(&pac)));
        let row = OperatorRow { pac, requester, members, storage };
        match row.pac.status.as_str() {
            // Requests appear once the requester has confirmed their email.
            "requested" if row.requester.as_ref().is_some_and(|p| p.is_verified()) => requested.push(row),
            "requested" => {}
            "active" => active.push(row),
            _ => other.push(row),
        }
    }
    render(OperatorPage { requested, active, other, msg: flash.msg })
}

pub async fn operator_action(
    State(app): State<AppState>,
    SignedIn(person): SignedIn,
    UrlPath((id, action)): UrlPath<(i64, String)>,
) -> AppResult<Response> {
    let hosted = hosted_or_404(&app)?;
    require_operator(&person)?;
    let pac = hosted.pac(id).await?.ok_or(AppError::NotFound)?;
    let requester = match pac.requested_by {
        Some(pid) => hosted.person(pid).await?,
        None => None,
    };
    let msg = match action.as_str() {
        "approve" => {
            hosted.activate(id).await?;
            let sent = match &requester {
                Some(r) => {
                    send_mail(
                        &hosted,
                        &r.email,
                        &format!("{} is ready on PAC Keeper", pac.name),
                        &format!(
                            "Hello {},\n\nThe books for {} are ready. Sign in here:\n\n{}/login\n\n\
                             Start in Settings: add the bank accounts with their opening balances, then import a bank file.\n\nPAC Keeper\n",
                            r.name, pac.name, hosted.base_url
                        ),
                    )
                    .await
                }
                None => true,
            };
            if sent {
                format!("{} approved.", pac.name)
            } else {
                format!("{} approved, but the email to its treasurer could not be sent. Tell them yourself.", pac.name)
            }
        }
        "reject" => {
            hosted.set_status(id, "rejected").await?;
            let sent = match &requester {
                Some(r) => {
                    send_mail(
                        &hosted,
                        &r.email,
                        &format!("Your request for {}", pac.name),
                        &format!(
                            "Hello {},\n\nWe cannot open {} on PAC Keeper at this time. Reply to this email if you have questions.\n\nPAC Keeper\n",
                            r.name, pac.name
                        ),
                    )
                    .await
                }
                None => true,
            };
            if sent {
                format!("{} rejected.", pac.name)
            } else {
                format!("{} rejected, but the email to its treasurer could not be sent.", pac.name)
            }
        }
        "suspend" => {
            hosted.set_status(id, "suspended").await?;
            format!("{} suspended. Its members cannot open it until you reactivate it.", pac.name)
        }
        "reactivate" => {
            hosted.activate(id).await?;
            format!("{} reactivated.", pac.name)
        }
        _ => return Err(AppError::NotFound),
    };
    Ok(super::redirect_ok("/operator", &msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ip_is_the_one_the_proxy_saw() {
        let mut headers = HeaderMap::new();
        assert_eq!(client_ip(&headers), "unknown");
        headers.insert("x-forwarded-for", "203.0.113.9".parse().unwrap());
        assert_eq!(client_ip(&headers), "203.0.113.9");
        // A visitor sends a made-up address; the proxy appends the real one.
        headers.insert("x-forwarded-for", "10.0.0.1, 198.51.100.7".parse().unwrap());
        assert_eq!(client_ip(&headers), "198.51.100.7");
        headers.append("x-forwarded-for", "192.0.2.4".parse().unwrap());
        assert_eq!(client_ip(&headers), "192.0.2.4");
    }

    /// When the mail server fails, the person sees what happened, not an
    /// error page, and the action stays done.
    #[tokio::test]
    async fn a_failed_email_is_a_message_not_an_error() {
        use axum::body::Body;
        use tower::ServiceExt;

        let dir = std::env::temp_dir().join(format!("pac-mailfail-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The outbox is a file, so every send fails.
        let outbox = dir.join("outbox");
        std::fs::write(&outbox, b"").unwrap();
        let hosted = Hosted::open(
            &dir.join("root"),
            "https://app.example.org".into(),
            crate::hosted::mail::Mailer::Outbox(outbox),
            1 << 30,
        )
        .await
        .unwrap();
        let control = hosted.control.clone();
        let app = crate::web::router(AppState {
            single: None,
            hosted: Some(Arc::new(hosted)),
            renderer: Arc::new(crate::reports::pdf::Renderer::new()),
            pdf_permits: Arc::new(tokio::sync::Semaphore::new(1)),
            auth: true,
            demo: None,
            loopback_only: false,
            desktop_secret: None,
            key: axum_extra::extract::cookie::Key::generate(),
        });
        let post = |path: &'static str, form: &'static str| {
            let app = app.clone();
            async move {
                let req = axum::http::Request::post(path)
                    .header(axum::http::header::HOST, "app.example.org")
                    .header(axum::http::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(form))
                    .unwrap();
                let res = app.oneshot(req).await.unwrap();
                let status = res.status();
                let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
                (status, String::from_utf8_lossy(&body).into_owned())
            }
        };

        let (status, body) = post(
            "/signup",
            "pac_name=Alpha+PAC&school=Alpha+School&district=&name=Ann&email=ann%40example.org&password=a-long-password&message=",
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("We could not send the email"), "{body}");
        let requests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pacs WHERE status = 'requested'")
            .fetch_one(&control)
            .await
            .unwrap();
        assert_eq!(requests, 1, "the request stays saved");

        // A reset answers the same as always, so it does not tell whether the address has an account.
        let (status, body) = post("/forgot", "email=ann%40example.org").await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert!(body.contains("If there is an account for that address"), "{body}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
