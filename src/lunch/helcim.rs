//! Card payments through a PAC's own Helcim account, with HelcimPay.js.
//!
//! The treasurer gives PAC Keeper an API token from the PAC's Helcim account.
//! The server keeps it sealed (AES-256-GCM) with a key made from the server's
//! secret key, so a copy of control.db alone does not show it.
//!
//! A payment: the server opens a checkout for the order's amount, with a new
//! invoice number, and the parent pays in Helcim's form on the page. Then the
//! server asks Helcim for the transactions with that invoice number. Only an
//! approved purchase of the order's amount in CAD pays the order. What the
//! browser reports is only a cue to ask; Helcim's answer decides.
//!
//! Helcim takes its fees in the PAC's account, and PAC Keeper takes none. With
//! PAC_HELCIM_PARTNER_TOKEN, every request names the operator as a Helcim
//! integration partner, for Helcim's revenue share.

use std::time::Duration;

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use anyhow::Context;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub struct Helcim {
    api_base: String,
    partner_token: Option<String>,
    seal_key: [u8; 32],
}

/// One checkout for one order.
pub struct CheckoutRequest<'a> {
    pub amount_cents: i64,
    /// New for each checkout, so that a payment belongs to one order.
    pub invoice_number: &'a str,
    pub contact_name: &'a str,
    pub description: &'a str,
}

/// A card transaction in the PAC's Helcim account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payment {
    pub transaction_id: String,
    pub amount_cents: i64,
    pub currency: String,
    pub approved: bool,
    pub purchase: bool,
}

impl Payment {
    /// True when this transaction pays an order of `amount_cents`.
    pub fn pays(&self, amount_cents: i64) -> bool {
        self.approved && self.purchase && self.currency.eq_ignore_ascii_case("CAD") && self.amount_cents == amount_cents
    }
}

impl Helcim {
    /// The key comes from the server's secret key. PAC_HELCIM_API changes the
    /// address, for tests.
    pub fn from_env(server_key: &[u8]) -> Helcim {
        let mut hash = Sha256::new();
        hash.update(b"pac-keeper helcim token v1");
        hash.update(server_key);
        Helcim {
            api_base: std::env::var("PAC_HELCIM_API").unwrap_or_else(|_| "https://api.helcim.com".into()),
            partner_token: std::env::var("PAC_HELCIM_PARTNER_TOKEN").ok().filter(|t| !t.trim().is_empty()),
            seal_key: hash.finalize().into(),
        }
    }

    pub fn for_test(api_base: &str) -> Helcim {
        Helcim { api_base: api_base.into(), partner_token: None, seal_key: [7; 32] }
    }

    /// Encrypts an API token for storage, as hex of the nonce and the sealed bytes.
    pub fn seal(&self, token: &str) -> anyhow::Result<String> {
        let mut nonce = [0u8; 12];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut nonce);
        let sealed = self
            .cipher()
            .encrypt(Nonce::from_slice(&nonce), token.as_bytes())
            .map_err(|_| anyhow::anyhow!("could not seal the token"))?;
        Ok(hex::encode([nonce.as_slice(), &sealed].concat()))
    }

    /// The API token from `seal`. Fails when the server's key changed.
    pub fn open(&self, sealed: &str) -> anyhow::Result<String> {
        let bytes = hex::decode(sealed).context("the stored token is damaged")?;
        anyhow::ensure!(bytes.len() > 12, "the stored token is damaged");
        let (nonce, sealed) = bytes.split_at(12);
        let plain = self
            .cipher()
            .decrypt(Nonce::from_slice(nonce), sealed)
            .map_err(|_| anyhow::anyhow!("the stored Helcim token cannot be read; connect Helcim again"))?;
        Ok(String::from_utf8(plain)?)
    }

    fn cipher(&self) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.seal_key))
    }

    /// Checks that Helcim takes the token.
    pub async fn check_token(&self, token: &str) -> anyhow::Result<()> {
        self.send(token, "GET", "/v2/connection-test", None).await.map(|_| ())
    }

    /// Opens a HelcimPay.js checkout and returns its checkout token.
    pub async fn open_checkout(&self, token: &str, r: &CheckoutRequest<'_>) -> anyhow::Result<String> {
        let amount = dollars(r.amount_cents);
        let body = json!({
            "paymentType": "purchase",
            "amount": amount,
            "currency": "CAD",
            "paymentMethod": "cc",
            "invoiceRequest": {
                "invoiceNumber": r.invoice_number,
                "contactName": r.contact_name,
                "lineItems": [{
                    "sku": "HOT-LUNCH",
                    "description": r.description,
                    "quantity": 1,
                    "price": amount,
                    "total": amount,
                }],
            },
        });
        let v = self.send(token, "POST", "/v2/helcim-pay/initialize", Some(body)).await?;
        v["checkoutToken"].as_str().map(str::to_string).context("Helcim sent no checkout token")
    }

    /// The transactions with this invoice number.
    pub async fn payments(&self, token: &str, invoice_number: &str) -> anyhow::Result<Vec<Payment>> {
        let path = format!("/v2/card-transactions?invoiceNumber={}", crate::web::urlencode(invoice_number));
        let v = self.send(token, "GET", &path, None).await?;
        let list = v.as_array().or_else(|| v["data"].as_array()).cloned().unwrap_or_default();
        Ok(list.iter().filter(|t| t["invoiceNumber"].as_str() == Some(invoice_number)).filter_map(payment).collect())
    }

    /// Refunds a whole payment. `ip` is the address of the person who asks,
    /// which Helcim requires.
    pub async fn refund(&self, token: &str, transaction_id: &str, amount_cents: i64, ip: &str) -> anyhow::Result<()> {
        let id: i64 = transaction_id.parse().context("the Helcim transaction number is not valid")?;
        let body = json!({ "originalTransactionId": id, "amount": dollars(amount_cents), "ipAddress": ip });
        self.send(token, "POST", "/v2/payment/refund", Some(body)).await.map(|_| ())
    }

    /// One request, in a blocking thread. Helcim's error messages become the error.
    async fn send(&self, token: &str, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let url = format!("{}{path}", self.api_base);
        let token = token.to_string();
        let partner = self.partner_token.clone();
        let post = method == "POST";
        tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(30)))
                .build()
                .into();
            let mut res = if post {
                let mut key = [0u8; 16];
                rand::Rng::fill_bytes(&mut rand::rng(), &mut key);
                let mut req = agent
                    .post(&url)
                    .header("api-token", &token)
                    .header("accept", "application/json")
                    .header("idempotency-key", hex::encode(key));
                if let Some(p) = &partner {
                    req = req.header("partner-token", p);
                }
                req.send_json(body.unwrap_or(Value::Null))?
            } else {
                let mut req = agent.get(&url).header("api-token", &token).header("accept", "application/json");
                if let Some(p) = &partner {
                    req = req.header("partner-token", p);
                }
                req.call()?
            };
            let status = res.status();
            let text = res.body_mut().read_to_string()?;
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if !status.is_success() {
                anyhow::bail!("{}", error_message(&v).unwrap_or_else(|| format!("Helcim answered {status}")));
            }
            Ok(v)
        })
        .await?
    }
}

/// An amount in dollars for Helcim's JSON, such as 40.25, made from text so
/// that no arithmetic happens in floating point.
fn dollars(cents: i64) -> Value {
    serde_json::from_str(&format!("{}.{:02}", cents / 100, cents % 100)).unwrap_or(Value::Null)
}

/// Cents from Helcim's amount, which is a number or text in dollars.
fn cents(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => crate::money::parse(&n.to_string()),
        Value::String(s) => crate::money::parse(s),
        _ => None,
    }
}

fn payment(t: &Value) -> Option<Payment> {
    let transaction_id = match &t["transactionId"] {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => return None,
    };
    Some(Payment {
        transaction_id,
        amount_cents: cents(&t["amount"])?,
        currency: t["currency"].as_str().unwrap_or_default().to_string(),
        approved: t["status"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("APPROVED")),
        purchase: t["type"].as_str().is_some_and(|s| s.eq_ignore_ascii_case("purchase")),
    })
}

/// Helcim lists errors as an array or an object of messages.
fn error_message(v: &Value) -> Option<String> {
    let messages: Vec<String> = match &v["errors"] {
        Value::Array(a) => a.iter().map(|m| m.as_str().map(str::to_string).unwrap_or_else(|| m.to_string())).collect(),
        Value::Object(o) => {
            o.values().map(|m| m.as_str().map(str::to_string).unwrap_or_else(|| m.to_string())).collect()
        }
        Value::String(s) => vec![s.clone()],
        _ => return v["message"].as_str().map(str::to_string),
    };
    (!messages.is_empty()).then(|| messages.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_token_opens_only_with_the_same_key() {
        let h = Helcim::for_test("http://127.0.0.1:9");
        let sealed = h.seal("token-123").unwrap();
        assert!(!sealed.contains("token"), "the token is not readable in the database");
        assert_eq!(h.open(&sealed).unwrap(), "token-123");
        assert_ne!(sealed, h.seal("token-123").unwrap(), "each seal has its own nonce");
        let other = Helcim { seal_key: [8; 32], ..Helcim::for_test("http://127.0.0.1:9") };
        assert!(other.open(&sealed).is_err());
        assert!(h.open("zz").is_err());
    }

    #[test]
    fn amounts_and_payments() {
        assert_eq!(dollars(4025).to_string(), "40.25");
        assert_eq!(dollars(500).to_string(), "5.0");
        assert_eq!(dollars(7).to_string(), "0.07");
        let t = json!({"transactionId": 2576, "amount": 40.25, "currency": "CAD", "status": "APPROVED", "type": "purchase"});
        let p = payment(&t).unwrap();
        assert_eq!((p.transaction_id.as_str(), p.amount_cents), ("2576", 4025));
        assert!(p.pays(4025) && !p.pays(4000));
        let declined = json!({"transactionId": "9", "amount": "40.25", "currency": "CAD", "status": "DECLINED", "type": "purchase"});
        assert!(!payment(&declined).unwrap().pays(4025));
        let usd =
            json!({"transactionId": 9, "amount": 40.25, "currency": "USD", "status": "APPROVED", "type": "purchase"});
        assert!(!payment(&usd).unwrap().pays(4025));
        assert_eq!(error_message(&json!({"errors": ["Invalid token"]})).unwrap(), "Invalid token");
        assert_eq!(error_message(&json!({"errors": {"amount": "too small"}})).unwrap(), "too small");
    }
}
