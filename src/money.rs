//! Money is integer cents everywhere. This module parses and formats it.

use std::fmt;

/// An amount in cents. Display gives "$1,234.56" or "-$1,234.56".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Money(pub i64);

impl Money {
    pub fn cents(self) -> i64 {
        self.0
    }

    pub fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Accounting style for reports: "1,234.56" or "(1,234.56)". Zero is "–".
    pub fn accounting(self) -> String {
        if self.0 == 0 {
            return "–".to_string();
        }
        let digits = group(self.0.unsigned_abs());
        if self.0 < 0 { format!("({digits})") } else { digits }
    }

    /// Plain decimal for form inputs: "1234.56" or "-1234.56".
    pub fn plain(self) -> String {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        format!("{sign}{}.{:02}", abs / 100, abs % 100)
    }

    /// The value for a form input: blank for zero.
    pub fn input(self) -> String {
        if self.0 == 0 { String::new() } else { self.plain() }
    }

    /// The absolute value, formatted with a dollar sign.
    pub fn abs(self) -> Money {
        Money(self.0.abs())
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        write!(f, "{sign}${}", group(self.0.unsigned_abs()))
    }
}

impl std::ops::Add for Money {
    type Output = Money;
    fn add(self, rhs: Money) -> Money {
        Money(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Money) -> Money {
        Money(self.0 - rhs.0)
    }
}

impl std::ops::AddAssign for Money {
    fn add_assign(&mut self, rhs: Money) {
        self.0 += rhs.0;
    }
}

impl std::ops::Neg for Money {
    type Output = Money;
    fn neg(self) -> Money {
        Money(-self.0)
    }
}

impl std::iter::Sum for Money {
    fn sum<I: Iterator<Item = Money>>(iter: I) -> Money {
        Money(iter.map(|m| m.0).sum())
    }
}

/// "123456" cents -> "1,234.56"
fn group(cents: u64) -> String {
    let dollars = (cents / 100).to_string();
    let mut out = String::with_capacity(dollars.len() + dollars.len() / 3 + 3);
    for (i, ch) in dollars.chars().enumerate() {
        if i > 0 && (dollars.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    format!("{out}.{:02}", cents % 100)
}

/// Parses amounts as banks and people write them:
/// "1,234.56", "-12", "$12.50", "(45.00)", "12.5-", "CR 10.00". Empty gives None.
pub fn parse(input: &str) -> Option<i64> {
    let mut s: String = input.trim().chars().filter(|c| !matches!(c, '$' | ',' | ' ' | '\u{a0}')).collect();
    if s.is_empty() {
        return None;
    }
    let mut negative = false;
    if s.starts_with('(') && s.ends_with(')') {
        negative = true;
        s = s[1..s.len() - 1].to_string();
    }
    if let Some(rest) = s.strip_prefix('-') {
        negative = !negative;
        s = rest.to_string();
    } else if let Some(rest) = s.strip_prefix('+') {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_suffix('-') {
        negative = !negative;
        s = rest.to_string();
    }
    let upper = s.to_ascii_uppercase();
    if let Some(rest) = upper.strip_prefix("DR").or_else(|| upper.strip_suffix("DR")) {
        negative = !negative;
        s = rest.to_string();
    } else if let Some(rest) = upper.strip_prefix("CR").or_else(|| upper.strip_suffix("CR")) {
        s = rest.to_string();
    }
    let (whole, frac) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s.as_str(), ""),
    };
    if whole.is_empty() && frac.is_empty() {
        return None;
    }
    if !whole.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if frac.len() > 2 {
        // Bank files sometimes carry 3 or 4 decimals ("12.3400"). Allow only zeros past cents.
        if !frac[2..].chars().all(|c| c == '0') {
            return None;
        }
    }
    let whole: i64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let frac_cents: i64 = match frac.len() {
        0 => 0,
        1 => frac.parse::<i64>().ok()? * 10,
        _ => frac[..2].parse().ok()?,
    };
    let cents = whole.checked_mul(100)?.checked_add(frac_cents)?;
    Some(if negative { -cents } else { cents })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(Money(123456).to_string(), "$1,234.56");
        assert_eq!(Money(-5).to_string(), "-$0.05");
        assert_eq!(Money(100_000_000).to_string(), "$1,000,000.00");
        assert_eq!(Money(-123456).accounting(), "(1,234.56)");
        assert_eq!(Money(0).accounting(), "–");
        assert_eq!(Money(-1050).plain(), "-10.50");
    }

    #[test]
    fn parses() {
        assert_eq!(parse("1,234.56"), Some(123456));
        assert_eq!(parse("$12.5"), Some(1250));
        assert_eq!(parse("-12"), Some(-1200));
        assert_eq!(parse("(45.00)"), Some(-4500));
        assert_eq!(parse("12.50-"), Some(-1250));
        assert_eq!(parse("12.3400"), Some(1234));
        assert_eq!(parse(".5"), Some(50));
        assert_eq!(parse(""), None);
        assert_eq!(parse("abc"), None);
        assert_eq!(parse("1.234"), None);
    }
}
