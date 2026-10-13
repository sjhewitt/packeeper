//! Hot lunch end to end, through the web routes, with a fake Stripe: a parent
//! joins, cannot reach the books, orders, pays by card, and the signed
//! webhook marks the order paid once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use super::{AppState, router};
use crate::hosted::Hosted;
use crate::lunch::stripe::Stripe;
use crate::lunch::{Lunch, fees::FeeConfig};

const BASE: &str = "https://app.example.org";
const WEBHOOK_SECRET: &str = "whsec_test";

/// A fake Stripe that records each Checkout session it is asked for.
async fn fake_stripe() -> (String, Arc<Mutex<Vec<HashMap<String, String>>>>) {
    use axum::routing::{get, post};
    let seen: Arc<Mutex<Vec<HashMap<String, String>>>> = Arc::default();
    let record = seen.clone();
    let app = axum::Router::new()
        .route(
            "/v1/checkout/sessions",
            post(move |axum::Form(form): axum::Form<HashMap<String, String>>| {
                let record = record.clone();
                async move {
                    record.lock().unwrap().push(form);
                    axum::Json(serde_json::json!({"id": "cs_test_1", "url": "https://checkout.stripe.test/cs_test_1"}))
                }
            }),
        )
        .route(
            "/v1/checkout/sessions/{id}",
            get(|| async { axum::Json(serde_json::json!({"id": "cs_test_1", "payment_status": "unpaid"})) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

struct Site {
    app: axum::Router,
    hosted: Arc<Hosted>,
    dir: std::path::PathBuf,
}

impl Site {
    async fn send(&self, method: &str, path: &str, cookie: Option<&str>, form: &str) -> axum::response::Response {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "app.example.org")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        self.app.clone().oneshot(req.body(Body::from(form.to_string())).unwrap()).await.unwrap()
    }

    async fn get(&self, path: &str, cookie: &str) -> (StatusCode, String, String) {
        let res = self.send("GET", path, Some(cookie), "").await;
        let status = res.status();
        let location = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        (status, location, String::from_utf8_lossy(&body).into_owned())
    }

    async fn post(&self, path: &str, cookie: &str, form: &str) -> (StatusCode, String) {
        let res = self.send("POST", path, Some(cookie), form).await;
        let location = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        (res.status(), location)
    }

    /// Signs in and returns the session cookie.
    async fn sign_in(&self, email: &str, password: &str) -> (String, String) {
        let res = self.send("POST", "/login", None, &format!("username={email}&password={password}")).await;
        let location = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let cookie = res
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with("pk_session="))
            .map(|v| v.split(';').next().unwrap().to_string())
            .expect("a session cookie");
        (cookie, location)
    }
}

async fn site(stripe_base: &str) -> Site {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "pac-lunch-web-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mailer = crate::hosted::mail::Mailer::Outbox(dir.join("outbox"));
    let hosted = Arc::new(Hosted::open(&dir, BASE.into(), mailer, 1 << 30).await.unwrap());
    let lunch = Lunch { stripe: Some(Stripe::for_test(stripe_base, WEBHOOK_SECRET)), fees: FeeConfig::default() };
    let app = router(AppState {
        single: None,
        hosted: Some(hosted.clone()),
        renderer: Arc::new(crate::reports::pdf::Renderer::new()),
        pdf_permits: Arc::new(tokio::sync::Semaphore::new(1)),
        auth: true,
        demo: None,
        loopback_only: false,
        desktop_secret: None,
        lunch: Some(Arc::new(lunch)),
        key: axum_extra::extract::cookie::Key::generate(),
    });
    Site { app, hosted, dir }
}

#[tokio::test]
async fn parents_order_and_pay_without_seeing_the_books() {
    let (stripe_base, checkouts) = fake_stripe().await;
    let s = site(&stripe_base).await;

    // A treasurer with an active PAC, on Stripe Express through the platform.
    let treasurer = s.hosted.ensure_person("tess@example.org", "Tess").await.unwrap();
    s.hosted.set_password(treasurer.id, "treasurer-password").await.unwrap();
    s.hosted.mark_verified(treasurer.id).await.unwrap();
    let pac_id = s.hosted.request_pac(treasurer.id, "Alpha PAC", "Alpha School", "", "").await.unwrap();
    let pac = s.hosted.activate(pac_id).await.unwrap();
    s.hosted.set_pac_stripe(pac.id, "acct_alpha", "express", true).await.unwrap();
    let (tess, _) = s.sign_in("tess@example.org", "treasurer-password").await;

    // Before the PAC turns hot lunch on, the menu page sends the treasurer back.
    assert_eq!(s.get("/lunch/menu", &tess).await.1, "/lunch?err=Turn%20hot%20lunch%20on%20first.");
    assert_eq!(s.post("/lunch/enable", &tess, "on=1").await.0, StatusCode::SEE_OTHER);
    s.post("/lunch/items", &tess, "name=Pizza&description=&price=5.50&active=1").await;
    s.post("/lunch/divisions", &tess, "name=Div+3&sort_order=3").await;
    let (_, to) = s.post("/lunch/periods", &tess, "name=January&order_deadline=2099-01-02").await;
    let period: i64 = to.trim_start_matches("/lunch/periods/").split('?').next().unwrap().parse().unwrap();
    let item: i64 = 1;
    s.post(
        &format!("/lunch/periods/{period}/days"),
        &tess,
        &format!("from=2099-01-05&to=2099-01-11&weekday=1&weekday=3&item_id={item}"),
    )
    .await;
    let (_, to) = s
        .post(&format!("/lunch/periods/{period}"), &tess, "name=January&order_deadline=2099-01-02&notes=&status=open")
        .await;
    assert!(to.contains("msg="), "opened: {to}");

    // A parent signs up with the PAC's link, confirms the address, and signs in.
    let code = s.hosted.family_code(pac.id).await.unwrap();
    let res = s
        .send("POST", &format!("/join/{code}"), None, "name=Pat&email=pat%40example.org&password=parent-password")
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let pat_person = s.hosted.person_by_email("pat@example.org").await.unwrap().unwrap();
    s.hosted.mark_verified(pat_person.id).await.unwrap();
    let (pat, landing) = s.sign_in("pat@example.org", "parent-password").await;
    assert_eq!(landing, "/family", "a parent lands on hot lunch");

    // The parent never reaches the books or the PAC's hot lunch pages.
    for path in ["/", "/transactions", "/settings", "/settings/backup.zip", "/lunch", "/lunch/setup", "/members"] {
        let (status, location, _) = s.get(path, &pat).await;
        assert!(status.is_redirection() && location == "/family", "{path}: {status} {location}");
    }
    let (status, location) = s.post("/lunch/enable", &pat, "on=0").await;
    assert!(status.is_redirection() && location == "/family");
    assert!(crate::lunch::enabled(&s.hosted.pac_pool(&pac).await.unwrap()).await.unwrap(), "still on");
    // And the treasurer, who is not a parent there, gets no /family pages.
    let (status, location, _) = s.get("/family", &tess).await;
    assert!(status.is_redirection() && location == "/pacs", "{status} {location}");

    // The parent adds a child and orders lunch on both days, by card.
    s.post("/family/children", &pat, "name=Ava&division_id=1&notes=no+nuts&active=1").await;
    let pool = s.hosted.pac_pool(&pac).await.unwrap();
    let days = crate::lunch::days(&pool, period).await.unwrap();
    assert_eq!(days.len(), 2);
    let form = format!("method=card&c1_d{}={item}&c1_d{}={item}", days[0].0.id, days[1].0.id);
    let (status, location) = s.post(&format!("/family/periods/{period}"), &pat, &form).await;
    assert_eq!((status, location.as_str()), (StatusCode::SEE_OTHER, "https://checkout.stripe.test/cs_test_1"));

    // Stripe got the right amount, fee, and destination.
    let sent = checkouts.lock().unwrap()[0].clone();
    assert_eq!(sent["line_items[0][price_data][unit_amount]"], "1100");
    let fee = FeeConfig::default().express_fee(1100, true);
    assert_eq!(sent["payment_intent_data[application_fee_amount]"], fee.to_string());
    assert_eq!(sent["payment_intent_data[transfer_data][destination]"], "acct_alpha");
    assert_eq!(sent["payment_intent_data[on_behalf_of]"], "acct_alpha");
    assert_eq!(sent["customer_email"], "pat@example.org");
    let order = crate::lunch::order_by_session(&pool, "cs_test_1").await.unwrap().unwrap();
    assert_eq!((order.status.as_str(), order.fee_cents), ("pending", fee));

    // A forged webhook is refused; a signed one marks the order paid, once.
    let event = serde_json::json!({
        "type": "checkout.session.completed",
        "data": {"object": {
            "id": "cs_test_1", "payment_status": "paid", "amount_total": 1100, "currency": "cad",
            "payment_intent": "pi_test_1",
            "metadata": {"pac_id": pac.id.to_string(), "order_id": order.id.to_string()}
        }}
    })
    .to_string();
    let hook = |signature: String| {
        let req = Request::post("/stripe/webhook")
            .header(header::HOST, "app.example.org")
            .header("stripe-signature", signature)
            .body(Body::from(event.clone()))
            .unwrap();
        s.app.clone().oneshot(req)
    };
    let now = chrono::Utc::now().timestamp();
    assert_eq!(
        hook(Stripe::sign_for_test("whsec_wrong", event.as_bytes(), now)).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(crate::lunch::order(&pool, order.id).await.unwrap().unwrap().status, "pending");
    for _ in 0..2 {
        assert_eq!(
            hook(Stripe::sign_for_test(WEBHOOK_SECRET, event.as_bytes(), now)).await.unwrap().status(),
            StatusCode::OK
        );
    }
    let paid = crate::lunch::order(&pool, order.id).await.unwrap().unwrap();
    assert_eq!((paid.status.as_str(), paid.stripe_payment_intent.as_deref()), ("paid", Some("pi_test_1")));
    let receipts = std::fs::read_dir(s.dir.join("outbox"))
        .unwrap()
        .filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok())
        .filter(|m| m.contains("is paid"))
        .count();
    assert_eq!(receipts, 1, "one receipt email");

    // The treasurer sees two pizzas to buy, and the parent sees the order paid.
    let (_, _, page) = s.get(&format!("/lunch/periods/{period}"), &tess).await;
    assert!(
        page.contains("Pizza × 1") && page.contains(&format!("HL-{}", order.id)),
        "the period page lists the order"
    );
    let (status, _, page) = s.get(&format!("/family/orders/{}", order.id), &pat).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains("Thank you"));

    // Another parent cannot open Pat's order.
    let other = s.hosted.ensure_person("olga@example.org", "Olga").await.unwrap();
    s.hosted.set_password(other.id, "another-password").await.unwrap();
    s.hosted.mark_verified(other.id).await.unwrap();
    s.hosted.add_family(other.id, pac.id).await.unwrap();
    let (olga, _) = s.sign_in("olga@example.org", "another-password").await;
    assert_eq!(s.get(&format!("/family/orders/{}", order.id), &olga).await.0, StatusCode::NOT_FOUND);
    let _ = std::fs::remove_dir_all(&s.dir);
}

#[tokio::test]
async fn without_the_flag_there_is_no_hot_lunch() {
    let s = site("http://127.0.0.1:9").await;
    // The same server without hot lunch.
    let hosted = s.hosted.clone();
    let app = router(AppState {
        single: None,
        hosted: Some(hosted.clone()),
        renderer: Arc::new(crate::reports::pdf::Renderer::new()),
        pdf_permits: Arc::new(tokio::sync::Semaphore::new(1)),
        auth: true,
        demo: None,
        loopback_only: false,
        desktop_secret: None,
        lunch: None,
        key: axum_extra::extract::cookie::Key::generate(),
    });
    let t = hosted.ensure_person("tess@example.org", "Tess").await.unwrap();
    hosted.set_password(t.id, "treasurer-password").await.unwrap();
    hosted.mark_verified(t.id).await.unwrap();
    let pac =
        hosted.activate(hosted.request_pac(t.id, "Beta PAC", "Beta School", "", "").await.unwrap()).await.unwrap();
    let s = Site { app, hosted, dir: s.dir.clone() };
    let (tess, _) = s.sign_in("tess@example.org", "treasurer-password").await;
    assert_eq!(s.get("/lunch", &tess).await.0, StatusCode::NOT_FOUND);
    let code = s.hosted.family_code(pac.id).await.unwrap();
    assert_eq!(s.send("GET", &format!("/join/{code}"), None, "").await.status(), StatusCode::NOT_FOUND);
    let res = s.send("POST", "/stripe/webhook", None, "{}").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let (_, _, settings) = s.get("/settings", &tess).await;
    assert!(!settings.contains("Hot lunch"));
    let _ = std::fs::remove_dir_all(&s.dir);
}
