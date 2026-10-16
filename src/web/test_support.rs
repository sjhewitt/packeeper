//! Helpers for the web tests: the example PAC on a local server, and requests to it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use sqlx::SqlitePool;
use tower::ServiceExt;

use super::{AppState, Single, router};

/// The example PAC on a local server with no sign-in.
pub async fn example(name: &str) -> (axum::Router, SqlitePool, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("pac-{name}-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let pool = crate::server::open_single(&dir).await.unwrap();
    crate::demo::load_shifted(&pool, &dir, 0).await.unwrap();
    let app = router(AppState {
        single: Some(Single { pool: pool.clone(), data_dir: dir.clone() }),
        hosted: None,
        renderer: Arc::new(crate::reports::pdf::Renderer::new()),
        pdf_permits: Arc::new(tokio::sync::Semaphore::new(1)),
        auth: false,
        demo: None,
        loopback_only: true,
        desktop: None,
        lunch: None,
        key: axum_extra::extract::cookie::Key::generate(),
    });
    (app, pool, dir)
}

/// Sends a request and returns the status, the redirect address, and the page.
pub async fn send(app: &axum::Router, method: &str, path: &str, form: &str) -> (StatusCode, String, String) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:8080")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let location = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (status, location, String::from_utf8_lossy(&body).into_owned())
}

/// Posts one file in a multipart form and returns the status and the redirect address.
pub async fn send_file(
    app: &axum::Router,
    path: &str,
    field: &str,
    filename: &str,
    bytes: &[u8],
) -> (StatusCode, String) {
    let boundary = "pac-keeper-test-boundary";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: application/zip\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let req = Request::post(path)
        .header(header::HOST, "127.0.0.1:8080")
        .header(header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .body(Body::from(body))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let location = res.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    (res.status(), location)
}
