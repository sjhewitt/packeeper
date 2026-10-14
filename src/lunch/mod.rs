//! Hot lunch ordering: the PAC sets a menu and ordering periods, parents
//! order lunches for their children, and pay by card (Stripe), e-transfer, or
//! cash. It is behind the hot lunch feature flag and runs on a hosted server,
//! where parents are people in control.db (see `hosted::Hosted::add_family`).
//!
//! The records live in the PAC's own database (migrations/0004_hot_lunch.sql).
//! Money from hot lunch reaches the books the usual way: the Stripe payouts or
//! e-transfers show up in the bank import, on the hot lunch line item.

pub mod fees;
pub mod helcim;
pub mod stripe;

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{Datelike, NaiveDate};
use sqlx::{FromRow, SqlitePool};

use crate::db;
use crate::money::Money;

/// The server-wide hot lunch settings: on when the server runs with
/// --hot-lunch. Card payments through Stripe need the platform's Stripe keys
/// as well. Helcim needs nothing on the server: each PAC brings its own account.
pub struct Lunch {
    pub stripe: Option<stripe::Stripe>,
    pub fees: fees::FeeConfig,
    pub helcim: helcim::Helcim,
}

impl Lunch {
    /// `server_key` seals the PACs' Helcim tokens.
    pub fn from_env(server_key: &[u8]) -> anyhow::Result<Lunch> {
        Ok(Lunch {
            stripe: stripe::Stripe::from_env()?,
            fees: fees::FeeConfig::from_env()?,
            helcim: helcim::Helcim::from_env(server_key),
        })
    }
}

// ----- Per-PAC settings -----

const ENABLED: &str = "lunch_enabled";
const ETRANSFER_EMAIL: &str = "lunch_etransfer_email";
const ETRANSFER_NOTE: &str = "lunch_etransfer_note";
const CASH: &str = "lunch_cash";

async fn setting(pool: &SqlitePool, key: &str) -> anyhow::Result<String> {
    Ok(sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await?
        .unwrap_or_default())
}

async fn set_setting(pool: &SqlitePool, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(key)
        .bind(value)
        .execute(pool)
        .await?;
    Ok(())
}

/// The PAC's treasurer turned hot lunch on.
pub async fn enabled(pool: &SqlitePool) -> anyhow::Result<bool> {
    Ok(setting(pool, ENABLED).await? == "1")
}

pub async fn set_enabled(pool: &SqlitePool, on: bool) -> anyhow::Result<()> {
    set_setting(pool, ENABLED, if on { "1" } else { "0" }).await
}

/// Ways to pay without a card. The treasurer marks these orders paid.
#[derive(Debug, Clone, Default)]
pub struct Offline {
    /// The address that takes Interac e-Transfers; empty for none.
    pub etransfer_email: String,
    pub etransfer_note: String,
    pub cash: bool,
}

impl Offline {
    pub async fn load(pool: &SqlitePool) -> anyhow::Result<Offline> {
        Ok(Offline {
            etransfer_email: setting(pool, ETRANSFER_EMAIL).await?,
            etransfer_note: setting(pool, ETRANSFER_NOTE).await?,
            cash: setting(pool, CASH).await? == "1",
        })
    }

    pub async fn save(&self, pool: &SqlitePool) -> anyhow::Result<()> {
        set_setting(pool, ETRANSFER_EMAIL, self.etransfer_email.trim()).await?;
        set_setting(pool, ETRANSFER_NOTE, self.etransfer_note.trim()).await?;
        set_setting(pool, CASH, if self.cash { "1" } else { "0" }).await
    }

    pub fn etransfer(&self) -> bool {
        !self.etransfer_email.is_empty()
    }
}

// ----- Rows -----

#[derive(Debug, Clone, FromRow)]
pub struct Division {
    pub id: i64,
    pub name: String,
    pub sort_order: i64,
    pub active: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct Item {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub price_cents: i64,
    pub active: bool,
}

impl Item {
    pub fn price(&self) -> Money {
        Money(self.price_cents)
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Period {
    pub id: i64,
    pub name: String,
    pub order_deadline: NaiveDate,
    pub status: String,
    pub notes: String,
    #[allow(dead_code)]
    pub created_at: String,
}

impl Period {
    /// Parents can order: the period is open and the deadline has not passed.
    pub fn takes_orders(&self, today: NaiveDate) -> bool {
        self.status == "open" && today <= self.order_deadline
    }

    pub fn status_label(&self) -> &'static str {
        match self.status.as_str() {
            "open" => "open for orders",
            "closed" => "closed",
            _ => "draft, hidden from parents",
        }
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Day {
    pub id: i64,
    pub period_id: i64,
    pub date: NaiveDate,
}

impl Day {
    pub fn label(&self) -> String {
        self.date.format("%a %b %-d").to_string()
    }
}

/// An item on one day's menu.
#[derive(Debug, Clone, FromRow)]
pub struct DayItem {
    pub day_id: i64,
    pub item_id: i64,
    pub name: String,
    pub price_cents: i64,
}

impl DayItem {
    pub fn price(&self) -> Money {
        Money(self.price_cents)
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Child {
    pub id: i64,
    pub person_id: i64,
    pub name: String,
    pub division_id: Option<i64>,
    pub notes: String,
    pub active: bool,
    #[allow(dead_code)]
    pub created_at: String,
    pub division_name: Option<String>,
}

impl Child {
    pub fn division(&self) -> &str {
        self.division_name.as_deref().unwrap_or("no division")
    }

    pub fn in_division(&self, id: &i64) -> bool {
        self.division_id == Some(*id)
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Order {
    pub id: i64,
    pub person_id: i64,
    pub period_id: i64,
    pub method: String,
    pub status: String,
    pub total_cents: i64,
    pub fee_cents: i64,
    pub monthly_fee_included: bool,
    pub stripe_session_id: Option<String>,
    pub stripe_payment_intent: Option<String>,
    pub created_at: String,
    pub paid_at: Option<String>,
    /// The invoice number of the latest Helcim checkout.
    pub helcim_invoice: Option<String>,
    pub helcim_transaction: Option<String>,
}

impl Order {
    pub fn total(&self) -> Money {
        Money(self.total_cents)
    }

    pub fn fee(&self) -> Money {
        Money(self.fee_cents)
    }

    /// What reaches the PAC after the platform's fee.
    pub fn net(&self) -> Money {
        Money(self.total_cents - self.fee_cents)
    }

    pub fn method_label(&self) -> &'static str {
        match self.method.as_str() {
            "card" => "card",
            "etransfer" => "e-transfer",
            _ => "cash",
        }
    }

    pub fn status_label(&self) -> &'static str {
        match (self.status.as_str(), self.method.as_str()) {
            ("paid", _) => "paid",
            ("cancelled", _) => "cancelled",
            ("refunded", _) => "refunded",
            ("pending", "card") => "not paid",
            _ => "waiting for payment",
        }
    }

    /// Counts toward the lunches to buy: paid, or promised by e-transfer or cash.
    pub fn counts(&self) -> bool {
        self.status == "paid" || (self.status == "pending" && self.method != "card")
    }

    pub fn reference(&self) -> String {
        format!("HL-{}", self.id)
    }
}

/// One line of an order, with names for display.
#[derive(Debug, Clone, FromRow)]
pub struct LineView {
    pub order_id: i64,
    pub child_name: String,
    pub division_name: Option<String>,
    pub date: NaiveDate,
    pub item_name: String,
    pub quantity: i64,
    pub price_cents: i64,
}

impl LineView {
    pub fn amount(&self) -> Money {
        Money(self.price_cents * self.quantity)
    }

    pub fn day_label(&self) -> String {
        self.date.format("%a %b %-d").to_string()
    }

    pub fn division(&self) -> &str {
        self.division_name.as_deref().unwrap_or("no division")
    }
}

// ----- Menu and divisions -----

pub async fn divisions(pool: &SqlitePool) -> anyhow::Result<Vec<Division>> {
    Ok(sqlx::query_as("SELECT * FROM lunch_divisions ORDER BY active DESC, sort_order, name").fetch_all(pool).await?)
}

pub async fn items(pool: &SqlitePool) -> anyhow::Result<Vec<Item>> {
    Ok(sqlx::query_as("SELECT * FROM lunch_items ORDER BY active DESC, name").fetch_all(pool).await?)
}

// ----- Periods and days -----

pub async fn periods(pool: &SqlitePool) -> anyhow::Result<Vec<Period>> {
    Ok(sqlx::query_as("SELECT * FROM lunch_periods ORDER BY order_deadline DESC, id DESC").fetch_all(pool).await?)
}

pub async fn period(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Period>> {
    Ok(sqlx::query_as("SELECT * FROM lunch_periods WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

/// The period's days, each with its menu.
pub async fn days(pool: &SqlitePool, period_id: i64) -> anyhow::Result<Vec<(Day, Vec<DayItem>)>> {
    let days: Vec<Day> = sqlx::query_as("SELECT * FROM lunch_days WHERE period_id = ? ORDER BY date")
        .bind(period_id)
        .fetch_all(pool)
        .await?;
    let items: Vec<DayItem> = sqlx::query_as(
        "SELECT di.day_id, di.item_id, i.name, di.price_cents
         FROM lunch_day_items di JOIN lunch_items i ON i.id = di.item_id JOIN lunch_days d ON d.id = di.day_id
         WHERE d.period_id = ? ORDER BY i.name",
    )
    .bind(period_id)
    .fetch_all(pool)
    .await?;
    let mut by_day: HashMap<i64, Vec<DayItem>> = HashMap::new();
    for item in items {
        by_day.entry(item.day_id).or_default().push(item);
    }
    Ok(days
        .into_iter()
        .map(|d| {
            let menu = by_day.remove(&d.id).unwrap_or_default();
            (d, menu)
        })
        .collect())
}

/// Adds lunch days from `from` to `to` on the chosen weekdays (1 = Monday),
/// each offering `item_ids` at their menu prices. Days that exist stay as they are.
pub async fn add_days(
    pool: &SqlitePool,
    period_id: i64,
    from: NaiveDate,
    to: NaiveDate,
    weekdays: &[u32],
    item_ids: &[i64],
) -> anyhow::Result<usize> {
    let prices: HashMap<i64, i64> = items(pool).await?.into_iter().map(|i| (i.id, i.price_cents)).collect();
    let mut added = 0;
    let mut tx = pool.begin().await?;
    let mut date = from;
    while date <= to {
        if weekdays.contains(&date.weekday().number_from_monday()) {
            let inserted = sqlx::query("INSERT OR IGNORE INTO lunch_days (period_id, date) VALUES (?, ?)")
                .bind(period_id)
                .bind(date)
                .execute(&mut *tx)
                .await?;
            if inserted.rows_affected() == 1 {
                let day_id = inserted.last_insert_rowid();
                for item_id in item_ids {
                    if let Some(price) = prices.get(item_id) {
                        sqlx::query("INSERT INTO lunch_day_items (day_id, item_id, price_cents) VALUES (?, ?, ?)")
                            .bind(day_id)
                            .bind(item_id)
                            .bind(price)
                            .execute(&mut *tx)
                            .await?;
                    }
                }
                added += 1;
            }
        }
        date = date.succ_opt().expect("date in range");
    }
    tx.commit().await?;
    Ok(added)
}

/// Sets one day's menu. Items already ordered that day stay on it.
pub async fn set_day_items(pool: &SqlitePool, day_id: i64, item_ids: &[i64]) -> anyhow::Result<()> {
    let prices: HashMap<i64, i64> = items(pool).await?.into_iter().map(|i| (i.id, i.price_cents)).collect();
    let ordered: HashSet<i64> =
        sqlx::query_scalar::<_, i64>("SELECT DISTINCT item_id FROM lunch_order_lines WHERE day_id = ?")
            .bind(day_id)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    let mut tx = pool.begin().await?;
    let current: Vec<i64> = sqlx::query_scalar("SELECT item_id FROM lunch_day_items WHERE day_id = ?")
        .bind(day_id)
        .fetch_all(&mut *tx)
        .await?;
    for item in current.iter().filter(|i| !item_ids.contains(i) && !ordered.contains(i)) {
        sqlx::query("DELETE FROM lunch_day_items WHERE day_id = ? AND item_id = ?")
            .bind(day_id)
            .bind(item)
            .execute(&mut *tx)
            .await?;
    }
    for item in item_ids.iter().filter(|i| !current.contains(i)) {
        if let Some(price) = prices.get(item) {
            sqlx::query("INSERT INTO lunch_day_items (day_id, item_id, price_cents) VALUES (?, ?, ?)")
                .bind(day_id)
                .bind(item)
                .bind(price)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

// ----- Children -----

const CHILD_SELECT: &str = "SELECT c.*, d.name AS division_name FROM lunch_children c
                            LEFT JOIN lunch_divisions d ON d.id = c.division_id";

pub async fn children(pool: &SqlitePool, person_id: i64) -> anyhow::Result<Vec<Child>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{CHILD_SELECT} WHERE c.person_id = ? ORDER BY c.active DESC, c.name"
    )))
    .bind(person_id)
    .fetch_all(pool)
    .await?)
}

pub async fn all_children(pool: &SqlitePool) -> anyhow::Result<Vec<Child>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{CHILD_SELECT} WHERE c.active = 1 ORDER BY d.sort_order, d.name, c.name"
    )))
    .fetch_all(pool)
    .await?)
}

// ----- Orders -----

/// One lunch a parent picked: a child, a day, an item, and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub child_id: i64,
    pub day_id: i64,
    pub item_id: i64,
    pub quantity: i64,
}

/// Saves a pending order. The prices come from the day's menu, never from the
/// form. An `Err(String)` is a message for the parent.
pub async fn create_order(
    pool: &SqlitePool,
    person_id: i64,
    period: &Period,
    method: &str,
    choices: &[Choice],
    today: NaiveDate,
) -> anyhow::Result<Result<Order, String>> {
    if !period.takes_orders(today) {
        return Ok(Err("This period does not take orders any more.".into()));
    }
    if !matches!(method, "card" | "etransfer" | "cash") {
        return Ok(Err("Choose how to pay.".into()));
    }
    if choices.is_empty() {
        return Ok(Err("Choose at least one lunch.".into()));
    }
    let kids: HashSet<i64> = children(pool, person_id).await?.into_iter().filter(|c| c.active).map(|c| c.id).collect();
    let mut menu: HashMap<(i64, i64), i64> = HashMap::new();
    for (day, items) in days(pool, period.id).await? {
        for item in items {
            menu.insert((day.id, item.item_id), item.price_cents);
        }
    }
    let mut total = 0;
    for c in choices {
        if !kids.contains(&c.child_id) {
            return Ok(Err("Choose lunches only for your own children.".into()));
        }
        if !(1..=5).contains(&c.quantity) {
            return Ok(Err("Order 1 to 5 of an item a day.".into()));
        }
        let Some(price) = menu.get(&(c.day_id, c.item_id)) else {
            return Ok(Err("An item is not on that day's menu. Reload the page and try again.".into()));
        };
        total += price * c.quantity;
    }
    let mut tx = pool.begin().await?;
    let id = sqlx::query(
        "INSERT INTO lunch_orders (person_id, period_id, method, total_cents, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(person_id)
    .bind(period.id)
    .bind(method)
    .bind(total)
    .bind(db::now())
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    for c in choices {
        sqlx::query(
            "INSERT INTO lunch_order_lines (order_id, child_id, day_id, item_id, quantity, price_cents) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(c.child_id)
        .bind(c.day_id)
        .bind(c.item_id)
        .bind(c.quantity)
        .bind(menu[&(c.day_id, c.item_id)])
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Ok(order(pool, id).await?.expect("just saved")))
}

pub async fn order(pool: &SqlitePool, id: i64) -> anyhow::Result<Option<Order>> {
    Ok(sqlx::query_as("SELECT * FROM lunch_orders WHERE id = ?").bind(id).fetch_optional(pool).await?)
}

pub async fn order_by_session(pool: &SqlitePool, session_id: &str) -> anyhow::Result<Option<Order>> {
    Ok(sqlx::query_as("SELECT * FROM lunch_orders WHERE stripe_session_id = ?")
        .bind(session_id)
        .fetch_optional(pool)
        .await?)
}

pub async fn orders_for_person(pool: &SqlitePool, person_id: i64) -> anyhow::Result<Vec<Order>> {
    Ok(sqlx::query_as("SELECT * FROM lunch_orders WHERE person_id = ? ORDER BY id DESC")
        .bind(person_id)
        .fetch_all(pool)
        .await?)
}

pub async fn orders_for_period(pool: &SqlitePool, period_id: i64) -> anyhow::Result<Vec<Order>> {
    Ok(sqlx::query_as("SELECT * FROM lunch_orders WHERE period_id = ? ORDER BY id DESC")
        .bind(period_id)
        .fetch_all(pool)
        .await?)
}

const LINE_SELECT: &str = "SELECT l.order_id, c.name AS child_name, dv.name AS division_name, d.date,
                                  i.name AS item_name, l.quantity, l.price_cents
                           FROM lunch_order_lines l
                           JOIN lunch_children c ON c.id = l.child_id
                           LEFT JOIN lunch_divisions dv ON dv.id = c.division_id
                           JOIN lunch_days d ON d.id = l.day_id
                           JOIN lunch_items i ON i.id = l.item_id";

pub async fn order_lines(pool: &SqlitePool, order_id: i64) -> anyhow::Result<Vec<LineView>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LINE_SELECT} WHERE l.order_id = ? ORDER BY d.date, c.name, i.name"
    )))
    .bind(order_id)
    .fetch_all(pool)
    .await?)
}

/// Saves the Stripe Checkout session of a card order and the fee it carries.
pub async fn set_checkout(
    pool: &SqlitePool,
    order_id: i64,
    session_id: &str,
    fee_cents: i64,
    monthly_fee: bool,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE lunch_orders SET stripe_session_id = ?, fee_cents = ?, monthly_fee_included = ? WHERE id = ? AND status = 'pending'",
    )
    .bind(session_id)
    .bind(fee_cents)
    .bind(monthly_fee)
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks a pending order paid. Returns false if it was not pending (for
/// example, the webhook and the return page both report one payment).
pub async fn mark_paid(pool: &SqlitePool, order_id: i64, payment_intent: Option<&str>) -> anyhow::Result<bool> {
    let done = sqlx::query(
        "UPDATE lunch_orders SET status = 'paid', paid_at = ?, stripe_payment_intent = COALESCE(?, stripe_payment_intent)
         WHERE id = ? AND status = 'pending'",
    )
    .bind(db::now())
    .bind(payment_intent)
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Starts a Helcim checkout for a pending card order: its new invoice number,
/// and no PAC Keeper fee.
pub async fn set_helcim_invoice(pool: &SqlitePool, order_id: i64, invoice: &str) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE lunch_orders SET helcim_invoice = ?, fee_cents = 0, monthly_fee_included = 0
         WHERE id = ? AND status = 'pending'",
    )
    .bind(invoice)
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks a pending order paid by Helcim transaction `transaction_id`, once.
pub async fn mark_paid_helcim(pool: &SqlitePool, order_id: i64, transaction_id: &str) -> anyhow::Result<bool> {
    let done = sqlx::query(
        "UPDATE lunch_orders SET status = 'paid', paid_at = ?, helcim_transaction = ? WHERE id = ? AND status = 'pending'",
    )
    .bind(db::now())
    .bind(transaction_id)
    .bind(order_id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Card orders that wait for a Helcim payment, made at `since` or later.
pub async fn waiting_for_helcim(pool: &SqlitePool, since: &str) -> anyhow::Result<Vec<Order>> {
    Ok(sqlx::query_as(
        "SELECT * FROM lunch_orders
         WHERE status = 'pending' AND method = 'card' AND helcim_invoice IS NOT NULL AND created_at >= ?",
    )
    .bind(since)
    .fetch_all(pool)
    .await?)
}

/// Cancels a pending order, or marks a paid one refunded.
pub async fn close_order(pool: &SqlitePool, order_id: i64, status: &str) -> anyhow::Result<bool> {
    let from = if status == "refunded" { "paid" } else { "pending" };
    let done = sqlx::query("UPDATE lunch_orders SET status = ? WHERE id = ? AND status = ?")
        .bind(status)
        .bind(order_id)
        .bind(from)
        .execute(pool)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// True when no card payment this month has carried the month's Express
/// account fee yet.
pub async fn monthly_fee_due(pool: &SqlitePool, today: NaiveDate) -> anyhow::Result<bool> {
    let month = today.format("%Y-%m").to_string();
    let paid: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM lunch_orders WHERE monthly_fee_included = 1 AND status IN ('paid', 'refunded')
           AND substr(paid_at, 1, 7) = ?",
    )
    .bind(month)
    .fetch_one(pool)
    .await?;
    Ok(paid == 0)
}

// ----- Reports -----

/// How many of each item to buy for one day: in total, and per division.
#[derive(Debug, Clone, Default)]
pub struct DayCount {
    pub day: Option<Day>,
    /// (item name, quantity), by item name.
    pub items: Vec<(String, i64)>,
    /// (division, [(item name, quantity)]), by division.
    pub divisions: Vec<(String, Vec<(String, i64)>)>,
}

impl DayCount {
    pub fn total(&self) -> i64 {
        self.items.iter().map(|(_, n)| n).sum()
    }
}

/// The lunches to buy each day of a period: paid orders, and orders promised
/// by e-transfer or cash. Card orders not paid yet do not count.
pub async fn day_counts(pool: &SqlitePool, period_id: i64) -> anyhow::Result<Vec<DayCount>> {
    #[derive(FromRow)]
    struct Row {
        day_id: i64,
        division: Option<String>,
        item: String,
        quantity: i64,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT l.day_id, dv.name AS division, i.name AS item, SUM(l.quantity) AS quantity
         FROM lunch_order_lines l
         JOIN lunch_orders o ON o.id = l.order_id
         JOIN lunch_children c ON c.id = l.child_id
         LEFT JOIN lunch_divisions dv ON dv.id = c.division_id
         JOIN lunch_items i ON i.id = l.item_id
         WHERE o.period_id = ? AND (o.status = 'paid' OR (o.status = 'pending' AND o.method != 'card'))
         GROUP BY l.day_id, dv.name, i.name",
    )
    .bind(period_id)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::new();
    for (day, _) in days(pool, period_id).await? {
        let mut items: BTreeMap<String, i64> = BTreeMap::new();
        let mut divisions: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
        for r in rows.iter().filter(|r| r.day_id == day.id) {
            *items.entry(r.item.clone()).or_default() += r.quantity;
            let division = r.division.clone().unwrap_or_else(|| "No division".into());
            *divisions.entry(division).or_default().entry(r.item.clone()).or_default() += r.quantity;
        }
        out.push(DayCount {
            day: Some(day),
            items: items.into_iter().collect(),
            divisions: divisions.into_iter().map(|(d, items)| (d, items.into_iter().collect())).collect(),
        });
    }
    Ok(out)
}

/// Every lunch on one day, for the delivery sheet: by division, then child.
pub async fn day_sheet(pool: &SqlitePool, day_id: i64) -> anyhow::Result<Vec<LineView>> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{LINE_SELECT} JOIN lunch_orders o ON o.id = l.order_id
         WHERE l.day_id = ? AND (o.status = 'paid' OR (o.status = 'pending' AND o.method != 'card'))
         ORDER BY dv.sort_order, dv.name, c.name, i.name"
    )))
    .bind(day_id)
    .fetch_all(pool)
    .await?)
}

/// Money for a period: what parents paid, the platform's fees, and what is
/// still promised by e-transfer or cash.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeriodMoney {
    pub paid: Money,
    pub fees: Money,
    pub waiting: Money,
    pub orders: i64,
}

impl PeriodMoney {
    pub fn net(&self) -> Money {
        self.paid - self.fees
    }
}

pub fn period_money(orders: &[Order]) -> PeriodMoney {
    let mut m = PeriodMoney::default();
    for o in orders {
        match o.status.as_str() {
            "paid" => {
                m.paid += o.total();
                m.fees += o.fee();
                m.orders += 1;
            }
            "pending" if o.method != "card" => {
                m.waiting += o.total();
                m.orders += 1;
            }
            _ => {}
        }
    }
    m
}

#[cfg(test)]
mod tests;
