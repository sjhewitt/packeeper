//! The Gaming Account Summary Report (GASR) for the BC Community Gaming Grant,
//! plus checks against the PAC grant conditions.
//!
//! The GASR is on a bank basis: only transactions the bank has cleared count.
//! A cheque not cashed by year end goes on the next year's report.

use std::collections::HashMap;

use chrono::{Days, Months, NaiveDate};
use sqlx::SqlitePool;

use crate::db;
use crate::models::{Account, FiscalYear, LineItem, Transaction};
use crate::money::Money;

/// PACs must spend each grant within 24 months of receipt.
pub const SPEND_WITHIN_MONTHS: u32 = 24;
/// The GASR is due within 90 days after the fiscal year end.
pub const DUE_WITHIN_DAYS: u64 = 90;

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

#[derive(Debug, Clone)]
pub struct Deposit {
    pub tx: Transaction,
    pub amount: Money,
    pub gasr_line: &'static str,
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
    /// Line 3
    pub grant: Money,
    /// Line 5. Should be zero for a PAC.
    pub other_income: Money,
    /// Line 14
    pub interest_gst: Money,
    /// Lines 18 and 27
    pub total_revenue: Money,
    /// Line 28
    pub available: Money,
    /// Lines 30 to 45
    pub disbursements: Vec<Disbursement>,
    /// Line 46
    pub total_disbursed: Money,
    /// Line 47
    pub closing: Money,
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

pub async fn build(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<GamingReport> {
    let accounts: Vec<Account> = db::accounts(pool).await?.into_iter().filter(|a| a.is_gaming()).collect();
    let items: HashMap<i64, LineItem> = db::line_items(pool).await?.into_iter().map(|li| (li.id, li)).collect();
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
        grant: Money(0),
        other_income: Money(0),
        interest_gst: Money(0),
        total_revenue: Money(0),
        available: Money(0),
        disbursements: Vec::new(),
        total_disbursed: Money(0),
        closing: Money(0),
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

    for tx in txs {
        let allocs = crate::ledger::allocations(pool, tx.id).await?;
        let mut grant = 0;
        let mut interest = 0;
        let mut other_in = 0;
        let mut spent = 0;
        let mut receipt_required = false;
        let mut names = Vec::new();
        for al in &allocs {
            let Some(li) = items.get(&al.line_item_id) else { continue };
            names.push(li.name.clone());
            match li.kind.as_str() {
                "income" => match li.gasr_line.as_deref() {
                    Some("3") => grant += al.amount_cents,
                    Some("14") => interest += al.amount_cents,
                    _ => other_in += al.amount_cents,
                },
                "expense" => {
                    spent -= al.amount_cents;
                    receipt_required |= li.receipt_required;
                }
                _ => report.transfers_net += Money(al.amount_cents),
            }
        }
        let remainder = tx.amount_cents - allocs.iter().map(|a| a.amount_cents).sum::<i64>();
        if remainder != 0 {
            report.checks.push(Check {
                level: Level::Warning,
                message: format!(
                    "{} {} \"{}\" has {} with no line item.",
                    tx.date,
                    tx.amount(),
                    tx.display_name(),
                    Money(remainder).abs()
                ),
                tx_id: Some(tx.id),
            });
            if remainder > 0 {
                other_in += remainder;
            } else {
                spent -= remainder;
                receipt_required = true;
            }
        }
        let line_names = names.join("; ");

        if grant + interest + other_in != 0 {
            report.grant += Money(grant);
            report.interest_gst += Money(interest);
            report.other_income += Money(other_in);
            let gasr_line = if other_in != 0 {
                "5"
            } else if grant != 0 {
                "3"
            } else {
                "14"
            };
            if other_in > 0 {
                report.checks.push(Check {
                    level: Level::Problem,
                    message: format!(
                        "{} deposit of {} (\"{}\") is not the gaming grant, interest, or a GST rebate. \
                         Non-gaming funds must not be deposited into the gaming account.",
                        tx.date,
                        Money(other_in),
                        tx.display_name()
                    ),
                    tx_id: Some(tx.id),
                });
            }
            report.deposits.push(Deposit { tx: tx.clone(), amount: Money(grant + interest + other_in), gasr_line });
        }

        if spent != 0 {
            let reimbursements: Vec<Transaction> =
                sqlx::query_as("SELECT * FROM transactions WHERE reimbursed_by_id = ? ORDER BY date, id")
                    .bind(tx.id)
                    .fetch_all(pool)
                    .await?;
            let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attachments WHERE transaction_id = ?")
                .bind(tx.id)
                .fetch_one(pool)
                .await?;
            let d = Disbursement {
                amount: Money(spent),
                disbursed_to: tx.display_name().to_string(),
                purpose: if tx.purpose.is_empty() { line_names.clone() } else { tx.purpose.clone() },
                reference: tx.reference.clone(),
                line_names,
                has_receipt: receipts > 0,
                receipt_required,
                reimbursements,
                tx,
            };
            check_disbursement(pool, fy, &d, &mut report.checks).await?;
            report.total_disbursed += d.amount;
            report.disbursements.push(d);
        }
    }

    report.total_revenue = report.grant + report.other_income + report.interest_gst;
    report.available = report.opening + report.total_revenue;
    report.closing = report.available - report.total_disbursed;

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
    if report.closing + report.transfers_net != report.accounts_total {
        report.checks.push(Check {
            level: Level::Problem,
            message: format!(
                "Line 47 ({}) does not equal the gaming account balances ({}). Check the opening balance and \
                 that every statement is imported.",
                report.closing, report.accounts_total
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

/// Grant payments with their spend-by dates. The balance at `as_of` is
/// assigned to the newest grants first, because spending uses the oldest money first.
pub async fn grant_schedule(pool: &SqlitePool, as_of: NaiveDate, balance: Money) -> anyhow::Result<Vec<GrantRow>> {
    let grants: Vec<(NaiveDate, i64)> = sqlx::query_as(
        "SELECT t.date, SUM(al.amount_cents)
         FROM allocations al
         JOIN line_items li ON li.id = al.line_item_id
         JOIN transactions t ON t.id = al.transaction_id
         WHERE li.gasr_line = '3' AND t.cleared = 1 AND t.date <= ? AND al.amount_cents > 0
         GROUP BY t.id ORDER BY t.date DESC",
    )
    .bind(as_of)
    .fetch_all(pool)
    .await?;
    let mut left = balance.cents().max(0);
    let mut out = Vec::new();
    for (received, amount) in grants {
        let unspent = left.min(amount);
        left -= unspent;
        out.push(GrantRow {
            received,
            amount: Money(amount),
            spend_by: received + Months::new(SPEND_WITHIN_MONTHS),
            unspent: Money(unspent),
        });
    }
    out.reverse();
    Ok(out)
}
