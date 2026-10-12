//! OFX, QFX, and QBO files (all OFX inside). Handles both OFX 1.x SGML, where
//! closing tags are optional, and OFX 2.x XML. The FITID is the bank's own
//! transaction ID, so duplicates are exact.

use chrono::NaiveDate;

use super::BankRow;
use crate::money;

#[derive(Debug, Clone, Default)]
pub struct Statement {
    pub account_id: String,
    pub ledger_balance_cents: Option<i64>,
    pub ledger_balance_date: Option<NaiveDate>,
    pub rows: Vec<BankRow>,
}

pub fn looks_like_ofx(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_ascii_uppercase();
    head.contains("OFXHEADER") || head.contains("<OFX>")
}

pub fn parse(bytes: &[u8]) -> anyhow::Result<Statement> {
    let text = super::csv::decode(bytes);
    let mut st = Statement { account_id: tag_value(&text, "ACCTID").unwrap_or_default(), ..Default::default() };
    if let Some(ledger) = block(&text, "LEDGERBAL", 0).map(|(b, _)| b) {
        st.ledger_balance_cents = tag_value(ledger, "BALAMT").and_then(|v| money::parse(&v));
        st.ledger_balance_date = tag_value(ledger, "DTASOF").and_then(|v| ofx_date(&v));
    }
    let mut pos = 0;
    while let Some((body, next)) = block(&text, "STMTTRN", pos) {
        pos = next;
        let date = tag_value(body, "DTPOSTED")
            .and_then(|v| ofx_date(&v))
            .ok_or_else(|| anyhow::anyhow!("transaction without DTPOSTED"))?;
        let amount_cents = tag_value(body, "TRNAMT")
            .and_then(|v| money::parse(&v))
            .ok_or_else(|| anyhow::anyhow!("transaction without TRNAMT"))?;
        let name = tag_value(body, "NAME").unwrap_or_default();
        let memo = tag_value(body, "MEMO").unwrap_or_default();
        let description = match (name.is_empty(), memo.is_empty()) {
            (false, false) if !name.contains(&memo) => format!("{name} {memo}"),
            (false, _) => name,
            (true, false) => memo,
            (true, true) => tag_value(body, "TRNTYPE").unwrap_or_else(|| "(no description)".into()),
        };
        let fitid = tag_value(body, "FITID").unwrap_or_default();
        let bank_ref =
            if fitid.is_empty() { format!("ofx:{date}|{amount_cents}|{description}") } else { format!("ofx:{fitid}") };
        st.rows.push(BankRow {
            date,
            description,
            amount_cents,
            balance_cents: None,
            reference: tag_value(body, "CHECKNUM").unwrap_or_default(),
            bank_ref,
        });
    }
    if st.rows.is_empty() && !text.to_ascii_uppercase().contains("<BANKTRANLIST>") {
        anyhow::bail!("this file is not an OFX bank statement");
    }
    Ok(st)
}

/// The text between <TAG> and </TAG> (or the next sibling block), starting at `from`.
fn block<'a>(text: &'a str, tag: &str, from: usize) -> Option<(&'a str, usize)> {
    let upper = text.to_ascii_uppercase();
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = upper[from..].find(&open)? + from + open.len();
    let end = match upper[start..].find(&close) {
        Some(e) => start + e,
        // SGML without a closing tag: stop at the next block of the same kind or the list end.
        None => upper[start..]
            .find(&open)
            .or_else(|| upper[start..].find("</BANKTRANLIST>"))
            .map(|e| start + e)
            .unwrap_or(text.len()),
    };
    Some((&text[start..end], end))
}

/// The value of a leaf element such as <TRNAMT>-12.34 or <TRNAMT>-12.34</TRNAMT>.
fn tag_value(text: &str, tag: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    let open = format!("<{tag}>");
    let start = upper.find(&open)? + open.len();
    let rest = &text[start..];
    let end = rest.find(['<', '\n', '\r']).unwrap_or(rest.len());
    let value = decode_entities(rest[..end].trim());
    if value.is_empty() { None } else { Some(value) }
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// "20260930120000.000[-8:PST]" -> 2026-09-30
fn ofx_date(v: &str) -> Option<NaiveDate> {
    let digits: String = v.chars().take(8).collect();
    NaiveDate::parse_from_str(&digits, "%Y%m%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SGML: &str = "OFXHEADER:100\nDATA:OFXSGML\nVERSION:102\n\n<OFX>\n<BANKMSGSRSV1><STMTTRNRS><STMTRS>\n\
        <BANKACCTFROM><BANKID>123<ACCTID>98765<ACCTTYPE>CHECKING</BANKACCTFROM>\n<BANKTRANLIST>\n\
        <STMTTRN>\n<TRNTYPE>DEBIT\n<DTPOSTED>20260902120000\n<TRNAMT>-152.30\n<FITID>A1\n<NAME>COSTCO &amp; CO\n\
        <STMTTRN>\n<TRNTYPE>CHECK\n<DTPOSTED>20260905\n<TRNAMT>-80.00\n<FITID>A2\n<CHECKNUM>104\n<NAME>CHEQUE\n<MEMO>Bus rental\n\
        </BANKTRANLIST>\n<LEDGERBAL><BALAMT>1767.70<DTASOF>20260930</LEDGERBAL>\n</STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>";

    #[test]
    fn sgml() {
        let st = parse(SGML.as_bytes()).unwrap();
        assert_eq!(st.account_id, "98765");
        assert_eq!(st.rows.len(), 2);
        assert_eq!(st.rows[0].amount_cents, -15230);
        assert_eq!(st.rows[0].description, "COSTCO & CO");
        assert_eq!(st.rows[1].reference, "104");
        assert_eq!(st.rows[1].description, "CHEQUE Bus rental");
        assert_eq!(st.rows[1].bank_ref, "ofx:A2");
        assert_eq!(st.ledger_balance_cents, Some(176770));
    }

    #[test]
    fn xml() {
        let xml = "<?xml version=\"1.0\"?><?OFX OFXHEADER=\"200\"?><OFX><BANKTRANLIST>\
            <STMTTRN><TRNTYPE>CREDIT</TRNTYPE><DTPOSTED>20260915</DTPOSTED><TRNAMT>40.00</TRNAMT>\
            <FITID>B7</FITID><NAME>E-TRANSFER</NAME></STMTTRN></BANKTRANLIST></OFX>";
        let st = parse(xml.as_bytes()).unwrap();
        assert_eq!(st.rows.len(), 1);
        assert_eq!(st.rows[0].amount_cents, 4000);
        assert_eq!(st.rows[0].date, NaiveDate::from_ymd_opt(2026, 9, 15).unwrap());
    }
}
