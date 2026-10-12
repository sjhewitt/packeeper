//! The Gaming Account Summary Report (GASR) for BC gaming grants and licensed
//! gaming events, plus checks against the PAC grant conditions.
//!
//! The GASR is on a bank basis: only transactions the bank has cleared count.
//! A cheque not cashed by year end goes on the next year's report.

use std::collections::{BTreeMap, HashMap};

use chrono::{Days, Months, NaiveDate};
use sqlx::SqlitePool;

use crate::db;
use crate::models::{Account, FiscalYear, GamingEvent, LineItem, Transaction};
use crate::money::Money;

/// PACs must spend each grant within 24 months of receipt.
pub const SPEND_WITHIN_MONTHS: u32 = 24;
/// The GASR is due within 90 days after the fiscal year end.
pub const DUE_WITHIN_DAYS: u64 = 90;

/// A payment itemized on lines 30 to 45.
#[derive(Debug, Clone)]
pub struct Disbursement {
    pub tx: Transaction,
    pub amount: Money,
    pub disbursed_to: String,
    pub purpose: String,
    pub reference: String,
    pub line_names: String,
    pub has_receipt: bool,
    pub receipt_required: bool,
    /// General-account expenses that this transfer repaid.
    pub reimbursements: Vec<Transaction>,
}

impl Disbursement {
    pub fn reimbursement_total(&self) -> Money {
        self.reimbursements.iter().map(|t| -t.amount()).sum()
    }
}

/// A prize or expense for a licensed gaming event (Section 4).
#[derive(Debug, Clone)]
pub struct EventCost {
    pub tx: Transaction,
    pub event: Option<GamingEvent>,
    /// "prize" or "event_expense".
    pub kind: &'static str,
    pub amount: Money,
}

impl EventCost {
    pub fn kind_label(&self) -> &'static str {
        if self.kind == "prize" { "Prize" } else { "Expense" }
    }
}

#[derive(Debug, Clone)]
pub struct Deposit {
    pub tx: Transaction,
    pub amount: Money,
    /// The GASR lines this deposit goes on, such as "3" or "6".
    pub lines: String,
    pub allowed: bool,
}

/// Gross revenue from one licence (one of lines 6 to 13).
#[derive(Debug, Clone)]
pub struct EventRevenue {
    /// None for event revenue with no licence chosen.
    pub event: Option<GamingEvent>,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub amount: Money,
}

#[derive(Debug, Clone)]
pub struct NamedAmount {
    pub name: String,
    pub amount: Money,
}

/// Section 4 costs, split the way the form splits them.
#[derive(Debug, Clone, Default)]
pub struct CostLines {
    pub bingo: Money,
    pub ticket_raffle: Money,
    pub other: Money,
    /// What the "other" amount is, such as "Class B: wheel of fortune".
    pub other_types: Vec<String>,
}

impl CostLines {
    pub fn total(&self) -> Money {
        self.bingo + self.ticket_raffle + self.other
    }

    fn add(&mut self, event: Option<&GamingEvent>, cents: i64) {
        match event.map(|e| e.event_type.as_str()) {
            Some("bingo") => self.bingo += Money(cents),
            Some("ticket_raffle") => self.ticket_raffle += Money(cents),
            _ => {
                self.other += Money(cents);
                let label = match event {
                    Some(e) => format!("Class {}: {}", e.licence_class, e.type_label()),
                    None => "licence not chosen".to_string(),
                };
                if !self.other_types.contains(&label) {
                    self.other_types.push(label);
                }
            }
        }
    }

    pub fn other_label(&self) -> String {
        self.other_types.join(", ")
    }
}

#[derive(Debug, Clone)]
pub struct GrantRow {
    pub received: NaiveDate,
    pub amount: Money,
    pub spend_by: NaiveDate,
    pub unspent: Money,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Problem,
    Warning,
    Info,
}

impl Level {
    pub fn css(self) -> &'static str {
        match self {
            Level::Problem => "problem",
            Level::Warning => "warning",
            Level::Info => "info",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub level: Level,
    pub message: String,
    pub tx_id: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct GamingReport {
    pub fy: FiscalYear,
    /// Line 1
    pub opening: Money,
    /// Line 2
    pub community_grant: Money,
    /// Line 3
    pub grant: Money,
    /// Line 5, with a description of each part.
    pub other_income: Vec<NamedAmount>,
    /// The part of line 5 that is not gaming revenue, such as general funds.
    pub not_allowed: Money,
    /// Lines 6 to 13, one per licence.
    pub events: Vec<EventRevenue>,
    /// Line 14
    pub interest_gst: Money,
    /// Lines 15 to 17, one per organization.
    pub donations: Vec<NamedAmount>,
    /// Lines 19 to 22
    pub prizes: CostLines,
    /// Lines 23 to 26
    pub event_expenses: CostLines,
    /// Lines 30 to 45
    pub disbursements: Vec<Disbursement>,
    /// The payments behind lines 19 to 26.
    pub event_costs: Vec<EventCost>,
    /// Section 7: each gaming account's bank balance at year end.
    pub accounts: Vec<(Account, Money)>,
    /// Line 52
    pub accounts_total: Money,
    pub transfers_net: Money,
    pub deposits: Vec<Deposit>,
    pub grants: Vec<GrantRow>,
    pub outstanding: Vec<Transaction>,
    pub checks: Vec<Check>,
    pub due_date: NaiveDate,
}

impl GamingReport {
    pub fn other_income_total(&self) -> Money {
        self.other_income.iter().map(|o| o.amount).sum()
    }

    pub fn event_revenue(&self) -> Money {
        self.events.iter().map(|e| e.amount).sum()
    }

    pub fn donations_total(&self) -> Money {
        self.donations.iter().map(|d| d.amount).sum()
    }

    /// Line 18
    pub fn total_revenue(&self) -> Money {
        self.community_grant
            + self.grant
            + self.other_income_total()
            + self.event_revenue()
            + self.interest_gst
            + self.donations_total()
    }

    /// Line 27
    pub fn net_proceeds(&self) -> Money {
        self.total_revenue() - self.prizes.total() - self.event_expenses.total()
    }

    /// Line 28
    pub fn available(&self) -> Money {
        self.opening + self.net_proceeds()
    }

    /// Line 46
    pub fn total_disbursed(&self) -> Money {
        self.disbursements.iter().map(|d| d.amount).sum()
    }

    /// Line 47
    pub fn closing(&self) -> Money {
        self.available() - self.total_disbursed()
    }

    pub fn has_events(&self) -> bool {
        !self.events.is_empty() || !self.prizes.total().is_zero() || !self.event_expenses.total().is_zero()
    }

    pub fn problems(&self) -> usize {
        self.checks.iter().filter(|c| c.level == Level::Problem).count()
    }

    pub fn warnings(&self) -> usize {
        self.checks.iter().filter(|c| c.level == Level::Warning).count()
    }
}

/// GASR date style: DD-MM-YYYY.
pub fn gasr_date(d: NaiveDate) -> String {
    d.format("%d-%m-%Y").to_string()
}

/// The parts of one transaction, sorted by where they go on the report.
#[derive(Default)]
struct Parts {
    community_grant: i64,
    grant: i64,
    other_ok: i64,
    other_bad: i64,
    event_revenue: i64,
    interest: i64,
    donation: i64,
    prize: i64,
    event_expense: i64,
    spent: i64,
    transfer: i64,
    receipt_required: bool,
    names: Vec<String>,
}

impl Parts {
    fn income(&self) -> i64 {
        self.community_grant
            + self.grant
            + self.other_ok
            + self.other_bad
            + self.event_revenue
            + self.interest
            + self.donation
    }

    fn lines(&self) -> String {
        let mut l = Vec::new();
        for (cents, line) in [
            (self.community_grant, "2"),
            (self.grant, "3"),
            (self.other_ok + self.other_bad, "5"),
            (self.event_revenue, "6–13"),
            (self.interest, "14"),
            (self.donation, "15–17"),
        ] {
            if cents != 0 {
                l.push(line);
            }
        }
        l.join(", ")
    }
}

fn split(tx: &Transaction, allocs: &[crate::models::Allocation], items: &HashMap<i64, LineItem>) -> Parts {
    let mut p = Parts::default();
    for al in allocs {
        let Some(li) = items.get(&al.line_item_id) else { continue };
        p.names.push(li.name.clone());
        let a = al.amount_cents;
        match (li.kind.as_str(), li.gasr_line.as_deref()) {
            ("income", Some("2")) => p.community_grant += a,
            ("income", Some("3")) => p.grant += a,
            ("income", Some("5")) => p.other_ok += a,
            ("income", Some("6")) => p.event_revenue += a,
            ("income", Some("14")) => p.interest += a,
            ("income", Some("15")) => p.donation += a,
            ("income", _) => p.other_bad += a,
            ("expense", code) => {
                p.receipt_required |= li.receipt_required;
                match code {
                    Some("prize") => p.prize -= a,
                    Some("event_expense") => p.event_expense -= a,
                    _ => p.spent -= a,
                }
            }
            _ => p.transfer += a,
        }
    }
    let remainder = tx.amount_cents - allocs.iter().map(|a| a.amount_cents).sum::<i64>();
    if remainder > 0 {
        p.other_bad += remainder;
    } else if remainder < 0 {
        p.spent -= remainder;
        p.receipt_required = true;
    }
    p
}

pub async fn build(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<GamingReport> {
    let accounts: Vec<Account> = db::accounts(pool).await?.into_iter().filter(|a| a.is_gaming()).collect();
    let items: HashMap<i64, LineItem> = db::line_items(pool).await?.into_iter().map(|li| (li.id, li)).collect();
    let events: HashMap<i64, GamingEvent> = db::gaming_events(pool).await?.into_iter().map(|e| (e.id, e)).collect();
    let mut opening = 0;
    let mut account_balances = Vec::new();
    for a in accounts.iter().filter(|a| a.opening_date <= fy.end_date) {
        // An account first tracked during the year adds its opening balance to line 1.
        opening += db::balance_before(pool, a, fy.start_date.max(a.opening_date), true).await?;
        account_balances.push((a.clone(), Money(db::balance_at(pool, a, fy.end_date, true).await?)));
    }

    let mut report = GamingReport {
        fy: fy.clone(),
        opening: Money(opening),
        community_grant: Money(0),
        grant: Money(0),
        other_income: Vec::new(),
        not_allowed: Money(0),
        events: Vec::new(),
        interest_gst: Money(0),
        donations: Vec::new(),
        prizes: CostLines::default(),
        event_expenses: CostLines::default(),
        disbursements: Vec::new(),
        event_costs: Vec::new(),
        accounts_total: account_balances.iter().map(|(_, b)| *b).sum(),
        accounts: account_balances,
        transfers_net: Money(0),
        deposits: Vec::new(),
        grants: Vec::new(),
        outstanding: Vec::new(),
        checks: Vec::new(),
        due_date: fy.end_date + Days::new(DUE_WITHIN_DAYS),
    };

    let txs: Vec<Transaction> = sqlx::query_as(
        "SELECT t.* FROM transactions t JOIN accounts a ON a.id = t.account_id
         WHERE a.kind = 'gaming' AND t.cleared = 1 AND t.date >= ? AND t.date <= ? AND t.date >= a.opening_date
         ORDER BY t.date, t.id",
    )
    .bind(fy.start_date)
    .bind(fy.end_date)
    .fetch_all(pool)
    .await?;

    // Revenue per licence (None = no licence chosen), with first and last deposit dates.
    let mut by_event: BTreeMap<Option<i64>, (i64, NaiveDate, NaiveDate)> = BTreeMap::new();
    let mut donors: BTreeMap<String, i64> = BTreeMap::new();

    for tx in txs {
        let allocs = crate::ledger::allocations(pool, tx.id).await?;
        let p = split(&tx, &allocs, &items);
        let event = tx.gaming_event_id.and_then(|id| events.get(&id));
        let what = format!("{} {} \"{}\"", tx.date, tx.amount(), tx.display_name());
        let push =
            |checks: &mut Vec<Check>, level, message: String| checks.push(Check { level, message, tx_id: Some(tx.id) });

        if allocs.iter().map(|a| a.amount_cents).sum::<i64>() != tx.amount_cents {
            let rest = Money(tx.amount_cents - allocs.iter().map(|a| a.amount_cents).sum::<i64>());
            push(&mut report.checks, Level::Warning, format!("{what} has {} with no line item.", rest.abs()));
        }
        report.transfers_net += Money(p.transfer);

        if p.income() != 0 {
            report.community_grant += Money(p.community_grant);
            report.grant += Money(p.grant);
            report.interest_gst += Money(p.interest);
            let describe = if tx.purpose.is_empty() { tx.display_name().to_string() } else { tx.purpose.clone() };
            if p.other_ok != 0 {
                report.other_income.push(NamedAmount { name: describe.clone(), amount: Money(p.other_ok) });
            }
            if p.other_bad != 0 {
                report.other_income.push(NamedAmount { name: describe, amount: Money(p.other_bad) });
                if p.other_bad > 0 {
                    report.not_allowed += Money(p.other_bad);
                    push(
                        &mut report.checks,
                        Level::Problem,
                        format!(
                            "{what}: {} is not gaming revenue. Only grants, licensed gaming event revenue, interest, \
                             GST rebates, and gaming donations may go into the gaming account. If it is, give it a line \
                             item with the right gaming report line.",
                            Money(p.other_bad)
                        ),
                    );
                }
            }
            if p.event_revenue != 0 {
                let entry = by_event.entry(event.map(|e| e.id)).or_insert((0, tx.date, tx.date));
                entry.0 += p.event_revenue;
                entry.1 = entry.1.min(tx.date);
                entry.2 = entry.2.max(tx.date);
                if event.is_none() {
                    push(
                        &mut report.checks,
                        Level::Warning,
                        format!("{what} is licensed gaming event revenue. Choose its licence on the transaction."),
                    );
                }
            }
            if p.donation != 0 {
                *donors.entry(tx.display_name().to_string()).or_default() += p.donation;
            }
            report.deposits.push(Deposit {
                tx: tx.clone(),
                amount: Money(p.income()),
                lines: p.lines(),
                allowed: p.other_bad <= 0,
            });
        }

        let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attachments WHERE transaction_id = ?")
            .bind(tx.id)
            .fetch_one(pool)
            .await?;

        for (kind, cents) in [("prize", p.prize), ("event_expense", p.event_expense)] {
            if cents == 0 {
                continue;
            }
            if kind == "prize" {
                report.prizes.add(event, cents);
            } else {
                report.event_expenses.add(event, cents);
            }
            if event.is_none() {
                push(
                    &mut report.checks,
                    Level::Warning,
                    format!("{what} is a licensed gaming event cost. Choose its licence on the transaction."),
                );
            }
            report.event_costs.push(EventCost { tx: tx.clone(), event: event.cloned(), kind, amount: Money(cents) });
        }
        if (p.prize != 0 || p.event_expense != 0) && p.spent == 0 && p.receipt_required && receipts == 0 {
            push(&mut report.checks, Level::Warning, format!("{what} has no receipt."));
        }

        if p.spent != 0 {
            let reimbursements: Vec<Transaction> =
                sqlx::query_as("SELECT * FROM transactions WHERE reimbursed_by_id = ? ORDER BY date, id")
                    .bind(tx.id)
                    .fetch_all(pool)
                    .await?;
            let line_names = p.names.join("; ");
            let d = Disbursement {
                amount: Money(p.spent),
                disbursed_to: tx.display_name().to_string(),
                purpose: if tx.purpose.is_empty() { line_names.clone() } else { tx.purpose.clone() },
                reference: tx.reference.clone(),
                line_names,
                has_receipt: receipts > 0,
                receipt_required: p.receipt_required,
                reimbursements,
                tx,
            };
            check_disbursement(pool, fy, &d, &mut report.checks).await?;
            report.disbursements.push(d);
        }
    }

    for (event_id, (cents, first, last)) in by_event {
        let event = event_id.and_then(|id| events.get(&id)).cloned();
        // The reporting period is the licensed activity that falls in this fiscal year.
        let (from, to) = match &event {
            Some(e) if e.start_date <= fy.end_date && e.end_date >= fy.start_date => {
                (e.start_date.max(fy.start_date), e.end_date.min(fy.end_date))
            }
            Some(e) => (e.start_date, e.end_date),
            None => (first, last),
        };
        report.events.push(EventRevenue { event, from, to, amount: Money(cents) });
    }
    report.events.sort_by_key(|e| (e.from, e.event.as_ref().map(|x| x.id)));
    report.donations = donors.into_iter().map(|(name, cents)| NamedAmount { name, amount: Money(cents) }).collect();

    if report.other_income_total() > report.not_allowed {
        report.checks.push(Check {
            level: Level::Info,
            message:
                "Line 5 is for transfers or repayments that the branch approved. Keep the approval with the report."
                    .into(),
            tx_id: None,
        });
    }
    if report.events.len() > 8 {
        report.checks.push(Check {
            level: Level::Info,
            message: format!(
                "There are {} licences. The form has room for 8 (lines 6 to 13). Attach a sheet for the rest.",
                report.events.len()
            ),
            tx_id: None,
        });
    }
    if report.donations.len() > 3 {
        report.checks.push(Check {
            level: Level::Info,
            message: "The form has room for 3 donating organizations (lines 15 to 17). Attach a sheet for the rest."
                .into(),
            tx_id: None,
        });
    }
    check_events(pool, fy, &events, &mut report.checks).await?;

    if !report.transfers_net.is_zero() {
        report.checks.push(Check {
            level: Level::Warning,
            message: format!(
                "Transfers between gaming accounts add up to {}, not zero. Each transfer needs both sides, \
                 and both accounts must be in the app.",
                report.transfers_net
            ),
            tx_id: None,
        });
    }
    if report.closing() + report.transfers_net != report.accounts_total {
        report.checks.push(Check {
            level: Level::Problem,
            message: format!(
                "Line 47 ({}) does not equal the gaming account balances ({}). Check the opening balance and \
                 that every statement is imported.",
                report.closing(),
                report.accounts_total
            ),
            tx_id: None,
        });
    }

    report.outstanding = sqlx::query_as(
        "SELECT t.* FROM transactions t JOIN accounts a ON a.id = t.account_id
         WHERE a.kind = 'gaming' AND t.cleared = 0 AND t.date <= ? ORDER BY t.date",
    )
    .bind(fy.end_date)
    .fetch_all(pool)
    .await?;
    for t in &report.outstanding {
        report.checks.push(Check {
            level: Level::Info,
            message: format!(
                "{} {} \"{}\" had not cleared the bank by year end. It goes on next year's report.",
                t.date,
                t.amount(),
                t.display_name()
            ),
            tx_id: Some(t.id),
        });
    }

    report.grants = grant_schedule(pool, fy.end_date, report.accounts_total).await?;
    for g in &report.grants {
        if g.unspent.cents() > 0 && g.spend_by < fy.end_date {
            report.checks.push(Check {
                level: Level::Problem,
                message: format!(
                    "{} of the grant received {} was not spent by {}. Unspent funds must be returned \
                     unless the branch gave a written extension.",
                    g.unspent, g.received, g.spend_by
                ),
                tx_id: None,
            });
        }
    }

    report.checks.push(Check {
        level: Level::Info,
        message: format!(
            "Submit this report by {}. Two unrelated executive members sign it, one of them the treasurer. \
             Wet signatures only. Keep the receipts and bank statements for 5 years.",
            report.due_date.format("%B %-d, %Y")
        ),
        tx_id: None,
    });
    report.checks.sort_by_key(|c| c.level);
    Ok(report)
}

async fn check_disbursement(
    pool: &SqlitePool,
    fy: &FiscalYear,
    d: &Disbursement,
    checks: &mut Vec<Check>,
) -> anyhow::Result<()> {
    let what = format!("{} {} to \"{}\"", d.tx.date, d.amount, d.disbursed_to);
    let id = d.tx.id;
    let mut push = |level, message: String, tx_id: i64| checks.push(Check { level, message, tx_id: Some(tx_id) });
    if d.receipt_required && !d.has_receipt && d.reimbursements.is_empty() {
        push(Level::Warning, format!("{what} has no receipt. Every gaming disbursement needs one."), id);
    }
    if d.tx.purpose.is_empty() {
        push(
            Level::Warning,
            format!("{what} has no purpose. The report uses the line item name, which may be too general."),
            id,
        );
    }
    if !d.reimbursements.is_empty() {
        let total = d.reimbursement_total();
        if total != d.amount {
            push(
                Level::Problem,
                format!(
                    "{what} repays general-account expenses of {total}. The reimbursement summary must match the transfer exactly."
                ),
                id,
            );
        }
        for r in &d.reimbursements {
            if !fy.contains(r.date) {
                push(
                    Level::Problem,
                    format!(
                        "{what} repays an expense dated {}. A reimbursement must be in the same fiscal year as the expense.",
                        r.date
                    ),
                    id,
                );
            }
            if r.date > d.tx.date {
                push(
                    Level::Problem,
                    format!(
                        "{what} repays an expense dated {}, after the transfer. Prepaid expenses cannot be reimbursed.",
                        r.date
                    ),
                    id,
                );
            }
            let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attachments WHERE transaction_id = ?")
                .bind(r.id)
                .fetch_one(pool)
                .await?;
            if receipts == 0 {
                push(
                    Level::Warning,
                    format!(
                        "The reimbursed expense {} {} \"{}\" has no receipt.",
                        r.date,
                        r.amount(),
                        r.display_name()
                    ),
                    r.id,
                );
            }
        }
    }
    Ok(())
}

/// Totals for one licensed gaming event over its whole life, all transactions.
#[derive(Debug, Clone, Default)]
pub struct EventTotals {
    pub revenue: Money,
    pub prizes: Money,
    pub expenses: Money,
    pub transactions: i64,
}

impl EventTotals {
    pub fn net(&self) -> Money {
        self.revenue - self.prizes - self.expenses
    }
}

pub async fn event_totals(pool: &SqlitePool) -> anyhow::Result<HashMap<i64, EventTotals>> {
    let rows: Vec<(i64, Option<String>, String, i64)> = sqlx::query_as(
        "SELECT t.gaming_event_id, li.gasr_line, li.kind, SUM(al.amount_cents)
         FROM allocations al
         JOIN line_items li ON li.id = al.line_item_id
         JOIN transactions t ON t.id = al.transaction_id
         WHERE t.gaming_event_id IS NOT NULL
         GROUP BY t.gaming_event_id, li.gasr_line, li.kind",
    )
    .fetch_all(pool)
    .await?;
    let counts: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT gaming_event_id, COUNT(*) FROM transactions WHERE gaming_event_id IS NOT NULL GROUP BY gaming_event_id",
    )
    .fetch_all(pool)
    .await?;
    let mut out: HashMap<i64, EventTotals> = HashMap::new();
    for (id, code, kind, cents) in rows {
        let t = out.entry(id).or_default();
        match (kind.as_str(), code.as_deref()) {
            ("income", Some("6")) => t.revenue += Money(cents),
            ("expense", Some("prize")) => t.prizes += Money(-cents),
            ("expense", Some("event_expense")) => t.expenses += Money(-cents),
            _ => {}
        }
    }
    for (id, n) in counts {
        out.entry(id).or_default().transactions = n;
    }
    Ok(out)
}

/// An event that has ended and cost more than it raised used other gaming money,
/// such as the PAC grant, to pay for fundraising.
async fn check_events(
    pool: &SqlitePool,
    fy: &FiscalYear,
    events: &HashMap<i64, GamingEvent>,
    checks: &mut Vec<Check>,
) -> anyhow::Result<()> {
    let totals = event_totals(pool).await?;
    let mut list: Vec<&GamingEvent> = events.values().filter(|e| fy.contains(e.end_date)).collect();
    list.sort_by_key(|e| e.end_date);
    for e in list {
        let Some(t) = totals.get(&e.id) else { continue };
        if t.net().is_negative() {
            checks.push(Check {
                level: Level::Warning,
                message: format!(
                    "{} cost {} more than it raised. PAC grant money may not pay fundraising costs. \
                     Check the event's revenue, prizes, and expenses.",
                    e.label(),
                    t.net().abs()
                ),
                tx_id: None,
            });
        }
    }
    Ok(())
}

/// Grant payments with their spend-by dates. The balance at `as_of` is
/// assigned to the newest money first, because spending uses the oldest money
/// first. All gaming revenue counts as money in, so event revenue does not
/// show as unspent grant.
pub async fn grant_schedule(pool: &SqlitePool, as_of: NaiveDate, balance: Money) -> anyhow::Result<Vec<GrantRow>> {
    let inflows: Vec<(NaiveDate, Option<String>, i64)> = sqlx::query_as(
        "SELECT t.date, li.gasr_line, SUM(al.amount_cents)
         FROM allocations al
         JOIN line_items li ON li.id = al.line_item_id
         JOIN transactions t ON t.id = al.transaction_id
         WHERE li.fund = 'gaming' AND li.kind = 'income' AND t.cleared = 1 AND t.date <= ?
         GROUP BY t.id, li.gasr_line
         HAVING SUM(al.amount_cents) > 0
         ORDER BY t.date DESC, t.id DESC",
    )
    .bind(as_of)
    .fetch_all(pool)
    .await?;
    let mut left = balance.cents().max(0);
    let mut out = Vec::new();
    for (received, code, amount) in inflows {
        let unspent = left.min(amount);
        left -= unspent;
        if code.as_deref() == Some("3") {
            out.push(GrantRow {
                received,
                amount: Money(amount),
                spend_by: received + Months::new(SPEND_WITHIN_MONTHS),
                unspent: Money(unspent),
            });
        }
    }
    out.reverse();
    Ok(out)
}
