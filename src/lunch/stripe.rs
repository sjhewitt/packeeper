//! The few Stripe calls that hot lunch needs, over Stripe's REST API.
//!
//! Card details never reach PAC Keeper: parents pay on Stripe Checkout, and
//! Stripe reports the payment back by webhook and on the return page.
//!
//! - A PAC with its own Stripe account connects it by OAuth (Connect
//!   Standard). Its payments are direct charges on that account.
//! - A PAC without one gets an Express account under the platform. Its
//!   payments are destination charges: the PAC is the merchant
//!   (`on_behalf_of`), the money goes to its account, and the platform keeps
//!   the application fee. Stripe pays the PAC once a week, on the payout
//!   day (`FeeConfig::payout_day`).
//!
//! Every request names the API version (`API_VERSION`), so the calls work the
//! same whatever version the platform account defaults to.

use std::time::Duration;

use anyhow::Context;

/// The Stripe API version of every request. Weekly payouts on a named day
/// need 2025-06-30 or later.
const API_VERSION: &str = "2025-06-30.basil";
use hmac::{KeyInit, Mac};
use serde_json::Value;

/// The platform's Stripe settings, from the environment.
#[derive(Clone)]
pub struct Stripe {
    secret_key: String,
    /// For Connect Standard OAuth ("ca_..."). Without it, PACs cannot connect
    /// their own Stripe accounts.
    pub client_id: Option<String>,
    /// Signing secrets of the webhook endpoints ("whsec_..."): one for the
    /// platform's events and one for connected accounts' events.
    webhook_secrets: Vec<String>,
    api_base: String,
    connect_base: String,
}

/// Where a payment goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A direct charge on the PAC's own Stripe account.
    Standard { account: String },
    /// A destination charge to the PAC's Express account.
    Express { account: String },
}

/// What a Checkout session needs.
pub struct CheckoutRequest<'a> {
    pub destination: &'a Destination,
    pub amount_cents: i64,
    pub application_fee_cents: i64,
    pub product_name: &'a str,
    pub description: &'a str,
    pub customer_email: &'a str,
    pub success_url: &'a str,
    pub cancel_url: &'a str,
    pub pac_id: i64,
    pub order_id: i64,
}

pub struct CheckoutSession {
    pub id: String,
    pub url: String,
}

/// A finished Checkout session, as Stripe reports it.
#[derive(Debug, Clone)]
pub struct SessionStatus {
    pub id: String,
    pub paid: bool,
    pub amount_total: i64,
    pub currency: String,
    pub payment_intent: Option<String>,
    pub pac_id: Option<i64>,
    pub order_id: Option<i64>,
}

impl SessionStatus {
    pub fn from_json(v: &Value) -> SessionStatus {
        let meta = |key: &str| v["metadata"][key].as_str().and_then(|s| s.parse().ok());
        SessionStatus {
            id: v["id"].as_str().unwrap_or_default().to_string(),
            paid: v["payment_status"].as_str() == Some("paid"),
            amount_total: v["amount_total"].as_i64().unwrap_or(-1),
            currency: v["currency"].as_str().unwrap_or_default().to_string(),
            payment_intent: v["payment_intent"].as_str().map(str::to_string),
            pac_id: meta("pac_id"),
            order_id: meta("order_id"),
        }
    }
}

/// An account's state after onboarding.
#[derive(Debug, Clone)]
pub struct AccountStatus {
    pub charges_enabled: bool,
    pub details_submitted: bool,
}

impl Stripe {
    /// PAC_STRIPE_SECRET_KEY turns card payments on. PAC_STRIPE_CLIENT_ID lets
    /// PACs connect their own Stripe. PAC_STRIPE_WEBHOOK_SECRETS holds the
    /// webhook signing secrets, separated by commas. PAC_STRIPE_API and
    /// PAC_STRIPE_CONNECT_API change the addresses, for tests.
    pub fn from_env() -> anyhow::Result<Option<Stripe>> {
        let Ok(secret_key) = std::env::var("PAC_STRIPE_SECRET_KEY") else {
            return Ok(None);
        };
        let webhook_secrets: Vec<String> = std::env::var("PAC_STRIPE_WEBHOOK_SECRETS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        anyhow::ensure!(
            !webhook_secrets.is_empty(),
            "PAC_STRIPE_WEBHOOK_SECRETS is needed with PAC_STRIPE_SECRET_KEY, so that payments are confirmed"
        );
        Ok(Some(Stripe {
            secret_key,
            client_id: std::env::var("PAC_STRIPE_CLIENT_ID").ok().filter(|s| !s.trim().is_empty()),
            webhook_secrets,
            api_base: std::env::var("PAC_STRIPE_API").unwrap_or_else(|_| "https://api.stripe.com".into()),
            connect_base: std::env::var("PAC_STRIPE_CONNECT_API")
                .unwrap_or_else(|_| "https://connect.stripe.com".into()),
        }))
    }

    /// For tests: a client for a fake Stripe at `base`.
    pub fn for_test(base: &str, webhook_secret: &str) -> Stripe {
        Stripe {
            secret_key: "sk_test_fake".into(),
            client_id: Some("ca_test".into()),
            webhook_secrets: vec![webhook_secret.into()],
            api_base: base.into(),
            connect_base: base.into(),
        }
    }

    // ----- Payments -----

    pub async fn create_checkout(&self, r: &CheckoutRequest<'_>) -> anyhow::Result<CheckoutSession> {
        let mut form: Vec<(String, String)> = vec![
            ("mode".into(), "payment".into()),
            ("line_items[0][quantity]".into(), "1".into()),
            ("line_items[0][price_data][currency]".into(), "cad".into()),
            ("line_items[0][price_data][unit_amount]".into(), r.amount_cents.to_string()),
            ("line_items[0][price_data][product_data][name]".into(), r.product_name.into()),
            ("line_items[0][price_data][product_data][description]".into(), r.description.into()),
            ("customer_email".into(), r.customer_email.into()),
            ("success_url".into(), r.success_url.into()),
            ("cancel_url".into(), r.cancel_url.into()),
            ("client_reference_id".into(), format!("{}-{}", r.pac_id, r.order_id)),
            ("metadata[pac_id]".into(), r.pac_id.to_string()),
            ("metadata[order_id]".into(), r.order_id.to_string()),
            ("payment_intent_data[metadata][pac_id]".into(), r.pac_id.to_string()),
            ("payment_intent_data[metadata][order_id]".into(), r.order_id.to_string()),
        ];
        if r.application_fee_cents > 0 {
            form.push(("payment_intent_data[application_fee_amount]".into(), r.application_fee_cents.to_string()));
        }
        let on_account = match r.destination {
            Destination::Standard { account } => Some(account.as_str()),
            Destination::Express { account } => {
                form.push(("payment_intent_data[transfer_data][destination]".into(), account.clone()));
                form.push(("payment_intent_data[on_behalf_of]".into(), account.clone()));
                None
            }
        };
        let v = self.post("/v1/checkout/sessions", form, on_account).await?;
        Ok(CheckoutSession {
            id: v["id"].as_str().context("Stripe gave no session id")?.to_string(),
            url: v["url"].as_str().context("Stripe gave no checkout address")?.to_string(),
        })
    }

    pub async fn checkout_status(&self, destination: &Destination, session_id: &str) -> anyhow::Result<SessionStatus> {
        let on_account = match destination {
            Destination::Standard { account } => Some(account.as_str()),
            Destination::Express { .. } => None,
        };
        let v = self.get(&format!("/v1/checkout/sessions/{session_id}"), on_account).await?;
        Ok(SessionStatus::from_json(&v))
    }

    /// Refunds a whole payment. The application fee stays with the platform:
    /// Stripe does not return its own fees on a refund either.
    pub async fn refund(&self, destination: &Destination, payment_intent: &str) -> anyhow::Result<()> {
        let mut form = vec![("payment_intent".to_string(), payment_intent.to_string())];
        let on_account = match destination {
            Destination::Standard { account } => Some(account.as_str()),
            Destination::Express { .. } => {
                // Take the money back from the PAC's account, not the platform's.
                form.push(("reverse_transfer".into(), "true".into()));
                None
            }
        };
        self.post("/v1/refunds", form, on_account).await?;
        Ok(())
    }

    // ----- Accounts -----

    /// A new Express account for a PAC, in Canada.
    /// Makes an Express account for a PAC, paid out weekly on `payout_day`
    /// (such as "monday").
    pub async fn create_express_account(
        &self,
        email: &str,
        pac_name: &str,
        payout_day: &str,
    ) -> anyhow::Result<String> {
        let mut form = vec![
            ("type".to_string(), "express".to_string()),
            ("country".into(), "CA".into()),
            ("email".into(), email.into()),
            ("business_profile[name]".into(), pac_name.into()),
            ("capabilities[card_payments][requested]".into(), "true".into()),
            ("capabilities[transfers][requested]".into(), "true".into()),
        ];
        form.extend(weekly_payouts(payout_day));
        let v = self.post("/v1/accounts", form, None).await?;
        Ok(v["id"].as_str().context("Stripe gave no account id")?.to_string())
    }

    /// The Stripe page where the PAC finishes setting up its Express account.
    pub async fn onboarding_link(&self, account: &str, return_url: &str, refresh_url: &str) -> anyhow::Result<String> {
        let form = vec![
            ("account".to_string(), account.to_string()),
            ("type".into(), "account_onboarding".into()),
            ("return_url".into(), return_url.into()),
            ("refresh_url".into(), refresh_url.into()),
        ];
        let v = self.post("/v1/account_links", form, None).await?;
        Ok(v["url"].as_str().context("Stripe gave no onboarding address")?.to_string())
    }

    /// Pays an Express account out once a week, on `payout_day`.
    pub async fn set_weekly_payouts(&self, account: &str, payout_day: &str) -> anyhow::Result<()> {
        self.post(&format!("/v1/accounts/{account}"), weekly_payouts(payout_day), None).await.map(|_| ())
    }

    pub async fn account_status(&self, account: &str) -> anyhow::Result<AccountStatus> {
        let v = self.get(&format!("/v1/accounts/{account}"), None).await?;
        Ok(AccountStatus {
            charges_enabled: v["charges_enabled"].as_bool().unwrap_or(false),
            details_submitted: v["details_submitted"].as_bool().unwrap_or(false),
        })
    }

    /// The Stripe page where a PAC signs in to connect its own account.
    /// `redirect_uri` must be one of the platform's redirect addresses in
    /// Stripe's Connect settings.
    pub fn oauth_url(&self, state: &str, redirect_uri: &str) -> Option<String> {
        let client_id = self.client_id.as_ref()?;
        Some(format!(
            "{}/oauth/authorize?response_type=code&scope=read_write&client_id={}&state={}&redirect_uri={}",
            self.connect_base,
            crate::web::urlencode(client_id),
            crate::web::urlencode(state),
            crate::web::urlencode(redirect_uri)
        ))
    }

    /// Turns the code from the OAuth return into the PAC's account id.
    pub async fn oauth_account(&self, code: &str) -> anyhow::Result<String> {
        let form = vec![
            ("grant_type".to_string(), "authorization_code".to_string()),
            ("code".into(), code.into()),
            ("client_secret".into(), self.secret_key.clone()),
        ];
        let url = format!("{}/oauth/token", self.connect_base);
        let v = self.send(url, Some(form), None).await?;
        Ok(v["stripe_user_id"].as_str().context("Stripe gave no account id")?.to_string())
    }

    // ----- Webhooks -----

    /// Checks the Stripe-Signature header against every webhook secret, and
    /// refuses an event older than five minutes. Returns the parsed event.
    pub fn verify_webhook(&self, payload: &[u8], header: &str, now: i64) -> anyhow::Result<Value> {
        let mut timestamp = None;
        let mut signatures = Vec::new();
        for part in header.split(',') {
            match part.trim().split_once('=') {
                Some(("t", t)) => timestamp = t.parse::<i64>().ok(),
                Some(("v1", s)) => signatures.push(s.to_string()),
                _ => {}
            }
        }
        let timestamp = timestamp.context("no timestamp in Stripe-Signature")?;
        anyhow::ensure!((now - timestamp).abs() <= 300, "the webhook is too old");
        let mut signed = timestamp.to_string().into_bytes();
        signed.push(b'.');
        signed.extend_from_slice(payload);
        let good = self.webhook_secrets.iter().any(|secret| {
            signatures.iter().any(|sig| {
                let Ok(sig) = hex::decode(sig) else { return false };
                let Ok(mut mac) = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()) else { return false };
                mac.update(&signed);
                mac.verify_slice(&sig).is_ok()
            })
        });
        anyhow::ensure!(good, "the webhook signature does not match");
        Ok(serde_json::from_slice(payload)?)
    }

    /// A Stripe-Signature header for `payload`, for tests.
    pub fn sign_for_test(secret: &str, payload: &[u8], timestamp: i64) -> String {
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
        mac.update(format!("{timestamp}.").as_bytes());
        mac.update(payload);
        format!("t={timestamp},v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    // ----- HTTP -----

    async fn post(&self, path: &str, form: Vec<(String, String)>, account: Option<&str>) -> anyhow::Result<Value> {
        self.send(format!("{}{path}", self.api_base), Some(form), account).await
    }

    async fn get(&self, path: &str, account: Option<&str>) -> anyhow::Result<Value> {
        self.send(format!("{}{path}", self.api_base), None, account).await
    }

    /// One request, in a blocking thread. Stripe's error message becomes the error.
    async fn send(
        &self,
        url: String,
        form: Option<Vec<(String, String)>>,
        account: Option<&str>,
    ) -> anyhow::Result<Value> {
        let key = self.secret_key.clone();
        let account = account.map(str::to_string);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(30)))
                .build()
                .into();
            let auth = format!("Bearer {key}");
            let mut res = match form {
                Some(form) => {
                    let mut req = agent.post(&url).header("Authorization", &auth).header("Stripe-Version", API_VERSION);
                    if let Some(a) = &account {
                        req = req.header("Stripe-Account", a);
                    }
                    req.send_form(form)?
                }
                None => {
                    let mut req = agent.get(&url).header("Authorization", &auth).header("Stripe-Version", API_VERSION);
                    if let Some(a) = &account {
                        req = req.header("Stripe-Account", a);
                    }
                    req.call()?
                }
            };
            let status = res.status();
            let body = res.body_mut().read_to_string()?;
            let v: Value = serde_json::from_str(&body).with_context(|| format!("Stripe answered {status}"))?;
            if !status.is_success() {
                let message = v["error"]["message"]
                    .as_str()
                    .or(v["error_description"].as_str())
                    .unwrap_or("no message")
                    .to_string();
                anyhow::bail!("Stripe refused the request ({status}): {message}");
            }
            Ok(v)
        })
        .await?
    }
}

/// The form fields of a weekly payout schedule on `day`, such as "monday".
fn weekly_payouts(day: &str) -> Vec<(String, String)> {
    vec![
        ("settings[payouts][schedule][interval]".into(), "weekly".into()),
        ("settings[payouts][schedule][weekly_payout_days][]".into(), day.into()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_signatures() {
        let stripe = Stripe::for_test("http://unused", "whsec_test");
        let body = br#"{"type":"checkout.session.completed"}"#;
        let now = 1_800_000_000;
        let header = Stripe::sign_for_test("whsec_test", body, now);
        assert!(stripe.verify_webhook(body, &header, now + 10).is_ok());
        assert!(stripe.verify_webhook(body, &header, now + 400).is_err(), "too old");
        assert!(stripe.verify_webhook(b"{}", &header, now).is_err(), "another body");
        let wrong = Stripe::sign_for_test("whsec_other", body, now);
        assert!(stripe.verify_webhook(body, &wrong, now).is_err(), "another secret");
        assert!(stripe.verify_webhook(body, "v1=abcd", now).is_err(), "no timestamp");
    }

    #[test]
    fn session_status_from_json() {
        let v = serde_json::json!({
            "id": "cs_1", "payment_status": "paid", "amount_total": 4000, "currency": "cad",
            "payment_intent": "pi_1", "metadata": {"pac_id": "3", "order_id": "12"}
        });
        let s = SessionStatus::from_json(&v);
        assert!(s.paid && s.amount_total == 4000 && s.currency == "cad");
        assert_eq!((s.pac_id, s.order_id, s.payment_intent.as_deref()), (Some(3), Some(12), Some("pi_1")));
    }
}
