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
use crate::lunch::helcim::Helcim;
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
    lunch: Arc<Lunch>,
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

    /// Closes the databases (see `Hosted::close`) and deletes the folder.
    async fn finish(self) {
        self.hosted.close().await;
        let _ = std::fs::remove_dir_all(&self.dir);
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
    site_with(stripe_base, "http://127.0.0.1:9").await
}

async fn site_with(stripe_base: &str, helcim_base: &str) -> Site {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "pac-lunch-web-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mailer = crate::hosted::mail::Mailer::Outbox(dir.join("outbox"));
    let hosted = Arc::new(Hosted::open(&dir, BASE.into(), mailer, 1 << 30, crate::crypt::test_key()).await.unwrap());
    let lunch = Arc::new(Lunch {
        stripe: Some(Stripe::for_test(stripe_base, WEBHOOK_SECRET)),
        fees: FeeConfig::default(),
        helcim: Helcim::for_test(helcim_base),
    });
    let app = router(AppState {
        single: None,
        hosted: Some(hosted.clone()),
        renderer: Arc::new(crate::reports::pdf::Renderer::new()),
        pdf_permits: Arc::new(tokio::sync::Semaphore::new(1)),
        auth: true,
        demo: None,
        loopback_only: false,
        desktop: None,
        lunch: Some(lunch.clone()),
        key: axum_extra::extract::cookie::Key::generate(),
    });
    Site { app, hosted, lunch, dir }
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
    let fee = FeeConfig::default().express_fee(1100, Some(chrono::Local::now().date_naive()));
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
    s.finish().await;
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
        desktop: None,
        lunch: None,
        key: axum_extra::extract::cookie::Key::generate(),
    });
    let t = hosted.ensure_person("tess@example.org", "Tess").await.unwrap();
    hosted.set_password(t.id, "treasurer-password").await.unwrap();
    hosted.mark_verified(t.id).await.unwrap();
    let pac =
        hosted.activate(hosted.request_pac(t.id, "Beta PAC", "Beta School", "", "").await.unwrap()).await.unwrap();
    let s = Site { app, hosted, lunch: s.lunch.clone(), dir: s.dir.clone() };
    let (tess, _) = s.sign_in("tess@example.org", "treasurer-password").await;
    assert_eq!(s.get("/lunch", &tess).await.0, StatusCode::NOT_FOUND);
    let code = s.hosted.family_code(pac.id).await.unwrap();
    assert_eq!(s.send("GET", &format!("/join/{code}"), None, "").await.status(), StatusCode::NOT_FOUND);
    let res = s.send("POST", "/stripe/webhook", None, "{}").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let (_, _, settings) = s.get("/settings", &tess).await;
    assert!(!settings.contains("Hot lunch"));
    s.finish().await;
}

const HELCIM_TOKEN: &str = "helcim-test-token";

/// What the fake Helcim was asked, and the transactions it holds.
#[derive(Default)]
struct FakeHelcim {
    checkouts: Vec<serde_json::Value>,
    transactions: Vec<serde_json::Value>,
    refunds: Vec<serde_json::Value>,
}

type Fake = Arc<Mutex<FakeHelcim>>;

fn helcim_denied(headers: &axum::http::HeaderMap) -> Option<axum::response::Response> {
    use axum::response::IntoResponse;
    let ok = headers.get("api-token").and_then(|v| v.to_str().ok()) == Some(HELCIM_TOKEN);
    (!ok).then(|| {
        (StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({"errors": ["Unauthorized"]}))).into_response()
    })
}

/// A fake Helcim: connection test, HelcimPay.js checkouts, transactions by
/// invoice number, and refunds.
async fn fake_helcim() -> (String, Fake) {
    use axum::extract::{Query, State};
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    let fake: Fake = Arc::default();
    let app = axum::Router::new()
        .route(
            "/v2/connection-test",
            get(|headers: HeaderMap| async move {
                helcim_denied(&headers)
                    .unwrap_or_else(|| axum::Json(serde_json::json!({"message": "Connected Successfully"})).into_response())
            }),
        )
        .route(
            "/v2/helcim-pay/initialize",
            post(|State(f): State<Fake>, headers: HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| async move {
                if let Some(denied) = helcim_denied(&headers) {
                    return denied;
                }
                f.lock().unwrap().checkouts.push(body);
                axum::Json(serde_json::json!({"checkoutToken": "ct_test_1", "secretToken": "st_test_1"})).into_response()
            }),
        )
        .route(
            "/v2/card-transactions",
            get(|State(f): State<Fake>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>| async move {
                if let Some(denied) = helcim_denied(&headers) {
                    return denied;
                }
                let invoice = q.get("invoiceNumber").cloned().unwrap_or_default();
                let list: Vec<serde_json::Value> = f
                    .lock()
                    .unwrap()
                    .transactions
                    .iter()
                    .filter(|t| t["invoiceNumber"].as_str() == Some(invoice.as_str()))
                    .cloned()
                    .collect();
                axum::Json(list).into_response()
            }),
        )
        .route(
            "/v2/payment/refund",
            post(|State(f): State<Fake>, headers: HeaderMap, axum::Json(body): axum::Json<serde_json::Value>| async move {
                if let Some(denied) = helcim_denied(&headers) {
                    return denied;
                }
                assert!(headers.get("idempotency-key").is_some(), "Helcim needs an idempotency key");
                f.lock().unwrap().refunds.push(body);
                axum::Json(serde_json::json!({"transactionId": 999, "status": "APPROVED", "type": "refund"})).into_response()
            }),
        )
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), fake)
}

fn transaction(id: i64, invoice: &str, amount: f64, status: &str) -> serde_json::Value {
    serde_json::json!({
        "transactionId": id, "invoiceNumber": invoice, "amount": amount, "currency": "CAD",
        "status": status, "type": "purchase"
    })
}

#[tokio::test]
async fn parents_pay_by_card_through_the_pacs_own_helcim() {
    let (stripe_base, _) = fake_stripe().await;
    let (helcim_base, fake) = fake_helcim().await;
    let s = site_with(&stripe_base, &helcim_base).await;

    // A treasurer with an active PAC, hot lunch on, and an open period.
    let treasurer = s.hosted.ensure_person("tess@example.org", "Tess").await.unwrap();
    s.hosted.set_password(treasurer.id, "treasurer-password").await.unwrap();
    s.hosted.mark_verified(treasurer.id).await.unwrap();
    let pac_id = s.hosted.request_pac(treasurer.id, "Alpha PAC", "Alpha School", "", "").await.unwrap();
    let pac = s.hosted.activate(pac_id).await.unwrap();
    let (tess, _) = s.sign_in("tess@example.org", "treasurer-password").await;
    s.post("/lunch/enable", &tess, "on=1").await;
    s.post("/lunch/items", &tess, "name=Pizza&description=&price=5.50&active=1").await;
    s.post("/lunch/divisions", &tess, "name=Div+3&sort_order=3").await;
    let (_, to) = s.post("/lunch/periods", &tess, "name=January&order_deadline=2099-01-02").await;
    let period: i64 = to.trim_start_matches("/lunch/periods/").split('?').next().unwrap().parse().unwrap();
    s.post(
        &format!("/lunch/periods/{period}/days"),
        &tess,
        "from=2099-01-05&to=2099-01-11&weekday=1&weekday=3&item_id=1",
    )
    .await;
    s.post(&format!("/lunch/periods/{period}"), &tess, "name=January&order_deadline=2099-01-02&notes=&status=open")
        .await;

    // A wrong token is refused. The right one is kept, sealed.
    let (_, to) = s.post("/lunch/setup/helcim", &tess, "token=wrong").await;
    assert!(to.contains("err=Helcim%20did%20not%20accept"), "{to}");
    let (_, to) = s.post("/lunch/setup/helcim", &tess, &format!("token={HELCIM_TOKEN}")).await;
    assert!(to.contains("msg=Helcim%20is%20connected"), "{to}");
    let sealed = s.hosted.pac_helcim(pac.id).await.unwrap().unwrap();
    assert!(!sealed.contains(HELCIM_TOKEN), "the token is not stored as text");
    // Only one way to take cards at a time.
    let (_, to) = s.post("/lunch/setup/express", &tess, "").await;
    assert!(to.contains("Helcim.%20Stop%20that%20first"), "{to}");
    let (_, _, setup) = s.get("/lunch/setup", &tess).await;
    assert!(setup.contains("The PAC's own Helcim account"));

    // A parent joins and orders two lunches by card.
    let code = s.hosted.family_code(pac.id).await.unwrap();
    s.send("POST", &format!("/join/{code}"), None, "name=Pat&email=pat%40example.org&password=parent-password").await;
    let pat_person = s.hosted.person_by_email("pat@example.org").await.unwrap().unwrap();
    s.hosted.mark_verified(pat_person.id).await.unwrap();
    let (pat, _) = s.sign_in("pat@example.org", "parent-password").await;
    s.post("/family/children", &pat, "name=Ava&division_id=1&notes=&active=1").await;
    s.post("/family/children", &pat, "name=Ben&division_id=1&notes=&active=1").await;
    let pool = s.hosted.pac_pool(&pac).await.unwrap();
    let days = crate::lunch::days(&pool, period).await.unwrap();
    let (_, _, form) = s.get(&format!("/family/periods/{period}"), &pat).await;
    assert!(form.contains("in Helcim&#39;s secure form"), "{form}");
    let (status, location) = s
        .post(
            &format!("/family/periods/{period}"),
            &pat,
            &format!("method=card&c1_d{}=1&c1_d{}=1", days[0].0.id, days[1].0.id),
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let order_id: i64 = location.trim_start_matches("/family/orders/").trim_end_matches("/card").parse().unwrap();

    // The card page opens a checkout for the order's amount, with a new invoice number.
    let (status, _, page) = s.get(&location, &pat).await;
    assert_eq!(status, StatusCode::OK);
    assert!(page.contains(r#"data-token="ct_test_1""#) && page.contains("secure.helcim.app"), "{page}");
    let sent = fake.lock().unwrap().checkouts[0].clone();
    assert_eq!((sent["amount"].as_f64(), sent["currency"].as_str()), (Some(11.0), Some("CAD")));
    let order = crate::lunch::order(&pool, order_id).await.unwrap().unwrap();
    let invoice = order.helcim_invoice.clone().unwrap();
    assert_eq!(sent["invoiceRequest"]["invoiceNumber"].as_str(), Some(invoice.as_str()));

    // Before a payment, and with payments that do not match, the order waits.
    let check = format!("/family/orders/{order_id}/card");
    let (_, to) = s.post(&check, &pat, "").await;
    assert!(to.contains("err=Helcim%20has%20no%20payment"), "{to}");
    fake.lock().unwrap().transactions.extend([
        transaction(501, "PK1-99-other", 11.0, "APPROVED"),
        transaction(502, &invoice, 5.5, "APPROVED"),
        transaction(503, &invoice, 11.0, "DECLINED"),
    ]);
    s.post(&check, &pat, "").await;
    assert_eq!(crate::lunch::order(&pool, order_id).await.unwrap().unwrap().status, "pending");

    // The approved purchase pays the order, once.
    fake.lock().unwrap().transactions.push(transaction(504, &invoice, 11.0, "APPROVED"));
    let (_, to) = s.post(&check, &pat, "").await;
    assert!(to.contains("msg=Paid"), "{to}");
    s.post(&check, &pat, "").await;
    let paid = crate::lunch::order(&pool, order_id).await.unwrap().unwrap();
    assert_eq!((paid.status.as_str(), paid.helcim_transaction.as_deref(), paid.fee_cents), ("paid", Some("504"), 0));
    let receipts = std::fs::read_dir(s.dir.join("outbox"))
        .unwrap()
        .filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok())
        .filter(|m| m.contains("is paid"))
        .count();
    assert_eq!(receipts, 1, "one receipt email");

    // The treasurer refunds it through Helcim.
    let (_, to) = s.post(&format!("/lunch/orders/{order_id}/refund"), &tess, "").await;
    assert!(to.contains("msg=Refunded"), "{to}");
    let refund = fake.lock().unwrap().refunds[0].clone();
    assert_eq!((refund["originalTransactionId"].as_i64(), refund["amount"].as_f64()), (Some(504), Some(11.0)));
    assert_eq!(crate::lunch::order(&pool, order_id).await.unwrap().unwrap().status, "refunded");

    // A parent who closes the page after paying: the regular check finds the payment.
    let (_, location) =
        s.post(&format!("/family/periods/{period}"), &pat, &format!("method=card&c2_d{}=1", days[0].0.id)).await;
    s.get(&location, &pat).await;
    let second: i64 = location.trim_start_matches("/family/orders/").trim_end_matches("/card").parse().unwrap();
    let invoice = crate::lunch::order(&pool, second).await.unwrap().unwrap().helcim_invoice.unwrap();
    fake.lock().unwrap().transactions.push(transaction(505, &invoice, 5.5, "APPROVED"));
    super::family::check_helcim_orders(&s.hosted, &s.lunch).await.unwrap();
    assert_eq!(crate::lunch::order(&pool, second).await.unwrap().unwrap().status, "paid");

    // Stopping Helcim forgets the token.
    s.post("/lunch/setup/helcim/remove", &tess, "").await;
    assert!(s.hosted.pac_helcim(pac.id).await.unwrap().is_none());
    s.finish().await;
}

/// A new Express account is paid out weekly, and every Stripe call names the API version.
#[tokio::test]
async fn express_accounts_get_weekly_payouts() {
    use axum::http::HeaderMap;
    use axum::routing::post;
    type Seen = Arc<Mutex<Vec<(String, String, HashMap<String, String>)>>>;
    let seen: Seen = Arc::default();
    let record = |path: &'static str, reply: serde_json::Value| {
        let seen = seen.clone();
        post(move |headers: HeaderMap, axum::Form(form): axum::Form<Vec<(String, String)>>| {
            let seen = seen.clone();
            let reply = reply.clone();
            async move {
                let version = headers.get("stripe-version").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                seen.lock().unwrap().push((path.to_string(), version, form.into_iter().collect()));
                axum::Json(reply)
            }
        })
    };
    let app = axum::Router::new()
        .route("/v1/accounts", record("accounts", serde_json::json!({"id": "acct_new"})))
        .route("/v1/account_links", record("links", serde_json::json!({"url": "https://connect.stripe.test/setup"})));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let s = site(&format!("http://{addr}")).await;

    let t = s.hosted.ensure_person("tess@example.org", "Tess").await.unwrap();
    s.hosted.set_password(t.id, "treasurer-password").await.unwrap();
    s.hosted.mark_verified(t.id).await.unwrap();
    let pac = s
        .hosted
        .activate(s.hosted.request_pac(t.id, "Alpha PAC", "Alpha School", "", "").await.unwrap())
        .await
        .unwrap();
    let (tess, _) = s.sign_in("tess@example.org", "treasurer-password").await;
    s.post("/lunch/enable", &tess, "on=1").await;
    let (_, to) = s.post("/lunch/setup/express", &tess, "").await;
    assert_eq!(to, "https://connect.stripe.test/setup");
    assert_eq!(s.hosted.pac_stripe(pac.id).await.unwrap().unwrap().account_id, "acct_new");

    {
        let seen = seen.lock().unwrap();
        let (_, version, form) = seen.iter().find(|(path, _, _)| path == "accounts").unwrap();
        assert_eq!(version, "2025-06-30.basil");
        assert_eq!(form["settings[payouts][schedule][interval]"], "weekly");
        assert_eq!(form["settings[payouts][schedule][weekly_payout_days][]"], "monday");
        assert!(seen.iter().all(|(_, version, _)| version == "2025-06-30.basil"));
    }
    s.finish().await;
}
