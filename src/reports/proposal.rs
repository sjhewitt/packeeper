//! Budget proposal for the next fiscal year, based on the year before it.

use chrono::NaiveDate;
use sqlx::SqlitePool;

use super::{actuals, budgets, fund_balance};
use crate::db::{self, Settings};
use crate::models::{FiscalYear, Fund};
use crate::money::Money;

#[derive(Debug, Clone)]
pub struct ProposalLine {
    pub line_item_id: i64,
    pub name: String,
    pub prior_budget: Money,
    pub prior_actual: Money,
    pub proposed: Money,
    pub notes: String,
}

impl ProposalLine {
    pub fn change(&self) -> Money {
        self.proposed - self.prior_budget
    }
}

#[derive(Debug, Clone)]
pub struct ProposalFund {
    pub fund: Fund,
    pub income: Vec<ProposalLine>,
    pub expense: Vec<ProposalLine>,
    /// The fund balance today (or at the base year end, if it has ended).
    pub balance_now: Money,
}

impl ProposalFund {
    pub fn title(&self) -> &'static str {
        self.fund.title()
    }

    fn sum(rows: &[ProposalLine], f: impl Fn(&ProposalLine) -> Money) -> Money {
        rows.iter().map(f).sum()
    }

    pub fn income_prior_budget(&self) -> Money {
        Self::sum(&self.income, |l| l.prior_budget)
    }
    pub fn income_prior_actual(&self) -> Money {
        Self::sum(&self.income, |l| l.prior_actual)
    }
    pub fn income_proposed(&self) -> Money {
        Self::sum(&self.income, |l| l.proposed)
    }
    pub fn expense_prior_budget(&self) -> Money {
        Self::sum(&self.expense, |l| l.prior_budget)
    }
    pub fn expense_prior_actual(&self) -> Money {
        Self::sum(&self.expense, |l| l.prior_actual)
    }
    pub fn expense_proposed(&self) -> Money {
        Self::sum(&self.expense, |l| l.proposed)
    }
    pub fn net_prior_budget(&self) -> Money {
        self.income_prior_budget() - self.expense_prior_budget()
    }
    pub fn net_prior_actual(&self) -> Money {
        self.income_prior_actual() - self.expense_prior_actual()
    }
    pub fn net_proposed(&self) -> Money {
        self.income_proposed() - self.expense_proposed()
    }
}

#[derive(Debug, Clone)]
pub struct Proposal {
    pub base: FiscalYear,
    pub target: FiscalYear,
    /// The last date the base-year actuals cover.
    pub actuals_to: NaiveDate,
    pub base_complete: bool,
    pub funds: Vec<ProposalFund>,
    pub expected_grant: Money,
    pub enrolment: i64,
    pub gaming_rate: Money,
}

pub async fn build(pool: &SqlitePool, base: &FiscalYear, target: &FiscalYear) -> anyhow::Result<Proposal> {
    let today = chrono::Local::now().date_naive();
    let actuals_to = base.end_date.min(today);
    let base_complete = base.end_date < today;
    let settings = Settings::load(pool).await?;
    let items = db::line_items(pool).await?;
    let accounts = db::accounts(pool).await?;
    let prior_budget = budgets(pool, base.id).await?;
    let proposed = budgets(pool, target.id).await?;
    let actual = actuals(pool, base.start_date, actuals_to, false).await?;

    let mut funds = Vec::new();
    for fund in Fund::ALL {
        let mut income = Vec::new();
        let mut expense = Vec::new();
        for li in items.iter().filter(|li| li.fund() == fund && li.kind != "transfer") {
            let sign = if li.kind == "income" { 1 } else { -1 };
            let line = ProposalLine {
                line_item_id: li.id,
                name: li.name.clone(),
                prior_budget: Money(prior_budget.get(&li.id).map(|b| b.0).unwrap_or(0)),
                prior_actual: Money(sign * actual.line(li.id)),
                proposed: Money(proposed.get(&li.id).map(|b| b.0).unwrap_or(0)),
                notes: proposed.get(&li.id).map(|b| b.1.clone()).unwrap_or_default(),
            };
            let keep =
                li.active || !line.prior_budget.is_zero() || !line.prior_actual.is_zero() || !line.proposed.is_zero();
            if !keep {
                continue;
            }
            if li.kind == "income" {
                income.push(line);
            } else {
                expense.push(line);
            }
        }
        funds.push(ProposalFund {
            fund,
            income,
            expense,
            balance_now: fund_balance(pool, &accounts, fund, actuals_to, false).await?,
        });
    }
    Ok(Proposal {
        base: base.clone(),
        target: target.clone(),
        actuals_to,
        base_complete,
        funds,
        expected_grant: Money(settings.expected_gaming_grant_cents()),
        enrolment: settings.enrolment,
        gaming_rate: Money(settings.gaming_rate_cents),
    })
}

/// How to fill the proposed amounts from the base year.
#[derive(Debug, Clone, Copy)]
pub enum Fill {
    Budget,
    Actual,
    /// Actual, rounded up to the next $50.
    ActualRounded,
}

/// Sets every proposed amount from the base year. The gaming grant line is set
/// from enrolment x rate when the enrolment is known. Notes are kept.
pub async fn fill(pool: &SqlitePool, base: &FiscalYear, target: &FiscalYear, how: Fill) -> anyhow::Result<()> {
    let p = build(pool, base, target).await?;
    let settings = Settings::load(pool).await?;
    let items = db::line_items(pool).await?;
    let mut tx = pool.begin().await?;
    for f in &p.funds {
        for line in f.income.iter().chain(&f.expense) {
            let mut amount = match how {
                Fill::Budget => line.prior_budget.cents(),
                Fill::Actual => line.prior_actual.cents().max(0),
                Fill::ActualRounded => {
                    let a = line.prior_actual.cents().max(0);
                    (a + 4999) / 5000 * 5000
                }
            };
            let is_grant = items.iter().any(|li| li.id == line.line_item_id && li.gasr_line.as_deref() == Some("3"));
            if is_grant && settings.enrolment > 0 {
                amount = settings.expected_gaming_grant_cents();
            }
            sqlx::query(
                "INSERT INTO budget_amounts (fiscal_year_id, line_item_id, amount_cents) VALUES (?, ?, ?)
                 ON CONFLICT (fiscal_year_id, line_item_id) DO UPDATE SET amount_cents = excluded.amount_cents",
            )
            .bind(target.id)
            .bind(line.line_item_id)
            .bind(amount)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}
