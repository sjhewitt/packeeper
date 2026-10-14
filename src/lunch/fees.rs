//! What a card payment costs the PAC, and the platform's share of it.
//!
//! A PAC with its own Stripe account (Connect Standard) pays Stripe's fees in
//! its own Stripe account, plus an optional platform fee. A PAC on the
//! platform's Stripe (Connect Express) pays one application fee per payment.
//! That fee must cover everything the platform pays Stripe for the payment,
//! and the platform's surcharge:
//!
//! - the card fee (2.9% + C$0.30 for Canadian cards),
//! - the payout fee: 0.25% of the money paid out to the PAC, and C$0.25 for
//!   each payout,
//! - the account fee (C$2 for each month in which the PAC gets a payout),
//! - the surcharge, a percentage and a fixed amount that the operator sets.
//!
//! The PAC gets one payout a week, on the payout day, so a month has at most
//! one payout for each payout day in it. The first card payment of each month
//! carries the month's charge: the account fee, and C$0.25 for each payout day
//! in that month.
//!
//! Every amount is rounded up, so the fee never falls short. All settings come
//! from the environment, so the operator can follow Stripe's price changes.

use chrono::{Datelike, NaiveDate, Weekday};

/// Basis points: 1% is 100.
type Bp = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeConfig {
    /// Stripe's card fee: a percentage and a fixed amount.
    pub card_bp: Bp,
    pub card_cents: i64,
    /// Stripe's fee on money paid out to an Express account: a percentage,
    /// and a fixed amount for each payout.
    pub payout_bp: Bp,
    pub payout_cents: i64,
    /// The weekday of the weekly payout to each Express account.
    pub payout_day: Weekday,
    /// Stripe's monthly fee for an Express account that gets a payout.
    pub monthly_cents: i64,
    /// The platform's surcharge on Express payments.
    pub surcharge_bp: Bp,
    pub surcharge_cents: i64,
    /// The platform's fee on payments to a PAC's own Stripe account.
    pub standard_bp: Bp,
    pub standard_cents: i64,
}

impl Default for FeeConfig {
    /// Stripe Canada's published prices, no surcharge, and no fee on PACs with
    /// their own Stripe.
    fn default() -> FeeConfig {
        FeeConfig {
            card_bp: 290,
            card_cents: 30,
            payout_bp: 25,
            payout_cents: 25,
            payout_day: Weekday::Mon,
            monthly_cents: 200,
            surcharge_bp: 0,
            surcharge_cents: 0,
            standard_bp: 0,
            standard_cents: 0,
        }
    }
}

impl FeeConfig {
    /// The defaults, with any of PAC_STRIPE_CARD_BP, PAC_STRIPE_CARD_CENTS,
    /// PAC_STRIPE_PAYOUT_BP, PAC_STRIPE_PAYOUT_CENTS, PAC_STRIPE_PAYOUT_DAY,
    /// PAC_STRIPE_MONTHLY_CENTS, PAC_SURCHARGE_BP, PAC_SURCHARGE_CENTS,
    /// PAC_STANDARD_FEE_BP, and PAC_STANDARD_FEE_CENTS.
    pub fn from_env() -> anyhow::Result<FeeConfig> {
        let d = FeeConfig::default();
        let read = |name: &str, default: i64| -> anyhow::Result<i64> {
            match std::env::var(name) {
                Ok(v) => {
                    let n: i64 = v.trim().parse().map_err(|_| anyhow::anyhow!("{name} must be a whole number"))?;
                    anyhow::ensure!(n >= 0, "{name} must not be negative");
                    Ok(n)
                }
                Err(_) => Ok(default),
            }
        };
        Ok(FeeConfig {
            card_bp: read("PAC_STRIPE_CARD_BP", d.card_bp)?,
            card_cents: read("PAC_STRIPE_CARD_CENTS", d.card_cents)?,
            payout_bp: read("PAC_STRIPE_PAYOUT_BP", d.payout_bp)?,
            payout_cents: read("PAC_STRIPE_PAYOUT_CENTS", d.payout_cents)?,
            payout_day: match std::env::var("PAC_STRIPE_PAYOUT_DAY") {
                Ok(v) => weekday(&v)
                    .ok_or_else(|| anyhow::anyhow!("PAC_STRIPE_PAYOUT_DAY must be a weekday, such as monday"))?,
                Err(_) => d.payout_day,
            },
            monthly_cents: read("PAC_STRIPE_MONTHLY_CENTS", d.monthly_cents)?,
            surcharge_bp: read("PAC_SURCHARGE_BP", d.surcharge_bp)?,
            surcharge_cents: read("PAC_SURCHARGE_CENTS", d.surcharge_cents)?,
            standard_bp: read("PAC_STANDARD_FEE_BP", d.standard_bp)?,
            standard_cents: read("PAC_STANDARD_FEE_CENTS", d.standard_cents)?,
        })
    }

    /// The month's charge for an Express account: the account fee, and the
    /// fixed fee of each payout the month can have.
    pub fn month_charge(&self, day: NaiveDate) -> i64 {
        self.monthly_cents + self.payout_cents * payout_days(day, self.payout_day)
    }

    /// The application fee on an Express payment of `amount` cents. `month`
    /// is a day of the month when this payment is the month's first: it then
    /// carries the month's charge.
    pub fn express_fee(&self, amount: i64, month: Option<NaiveDate>) -> i64 {
        // Each part is rounded up on its own, as Stripe works out each fee on
        // its own: one rounding of the sum could fall a cent or two short.
        let monthly = month.map_or(0, |day| self.month_charge(day));
        percent_up(amount, self.card_bp)
            + percent_up(amount, self.payout_bp)
            + percent_up(amount, self.surcharge_bp)
            + self.card_cents
            + self.surcharge_cents
            + monthly
    }

    /// The platform fee on a payment to a PAC's own Stripe account.
    pub fn standard_fee(&self, amount: i64) -> i64 {
        percent_up(amount, self.standard_bp) + self.standard_cents
    }

    /// The fee of an Express payment, said in words for the treasurer, such as
    /// "3.15% + $0.30 per payment, and $2.00 plus $0.25 for each Monday in
    /// each month with card payments".
    pub fn express_terms(&self) -> String {
        format!(
            "{} + {} per payment, and {} plus {} for each {} in each month with card payments",
            percent_text(self.card_bp + self.payout_bp + self.surcharge_bp),
            crate::money::Money(self.card_cents + self.surcharge_cents),
            crate::money::Money(self.monthly_cents),
            crate::money::Money(self.payout_cents),
            day_name(self.payout_day)
        )
    }

    /// The payout day as Stripe names it, such as "monday".
    pub fn payout_day_key(&self) -> String {
        day_name(self.payout_day).to_lowercase()
    }

    /// The payout day in words, such as "Monday".
    pub fn payout_day_name(&self) -> &'static str {
        day_name(self.payout_day)
    }

    /// The platform fee on payments to a PAC's own Stripe, in words, or None.
    pub fn standard_terms(&self) -> Option<String> {
        (self.standard_bp > 0 || self.standard_cents > 0).then(|| {
            format!("{} + {} per payment", percent_text(self.standard_bp), crate::money::Money(self.standard_cents))
        })
    }

    /// Card payments need an amount that leaves something for the PAC after
    /// the fee, and Stripe's own minimum charge of C$0.50.
    pub fn smallest_card_amount(&self, month: Option<NaiveDate>) -> i64 {
        let mut amount = 50;
        while self.express_fee(amount, month) >= amount {
            amount += 1;
        }
        amount
    }
}

/// How many times `weekday` falls in the month of `day`.
fn payout_days(day: NaiveDate, weekday: Weekday) -> i64 {
    let first = day.with_day(1).unwrap_or(day);
    first.iter_days().take_while(|d| d.month() == first.month()).filter(|d| d.weekday() == weekday).count() as i64
}

fn weekday(name: &str) -> Option<Weekday> {
    name.trim().parse().ok().filter(|d| !matches!(d, Weekday::Sat | Weekday::Sun))
}

fn day_name(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// `amount` times `bp` basis points, rounded up to a whole cent.
fn percent_up(amount: i64, bp: Bp) -> i64 {
    (amount * bp + 9_999) / 10_000
}

/// "3.15%" for 315 basis points.
fn percent_text(bp: Bp) -> String {
    let text = format!("{}.{:02}", bp / 100, bp % 100);
    format!("{}%", text.trim_end_matches('0').trim_end_matches('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn express_fee_covers_stripe_and_the_surcharge() {
        let fees = FeeConfig { surcharge_bp: 100, surcharge_cents: 25, ..FeeConfig::default() };
        // $40.00: card 2.9% = 116, payout 0.25% = 10, surcharge 1% = 40, plus 30 and 25.
        assert_eq!(fees.express_fee(4000, None), 116 + 10 + 40 + 30 + 25);
        // November 2026 has 5 Mondays: the account fee and 5 payout fees.
        assert_eq!(fees.express_fee(4000, Some(day("2026-11-17"))), 116 + 10 + 40 + 30 + 25 + 200 + 5 * 25);
        // What Stripe takes from the platform for the same payment never exceeds the fee.
        for amount in [50, 99, 101, 1234, 4000, 99_999] {
            let stripe_cost = percent_up(amount, 290) + 30 + percent_up(amount, 25);
            assert!(fees.express_fee(amount, None) >= stripe_cost + percent_up(amount, 100) + 25);
        }
    }

    #[test]
    fn the_month_charge_covers_every_payout_day() {
        let fees = FeeConfig::default();
        assert_eq!(payout_days(day("2026-11-01"), Weekday::Mon), 5);
        assert_eq!(payout_days(day("2026-10-31"), Weekday::Mon), 4);
        assert_eq!(payout_days(day("2027-02-10"), Weekday::Mon), 4);
        assert_eq!(fees.month_charge(day("2026-10-13")), 200 + 4 * 25);
        let friday = FeeConfig { payout_day: Weekday::Fri, ..FeeConfig::default() };
        assert_eq!(friday.month_charge(day("2026-10-13")), 200 + 5 * 25, "October 2026 has 5 Fridays");
        assert_eq!(friday.payout_day_key(), "friday");
        assert_eq!(weekday(" Thursday "), Some(Weekday::Thu));
        assert_eq!(weekday("sunday"), None, "Stripe pays out on business days");
    }

    #[test]
    fn rounding_is_up() {
        assert_eq!(percent_up(1, 290), 1);
        assert_eq!(percent_up(1000, 290), 29);
        assert_eq!(percent_up(1001, 290), 30);
        assert_eq!(percent_up(0, 290), 0);
    }

    #[test]
    fn terms_and_minimum() {
        let fees = FeeConfig::default();
        assert_eq!(
            fees.express_terms(),
            "3.15% + $0.30 per payment, and $2.00 plus $0.25 for each Monday in each month with card payments"
        );
        assert_eq!(fees.standard_terms(), None);
        let with_fee = FeeConfig { standard_bp: 50, ..FeeConfig::default() };
        assert_eq!(with_fee.standard_terms().unwrap(), "0.5% + $0.00 per payment");
        assert_eq!(fees.standard_fee(4000), 0);
        assert_eq!(with_fee.standard_fee(4000), 20);
        let month = Some(day("2026-10-13"));
        let min = fees.smallest_card_amount(month);
        assert!(fees.express_fee(min, month) < min && fees.express_fee(min - 1, month) >= min - 1);
        assert_eq!(fees.smallest_card_amount(None), 50);
    }
}
