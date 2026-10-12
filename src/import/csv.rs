//! CSV bank exports. Column layouts differ between banks and change when a bank
//! changes platform, so the layout is a mapping that the treasurer can adjust.
//! The mapping is saved on the account and used again for the next import.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::BankRow;
use crate::money;

/// Date formats seen in Canadian bank exports, in order of preference.
pub const DATE_FORMATS: &[(&str, &str)] = &[
    ("%Y-%m-%d", "2026-09-30"),
    ("%m/%d/%Y", "09/30/2026 (month first)"),
    ("%d/%m/%Y", "30/09/2026 (day first)"),
    ("%Y/%m/%d", "2026/09/30"),
    ("%d-%b-%Y", "30-Sep-2026"),
    ("%d %b %Y", "30 Sep 2026"),
    ("%b %d, %Y", "Sep 30, 2026"),
    ("%b %d %Y", "Sep 30 2026"),
    ("%m/%d/%y", "09/30/26 (month first)"),
    ("%d/%m/%y", "30/09/26 (day first)"),
    ("%Y%m%d", "20260930"),
    ("%d-%m-%Y", "30-09-2026"),
    ("%m-%d-%Y", "09-30-2026"),
];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CsvMapping {
    /// Lines before the header, such as an account number preamble.
    #[serde(default)]
    pub skip_rows: usize,
    pub has_header: bool,
    pub date_col: Option<usize>,
    pub date_format: String,
    pub description_cols: Vec<usize>,
    pub amount_col: Option<usize>,
    pub debit_col: Option<usize>,
    pub credit_col: Option<usize>,
    pub balance_col: Option<usize>,
    pub reference_col: Option<usize>,
    /// Flip the sign of a single amount column, for banks that show withdrawals as positive.
    pub invert_amount: bool,
}

impl CsvMapping {
    pub fn has_description(&self, col: &usize) -> bool {
        self.description_cols.contains(col)
    }

    pub fn split_amounts(&self) -> bool {
        self.amount_col.is_none() && (self.debit_col.is_some() || self.credit_col.is_some())
    }
}

/// A CSV file read into memory as text cells.
#[derive(Debug, Clone)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
}

impl Table {
    pub fn width(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }

    fn body(&self, skip: usize) -> &[Vec<String>] {
        &self.rows[skip.min(self.rows.len())..]
    }

    /// Column names: the header cells, or "Column N" when there is no header.
    pub fn column_names(&self, m: &CsvMapping) -> Vec<String> {
        let header = if m.has_header { self.body(m.skip_rows).first() } else { None };
        (0..self.width())
            .map(|i| {
                let name = header.and_then(|r| r.get(i)).map(|s| s.trim().to_string());
                match name {
                    Some(n) if !n.is_empty() => n,
                    _ => format!("Column {}", i + 1),
                }
            })
            .collect()
    }

    pub fn data_rows(&self, m: &CsvMapping) -> &[Vec<String>] {
        let body = self.body(m.skip_rows);
        if m.has_header && !body.is_empty() { &body[1..] } else { body }
    }

    /// Leading rows that are narrower than most rows, such as an account preamble.
    fn preamble_rows(&self) -> usize {
        let mut counts = std::collections::HashMap::new();
        for r in &self.rows {
            *counts.entry(r.len()).or_insert(0) += 1;
        }
        let common = counts.into_iter().max_by_key(|(w, c)| (*c, *w)).map(|(w, _)| w).unwrap_or(0);
        self.rows.iter().take_while(|r| r.len() < common).count()
    }
}

/// Decodes bytes (UTF-8 with or without BOM, else Windows-1252) and reads all rows.
pub fn read(bytes: &[u8]) -> anyhow::Result<Table> {
    let text = decode(bytes);
    let delimiter = sniff_delimiter(&text);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        let row: Vec<String> = record.iter().map(|s| s.to_string()).collect();
        if row.iter().all(|c| c.is_empty()) {
            continue;
        }
        rows.push(row);
    }
    if rows.is_empty() {
        anyhow::bail!("the file has no rows");
    }
    Ok(Table { rows })
}

pub fn decode(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
    }
}

fn sniff_delimiter(text: &str) -> u8 {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    [b',', b';', b'\t', b'|']
        .into_iter()
        .max_by_key(|d| first.matches(*d as char).count())
        .filter(|d| first.contains(*d as char))
        .unwrap_or(b',')
}

pub fn parse_date(value: &str, format: &str) -> Option<NaiveDate> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Ok(d) = NaiveDate::parse_from_str(v, format) {
        return Some(d);
    }
    // Tolerate a time after the date ("2026-09-30 00:00:00", "2026-09-30T00:00").
    if !format.contains(' ') {
        let head = v.split([' ', 'T']).next().unwrap_or(v);
        if head != v {
            return NaiveDate::parse_from_str(head, format).ok();
        }
    }
    None
}

/// The first format that parses every non-empty value, if any.
pub fn detect_date_format<'a>(values: impl Iterator<Item = &'a str> + Clone) -> Option<&'static str> {
    DATE_FORMATS.iter().map(|(f, _)| *f).find(|f| {
        let mut any = false;
        for v in values.clone() {
            if v.trim().is_empty() {
                continue;
            }
            any = true;
            if parse_date(v, f).is_none() {
                return false;
            }
        }
        any
    })
}

fn header_matches(name: &str, options: &[&str]) -> bool {
    let n = name.trim().to_ascii_lowercase();
    options.iter().any(|o| n == *o)
}

/// Guesses the mapping from header names and cell contents.
pub fn detect(table: &Table) -> CsvMapping {
    let skip_rows = table.preamble_rows();
    let first = &table.body(skip_rows)[0];
    let first_has_date = first.iter().any(|c| DATE_FORMATS.iter().any(|(f, _)| parse_date(c, f).is_some()));
    let first_has_amount = first.iter().any(|c| money::parse(c).is_some());
    let has_header = !first_has_date && !first_has_amount && first.iter().any(|c| c.chars().any(char::is_alphabetic));

    let mut m = CsvMapping { skip_rows, has_header, ..Default::default() };
    let names = table.column_names(&m);
    let data = table.data_rows(&m);
    let width = table.width();
    let column = |i: usize| data.iter().map(move |r| r.get(i).map(String::as_str).unwrap_or(""));

    if has_header {
        for (i, name) in names.iter().enumerate() {
            if m.date_col.is_none()
                && header_matches(
                    name,
                    &["date", "transaction date", "posted date", "posting date", "trans date", "effective date"],
                )
            {
                m.date_col = Some(i);
            } else if header_matches(
                name,
                &[
                    "description",
                    "details",
                    "transaction details",
                    "memo",
                    "payee",
                    "name",
                    "description 1",
                    "description 2",
                    "transaction description",
                    "narrative",
                ],
            ) {
                m.description_cols.push(i);
            } else if header_matches(name, &["amount", "transaction amount", "amount (cad)", "cad$", "cad"]) {
                m.amount_col = Some(i);
            } else if header_matches(
                name,
                &[
                    "debit",
                    "debits",
                    "withdrawal",
                    "withdrawals",
                    "money out",
                    "debit amount",
                    "withdrawal amount",
                    "funds out",
                ],
            ) {
                m.debit_col = Some(i);
            } else if header_matches(
                name,
                &[
                    "credit",
                    "credits",
                    "deposit",
                    "deposits",
                    "money in",
                    "credit amount",
                    "deposit amount",
                    "funds in",
                ],
            ) {
                m.credit_col = Some(i);
            } else if header_matches(name, &["balance", "running balance", "account balance", "balance (cad)"]) {
                m.balance_col = Some(i);
            } else if header_matches(
                name,
                &[
                    "cheque",
                    "cheque number",
                    "cheque #",
                    "cheque no",
                    "check number",
                    "chq",
                    "chq #",
                    "reference",
                    "reference number",
                    "ref",
                ],
            ) {
                m.reference_col = Some(i);
            }
        }
    }

    if m.date_col.is_none() {
        m.date_col = (0..width).find(|&i| detect_date_format(column(i)).is_some());
    }
    if let Some(dc) = m.date_col {
        m.date_format = detect_date_format(column(dc)).unwrap_or("%Y-%m-%d").to_string();
    } else {
        m.date_format = "%Y-%m-%d".to_string();
    }

    if m.amount_col.is_none() && m.debit_col.is_none() && m.credit_col.is_none() {
        // No usable header: take the numeric columns, left to right.
        let numeric: Vec<usize> = (0..width)
            .filter(|&i| Some(i) != m.date_col)
            .filter(|&i| {
                let mut any = false;
                for v in column(i) {
                    if v.is_empty() {
                        continue;
                    }
                    if money::parse(v).is_none() {
                        return false;
                    }
                    any = true;
                }
                any
            })
            .collect();
        match numeric.as_slice() {
            [a] => m.amount_col = Some(*a),
            [a, b] => {
                m.amount_col = Some(*a);
                m.balance_col = Some(*b);
            }
            [.., d, c, b] => {
                m.debit_col = Some(*d);
                m.credit_col = Some(*c);
                m.balance_col = Some(*b);
            }
            _ => {}
        }
    }

    if m.description_cols.is_empty() {
        let used = [m.date_col, m.amount_col, m.debit_col, m.credit_col, m.balance_col, m.reference_col];
        let best = (0..width)
            .filter(|i| !used.contains(&Some(*i)))
            .max_by_key(|&i| column(i).map(|v| v.chars().filter(|c| c.is_alphabetic()).count()).sum::<usize>());
        if let Some(i) = best {
            m.description_cols.push(i);
        }
    }
    m
}

#[derive(Debug, Clone)]
pub struct RowError {
    pub line: usize,
    pub message: String,
}

/// Applies the mapping. Rows that fail are reported, not imported.
pub fn parse_rows(table: &Table, m: &CsvMapping) -> (Vec<BankRow>, Vec<RowError>) {
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let offset = m.skip_rows + if m.has_header { 2 } else { 1 };
    let cell = |row: &Vec<String>, col: Option<usize>| -> String {
        col.and_then(|c| row.get(c)).map(|s| s.trim().to_string()).unwrap_or_default()
    };
    for (i, row) in table.data_rows(m).iter().enumerate() {
        let line = i + offset;
        let Some(date) = parse_date(&cell(row, m.date_col), &m.date_format) else {
            errors.push(RowError { line, message: format!("no date in {:?}", cell(row, m.date_col)) });
            continue;
        };
        let amount = if let Some(col) = m.amount_col {
            money::parse(&cell(row, Some(col))).map(|a| if m.invert_amount { -a } else { a })
        } else {
            let debit = money::parse(&cell(row, m.debit_col)).map(|v| v.abs());
            let credit = money::parse(&cell(row, m.credit_col)).map(|v| v.abs());
            match (debit, credit) {
                (None, None) => None,
                (d, c) => Some(c.unwrap_or(0) - d.unwrap_or(0)),
            }
        };
        let Some(amount_cents) = amount else {
            errors.push(RowError { line, message: "no amount".into() });
            continue;
        };
        let description = m
            .description_cols
            .iter()
            .map(|c| cell(row, Some(*c)))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let description = collapse_spaces(&description);
        let key = format!("{date}|{amount_cents}|{}", description.to_lowercase());
        let n = seen.entry(key.clone()).or_insert(0);
        *n += 1;
        let digest = Sha256::digest(format!("{key}|{n}").as_bytes());
        rows.push(BankRow {
            date,
            description: if description.is_empty() { "(no description)".into() } else { description },
            amount_cents,
            balance_cents: money::parse(&cell(row, m.balance_col)),
            reference: cell(row, m.reference_col),
            bank_ref: format!("csv:{}", &hex::encode(digest)[..20]),
        });
    }
    (rows, errors)
}

fn collapse_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_with_debit_credit() {
        let csv = "Date,Description,Withdrawals,Deposits,Balance\n\
                   09/02/2026,COSTCO WHOLESALE #123,152.30,,1847.70\n\
                   09/15/2026,E-TRANSFER FROM J SMITH,,40.00,1887.70\n";
        let t = read(csv.as_bytes()).unwrap();
        let m = detect(&t);
        assert!(m.has_header);
        assert_eq!(m.date_col, Some(0));
        assert_eq!(m.date_format, "%m/%d/%Y");
        assert_eq!(m.debit_col, Some(2));
        assert_eq!(m.credit_col, Some(3));
        let (rows, errors) = parse_rows(&t, &m);
        assert!(errors.is_empty());
        assert_eq!(rows[0].amount_cents, -15230);
        assert_eq!(rows[1].amount_cents, 4000);
        assert_eq!(rows[1].balance_cents, Some(188770));
    }

    #[test]
    fn headerless_signed_amount() {
        let csv = "2026-09-02,\"COSTCO, BURNABY\",-152.30,1847.70\n2026-09-02,\"COSTCO, BURNABY\",-152.30,1695.40\n";
        let t = read(csv.as_bytes()).unwrap();
        let m = detect(&t);
        assert!(!m.has_header);
        assert_eq!(m.amount_col, Some(2));
        assert_eq!(m.balance_col, Some(3));
        let (rows, _) = parse_rows(&t, &m);
        assert_eq!(rows.len(), 2);
        // Identical rows on the same day get different references.
        assert_ne!(rows[0].bank_ref, rows[1].bank_ref);
    }

    #[test]
    fn preamble() {
        let csv = "Account,12345-678\n\nDate,Description,Amount\n2026-09-02,Fee,-1.50\n2026-09-03,Deposit,20\n";
        let t = read(csv.as_bytes()).unwrap();
        let m = detect(&t);
        assert_eq!(m.skip_rows, 1);
        assert!(m.has_header);
        let (rows, errors) = parse_rows(&t, &m);
        assert!(errors.is_empty());
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn windows_1252() {
        let bytes = b"Date,Description,Amount\n2026-09-02,Caf\xe9,-5.00\n";
        let t = read(bytes).unwrap();
        assert_eq!(t.rows[1][1], "Café");
    }
}
