//! What a card payment costs the PAC, and the platform's share of it.
//!
//! A PAC with its own Stripe account (Connect Standard) pays Stripe's fees in
//! its own Stripe account, plus an optional platform fee. A PAC on the
//! platform's Stripe (Connect Express) pays one application fee per payment.
//! That fee must cover everything the platform pays Stripe for the payment,
//! and the platform's surcharge:
//!
//! - the card fee (2.9% + C$0.30 for Canadian cards),
//! - the payout fee (0.25% of the money paid out to the PAC),
//! - the account fee (C$2 for each month in which the PAC gets a payout),
//!   added to the first card payment of each month,
//! - the surcharge, a percentage and a fixed amount that the operator sets.
//!
//! Every amount is rounded up, so the fee never falls short. All settings come
//! from the environment, so the operator can follow Stripe's price changes.

/// Basis points: 1% is 100.
type Bp = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeeConfig {
    /// Stripe's card fee: a percentage and a fixed amount.
    pub card_bp: Bp,
    pub card_cents: i64,
    /// Stripe's fee on money paid out to an Express account.
    pub payout_bp: Bp,
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
    /// PAC_STRIPE_PAYOUT_BP, PAC_STRIPE_MONTHLY_CENTS, PAC_SURCHARGE_BP,
    /// PAC_SURCHARGE_CENTS, PAC_STANDARD_FEE_BP, and PAC_STANDARD_FEE_CENTS.
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
            monthly_cents: read("PAC_STRIPE_MONTHLY_CENTS", d.monthly_cents)?,
            surcharge_bp: read("PAC_SURCHARGE_BP", d.surcharge_bp)?,
            surcharge_cents: read("PAC_SURCHARGE_CENTS", d.surcharge_cents)?,
            standard_bp: read("PAC_STANDARD_FEE_BP", d.standard_bp)?,
            standard_cents: read("PAC_STANDARD_FEE_CENTS", d.standard_cents)?,
        })
    }

    /// The application fee on an Express payment of `amount` cents.
    /// `first_this_month` adds the month's account fee.
    pub fn express_fee(&self, amount: i64, first_this_month: bool) -> i64 {
        // Each part is rounded up on its own, as Stripe works out each fee on
        // its own: one rounding of the sum could fall a cent or two short.
        let monthly = if first_this_month { self.monthly_cents } else { 0 };
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
    /// "3.15% + $0.30 per payment, and $2.00 in each month with card payments".
    pub fn express_terms(&self) -> String {
        format!(
            "{} + {} per payment, and {} in each month with card payments",
            percent_text(self.card_bp + self.payout_bp + self.surcharge_bp),
            crate::money::Money(self.card_cents + self.surcharge_cents),
            crate::money::Money(self.monthly_cents)
        )
    }

    /// The platform fee on payments to a PAC's own Stripe, in words, or None.
    pub fn standard_terms(&self) -> Option<String> {
        (self.standard_bp > 0 || self.standard_cents > 0).then(|| {
            format!("{} + {} per payment", percent_text(self.standard_bp), crate::money::Money(self.standard_cents))
        })
    }

    /// Card payments need an amount that leaves something for the PAC after
    /// the fee, and Stripe's own minimum charge of C$0.50.
    pub fn smallest_card_amount(&self, first_this_month: bool) -> i64 {
        let mut amount = 50;
        while self.express_fee(amount, first_this_month) >= amount {
            amount += 1;
        }
        amount
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

    #[test]
    fn express_fee_covers_stripe_and_the_surcharge() {
        let fees = FeeConfig { surcharge_bp: 100, surcharge_cents: 25, ..FeeConfig::default() };
        // $40.00: card 2.9% = 116, payout 0.25% = 10, surcharge 1% = 40, plus 30 and 25.
        assert_eq!(fees.express_fee(4000, false), 116 + 10 + 40 + 30 + 25);
        assert_eq!(fees.express_fee(4000, true), 116 + 10 + 40 + 30 + 25 + 200);
        // What Stripe takes from the platform for the same payment never exceeds the fee.
        for amount in [50, 99, 101, 1234, 4000, 99_999] {
            let stripe_cost = percent_up(amount, 290) + 30 + percent_up(amount, 25);
            assert!(fees.express_fee(amount, false) >= stripe_cost + percent_up(amount, 100) + 25);
        }
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
        assert_eq!(fees.express_terms(), "3.15% + $0.30 per payment, and $2.00 in each month with card payments");
        assert_eq!(fees.standard_terms(), None);
        let with_fee = FeeConfig { standard_bp: 50, ..FeeConfig::default() };
        assert_eq!(with_fee.standard_terms().unwrap(), "0.5% + $0.00 per payment");
        assert_eq!(fees.standard_fee(4000), 0);
        assert_eq!(with_fee.standard_fee(4000), 20);
        let min = fees.smallest_card_amount(true);
        assert!(fees.express_fee(min, true) < min && fees.express_fee(min - 1, true) >= min - 1);
        assert_eq!(fees.smallest_card_amount(false), 50);
    }
}
