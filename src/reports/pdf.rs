//! PDF output. Each report builds a JSON document and a Typst template lays it
//! out. Typst and its fonts are built into the binary.

use chrono::NaiveDate;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use typst::foundations::{Dict, IntoValue};
use typst_as_lib::TypstEngine;
use typst_as_lib::typst_kit_options::TypstKitFontOptions;
use typst_layout::PagedDocument;

use super::gaming::{self, GamingReport, Level, gasr_date};
use super::proposal::Proposal;
use super::{FundSummary, account_summaries, fund_summaries, monthly_totals};
use crate::db::Settings;
use crate::fiscal;
use crate::ledger::TxRow;
use crate::models::FiscalYear;
use crate::money::Money;

const TEMPLATES: [(&str, &str); 5] = [
    ("lib.typ", include_str!("../../typst/lib.typ")),
    ("monthly.typ", include_str!("../../typst/monthly.typ")),
    ("annual.typ", include_str!("../../typst/annual.typ")),
    ("budget.typ", include_str!("../../typst/budget.typ")),
    ("gaming.typ", include_str!("../../typst/gaming.typ")),
];

pub struct Renderer {
    engine: TypstEngine,
}

impl Renderer {
    pub fn new() -> Renderer {
        let engine = TypstEngine::builder()
            .search_fonts_with(TypstKitFontOptions::default().include_system_fonts(false).include_embedded_fonts(true))
            .with_static_source_file_resolver(TEMPLATES)
            .build();
        Renderer { engine }
    }

    /// Compiles one template with `data` as sys.inputs.data. This is CPU work;
    /// call it from a blocking task.
    pub fn render(&self, template: &str, data: &Value) -> anyhow::Result<Vec<u8>> {
        let mut inputs = Dict::new();
        inputs.insert("data".into(), data.to_string().into_value());
        let doc: PagedDocument = self
            .engine
            .compile_with_input(template, inputs)
            .output
            .map_err(|e| anyhow::anyhow!("typst compile failed: {e:?}"))?;
        typst_pdf::pdf(&doc, &Default::default()).map_err(|e| anyhow::anyhow!("pdf export failed: {e:?}"))
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

fn row(cells: Vec<String>) -> Value {
    json!({ "kind": "row", "cells": cells })
}
fn flag(cells: Vec<String>) -> Value {
    json!({ "kind": "flag", "cells": cells })
}
fn section(title: &str) -> Value {
    json!({ "kind": "section", "cells": [title] })
}
fn subtotal(cells: Vec<String>) -> Value {
    json!({ "kind": "subtotal", "cells": cells })
}
fn total(cells: Vec<String>) -> Value {
    json!({ "kind": "total", "cells": cells })
}
fn note(text: &str) -> Value {
    json!({ "kind": "note", "cells": [text] })
}

fn acc(m: Money) -> String {
    m.accounting()
}

fn long_date(d: NaiveDate) -> String {
    d.format("%B %-d, %Y").to_string()
}

fn short_date(d: NaiveDate) -> String {
    d.format("%b %-d").to_string()
}

fn base(settings: &Settings) -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("org".into(), json!(settings.heading()));
    m.insert("prepared_on".into(), json!(long_date(chrono::Local::now().date_naive())));
    m.insert("prepared_by".into(), json!(settings.treasurer_name));
    m
}

fn figure(label: &str, value: Money, note: &str) -> Value {
    json!({ "label": label, "value": value.to_string(), "note": note })
}

/// Transactions in the period with a problem a reader should know about.
fn flags(txs: &[TxRow]) -> Vec<String> {
    let mut out = Vec::new();
    let uncategorized = txs.iter().filter(|t| !t.is_categorized()).count();
    if uncategorized > 0 {
        out.push(format!("{uncategorized} transaction(s) have no line item. They show as \"Uncategorized\"."));
    }
    let missing = txs.iter().filter(|t| t.needs_receipt()).count();
    if missing > 0 {
        out.push(format!("{missing} payment(s) have no receipt attached."));
    }
    out
}

fn transaction_rows(txs: &[TxRow]) -> Vec<Value> {
    txs.iter()
        .map(|t| {
            let mut desc = t.tx.display_name().to_string();
            if !t.tx.purpose.is_empty() {
                desc = format!("{desc} — {}", t.tx.purpose);
            }
            let line = if t.is_categorized() {
                t.line_names().to_string()
            } else if t.allocation_count == 0 {
                "Uncategorized".to_string()
            } else {
                format!("{}; {} uncategorized", t.line_names(), t.unallocated().abs())
            };
            let receipt = if t.receipt_count > 0 {
                "✓"
            } else if t.needs_receipt() {
                "✗"
            } else {
                ""
            };
            let cells =
                vec![short_date(t.tx.date), t.account_name.clone(), desc, line, acc(t.tx.amount()), receipt.into()];
            if t.is_categorized() && !t.needs_receipt() { row(cells) } else { flag(cells) }
        })
        .collect()
}

/// Lines with no budget and no activity add nothing to a printed report.
fn has_numbers(r: &super::LineRow) -> bool {
    !(r.budget.is_zero() && r.period.is_zero() && r.ytd.is_zero())
}

fn budget_rows(f: &FundSummary, monthly: bool) -> Vec<Value> {
    let mut rows = vec![section("Income")];
    for r in f.income.iter().filter(|r| has_numbers(r)) {
        rows.push(row(if monthly {
            vec![r.name.clone(), acc(r.budget), acc(r.period), acc(r.ytd), acc(r.remaining())]
        } else {
            vec![r.name.clone(), acc(r.budget), acc(r.ytd), acc(r.ytd - r.budget)]
        }));
    }
    rows.push(subtotal(if monthly {
        vec![
            "Total income".into(),
            acc(f.income_budget()),
            acc(f.income_period()),
            acc(f.income_ytd()),
            acc(f.income_budget() - f.income_ytd()),
        ]
    } else {
        vec![
            "Total income".into(),
            acc(f.income_budget()),
            acc(f.income_ytd()),
            acc(f.income_ytd() - f.income_budget()),
        ]
    }));
    rows.push(section("Expenses"));
    for r in f.expense.iter().filter(|r| has_numbers(r)) {
        rows.push(row(if monthly {
            vec![r.name.clone(), acc(r.budget), acc(r.period), acc(r.ytd), acc(r.remaining())]
        } else {
            vec![r.name.clone(), acc(r.budget), acc(r.ytd), acc(r.budget - r.ytd)]
        }));
    }
    rows.push(subtotal(if monthly {
        vec![
            "Total expenses".into(),
            acc(f.expense_budget()),
            acc(f.expense_period()),
            acc(f.expense_ytd()),
            acc(f.expense_budget() - f.expense_ytd()),
        ]
    } else {
        vec![
            "Total expenses".into(),
            acc(f.expense_budget()),
            acc(f.expense_ytd()),
            acc(f.expense_budget() - f.expense_ytd()),
        ]
    }));
    rows.push(total(if monthly {
        vec![
            "Net (income less expenses)".into(),
            acc(f.net_budget()),
            acc(f.net_period()),
            acc(f.net_ytd()),
            String::new(),
        ]
    } else {
        vec![
            "Net (income less expenses)".into(),
            acc(f.net_budget()),
            acc(f.net_ytd()),
            acc(f.net_ytd() - f.net_budget()),
        ]
    }));
    rows
}

pub async fn monthly(pool: &SqlitePool, month: NaiveDate) -> anyhow::Result<Value> {
    let settings = Settings::load(pool).await?;
    let fy = fiscal::ensure(pool, month).await?;
    let end = fiscal::month_end(month);
    let funds = fund_summaries(pool, &fy, month, end).await?;
    let accounts = account_summaries(pool, month, end).await?;
    let txs = super::transactions(pool, month, end).await?;

    let mut account_rows = Vec::new();
    let mut outstanding = Vec::new();
    let mut reconcile = Vec::new();
    for a in &accounts {
        let name = if a.account.is_gaming() { format!("{} (gaming)", a.account.name) } else { a.account.name.clone() };
        let bank = match a.bank_balance {
            Some((d, b)) => format!("{} ({})", acc(b), short_date(d)),
            None => "–".into(),
        };
        account_rows.push(row(vec![name, acc(a.opening), acc(a.money_in), acc(-a.money_out), acc(a.closing), bank]));
        for t in &a.outstanding {
            let r = if t.reference.is_empty() { String::new() } else { format!("#{} ", t.reference) };
            outstanding.push(format!(
                "{} {r}{} {} ({})",
                a.account.name,
                t.display_name(),
                t.amount(),
                short_date(t.date)
            ));
        }
        if a.reconciled() == Some(false) {
            let (d, b) = a.bank_balance.expect("reconciled() checked");
            let book = a.book_at_bank_date.expect("reconciled() checked");
            reconcile.push(format!(
                "{}: the bank shows {} on {}, but the books show {}. Difference: {}. Check for a missing import or a wrong opening balance.",
                a.account.name,
                b,
                short_date(d),
                book,
                b - book
            ));
        }
    }
    let sum = |f: fn(&super::AccountSummary) -> Money| accounts.iter().map(f).sum::<Money>();
    account_rows.push(total(vec![
        "Total".into(),
        acc(sum(|a| a.opening)),
        acc(sum(|a| a.money_in)),
        acc(-sum(|a| a.money_out)),
        acc(sum(|a| a.closing)),
        String::new(),
    ]));

    let fund_json: Vec<Value> = funds
        .iter()
        .filter(|f| f.has_activity())
        .map(|f| {
            json!({
                "title": f.title(),
                "rows": budget_rows(f, true),
                "opening": (f.opening + f.opened).to_string(),
                "closing": f.closing.to_string(),
                "transfers": if f.transfers_period.is_zero() { String::new() } else { f.transfers_period.to_string() },
            })
        })
        .collect();

    let income: Money = funds.iter().map(|f| f.income_period()).sum();
    let expense: Money = funds.iter().map(|f| f.expense_period()).sum();
    let mut figures = Vec::new();
    for f in &funds {
        if f.fund == crate::models::Fund::General || f.has_activity() {
            figures.push(figure(f.title(), f.closing, "balance at month end"));
        }
    }
    figures.push(figure("Income", income, "this month"));
    figures.push(figure("Expenses", expense, "this month"));

    let mut m = base(&settings);
    m.insert("month_label".into(), json!(month.format("%B %Y").to_string()));
    m.insert("month_short".into(), json!(month.format("%b %Y").to_string()));
    m.insert("fy_label".into(), json!(fy.label));
    m.insert("figures".into(), json!(figures));
    m.insert("accounts".into(), json!(account_rows));
    m.insert("outstanding".into(), json!(outstanding));
    m.insert("reconcile_warnings".into(), json!(reconcile));
    m.insert("funds".into(), json!(fund_json));
    m.insert("transactions".into(), json!(transaction_rows(&txs)));
    m.insert("flags".into(), json!(flags(&txs)));
    Ok(Value::Object(m))
}

/// The short gaming summary in the annual report. Lines that are zero are left out.
fn gaming_summary_rows(g: &GamingReport) -> Vec<Value> {
    let mut rows = vec![row(vec!["Opening balance (line 1)".into(), acc(g.opening)])];
    let optional = [
        ("Community Gaming Grant (line 2)", g.community_grant),
        ("Other gaming revenue (line 5)", g.other_income_total()),
        ("Licensed gaming event revenue (lines 6 to 13)", g.event_revenue()),
        ("Donations from other organizations (lines 15 to 17)", g.donations_total()),
        ("Less: prize costs for licensed events (line 22)", -g.prizes.total()),
        ("Less: expenses for licensed events (line 26)", -g.event_expenses.total()),
    ];
    rows.push(row(vec!["PAC gaming grant received (line 3)".into(), acc(g.grant)]));
    for (label, amount) in &optional[..2] {
        if !amount.is_zero() {
            rows.push(row(vec![label.to_string(), acc(*amount)]));
        }
    }
    rows.push(row(vec!["Interest and GST rebates (line 14)".into(), acc(g.interest_gst)]));
    for (label, amount) in &optional[2..] {
        if !amount.is_zero() {
            rows.push(row(vec![label.to_string(), acc(*amount)]));
        }
    }
    rows.push(subtotal(vec!["Total available (line 28)".into(), acc(g.available())]));
    rows.push(row(vec![
        format!("Disbursements, {} items (line 46)", g.disbursements.len()),
        acc(-g.total_disbursed()),
    ]));
    rows.push(total(vec!["Closing balance (line 47)".into(), acc(g.closing())]));
    rows
}

/// Sections 3 to 6 of the GASR, line by line, as the form lays them out.
fn gasr_lines(g: &GamingReport) -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
    let line = |n: &str, label: &str, amount: Money| row(vec![n.into(), label.into(), acc(amount)]);
    let mut revenue = vec![
        line("2", "Community Gaming Grant revenue", g.community_grant),
        line("3", "PAC/DPAC grant revenue", g.grant),
        line("4", "Capital Project Grant revenue (not used by PACs)", Money(0)),
    ];
    if g.other_income.is_empty() {
        revenue.push(line("5", "Other gaming revenue", Money(0)));
    } else {
        let names: Vec<&str> = g.other_income.iter().map(|o| o.name.as_str()).collect();
        let cells =
            vec!["5".into(), format!("Other gaming revenue: {}", names.join("; ")), acc(g.other_income_total())];
        revenue.push(if g.not_allowed.is_zero() { row(cells) } else { flag(cells) });
    }
    revenue.push(line("6–13", "Gross revenue from gaming event licences (see the licence table)", g.event_revenue()));
    revenue.push(line("14", "GST rebate, interest, or sale of assets bought with gaming funds", g.interest_gst));
    if g.donations.is_empty() {
        revenue.push(line("15–17", "Gaming fund donations from other organizations", Money(0)));
    } else {
        for (i, d) in g.donations.iter().enumerate() {
            revenue.push(line(&(15 + i).to_string(), &format!("Gaming fund donation: {}", d.name), d.amount));
        }
    }
    revenue.push(total(vec!["18".into(), "Total gaming revenue (lines 2 to 17)".into(), acc(g.total_revenue())]));

    let other = |base: &str, c: &super::gaming::CostLines| {
        if c.other_types.is_empty() { base.to_string() } else { format!("{base} ({})", c.other_label()) }
    };
    let costs = vec![
        line("19", "Bingo prize costs", g.prizes.bingo),
        line("20", "Ticket raffle prize costs", g.prizes.ticket_raffle),
        line("21", &other("Other prize costs", &g.prizes), g.prizes.other),
        subtotal(vec!["22".into(), "Total prize costs (lines 19 to 21)".into(), acc(g.prizes.total())]),
        line("23", "Bingo expenses", g.event_expenses.bingo),
        line("24", "Ticket raffle expenses", g.event_expenses.ticket_raffle),
        line("25", &other("Other gaming event expenses", &g.event_expenses), g.event_expenses.other),
        subtotal(vec![
            "26".into(),
            "Total gaming event expenses (lines 23 to 25)".into(),
            acc(g.event_expenses.total()),
        ]),
    ];

    let proceeds = vec![
        line("1", "Opening balance (last year's line 47)", g.opening),
        line("27", "Net gaming proceeds (line 18 − line 22 − line 26)", g.net_proceeds()),
        subtotal(vec!["28".into(), "Total available for disbursement (line 1 + line 27)".into(), acc(g.available())]),
        line("29", "Capital Project Grant disbursements (not used by PACs)", Money(0)),
        line("46", "Total gaming fund disbursements (lines 29 to 45)", g.total_disbursed()),
        total(vec!["47".into(), "Closing balance (line 28 − line 46)".into(), acc(g.closing())]),
    ];

    let mut licences: Vec<Value> = g
        .events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let line_no = if i < 8 { (6 + i).to_string() } else { "sheet".into() };
            let (number, class) = match &e.event {
                Some(ev) => (ev.licence_number.clone(), format!("Class {} ({})", ev.licence_class, ev.type_label())),
                None => ("licence not chosen".into(), String::new()),
            };
            let cells = vec![line_no, number, class, gasr_date(e.from), gasr_date(e.to), acc(e.amount)];
            if e.event.is_none() { flag(cells) } else { row(cells) }
        })
        .collect();
    if licences.is_empty() {
        licences.push(note("No licensed gaming event revenue in this fiscal year."));
    } else {
        licences.push(total(vec![
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            "Total".into(),
            acc(g.event_revenue()),
        ]));
    }
    (revenue, costs, proceeds, licences)
}

pub async fn annual(pool: &SqlitePool, fy: &FiscalYear) -> anyhow::Result<Value> {
    let settings = Settings::load(pool).await?;
    let funds = fund_summaries(pool, fy, fy.start_date, fy.end_date).await?;
    let accounts = account_summaries(pool, fy.start_date, fy.end_date).await?;
    let txs = super::transactions(pool, fy.start_date, fy.end_date).await?;
    let months = monthly_totals(pool, fy).await?;
    let has_gaming = crate::db::accounts(pool).await?.iter().any(|a| a.is_gaming());

    let col = |f: fn(&FundSummary) -> Money| -> Vec<String> {
        let vals: Vec<Money> = funds.iter().map(f).collect();
        let mut cells: Vec<String> = vals.iter().map(|m| acc(*m)).collect();
        cells.push(acc(vals.iter().copied().sum()));
        cells
    };
    let labelled = |label: &str, cells: Vec<String>| {
        let mut v = vec![label.to_string()];
        v.extend(cells);
        v
    };
    let mut summary = vec![row(labelled("Opening balance", col(|f| f.year_opening)))];
    if funds.iter().any(|f| !f.year_opened.is_zero()) {
        summary.push(row(labelled("Add: accounts opened during the year", col(|f| f.year_opened))));
    }
    summary.extend([
        row(labelled("Add: income", col(|f| f.income_ytd()))),
        row(labelled("Less: expenses", col(|f| -f.expense_ytd()))),
    ]);
    if funds.iter().any(|f| !f.transfers_ytd.is_zero()) {
        summary.push(row(labelled("Transfers between accounts", col(|f| f.transfers_ytd))));
    }
    summary.push(total(labelled("Closing balance", col(|f| f.closing))));
    if funds.iter().any(|f| !f.balances()) {
        summary.push(note("The opening balance plus activity does not equal the closing balance. Check the account opening balances and dates."));
    }

    let fund_json: Vec<Value> = funds
        .iter()
        .filter(|f| f.fund == crate::models::Fund::General || f.has_activity())
        .map(|f| json!({ "title": f.title(), "rows": budget_rows(f, false) }))
        .collect();

    let mut account_rows: Vec<Value> = accounts
        .iter()
        .map(|a| {
            let bank = match a.bank_balance {
                Some((d, b)) => format!("{} ({})", acc(b), short_date(d)),
                None => "–".into(),
            };
            row(vec![a.account.name.clone(), acc(a.closing), bank, acc(a.outstanding_total())])
        })
        .collect();
    account_rows.push(total(vec![
        "Total".into(),
        acc(accounts.iter().map(|a| a.closing).sum()),
        String::new(),
        acc(accounts.iter().map(|a| a.outstanding_total()).sum()),
    ]));

    let mut month_rows: Vec<Value> = months
        .iter()
        .map(|m| {
            row(vec![
                m.month.format("%B %Y").to_string(),
                acc(m.general_in),
                acc(m.general_out),
                acc(m.gaming_in),
                acc(m.gaming_out),
            ])
        })
        .collect();
    month_rows.push(total(vec![
        "Total".into(),
        acc(months.iter().map(|m| m.general_in).sum()),
        acc(months.iter().map(|m| m.general_out).sum()),
        acc(months.iter().map(|m| m.gaming_in).sum()),
        acc(months.iter().map(|m| m.gaming_out).sum()),
    ]));

    let gaming_json = if has_gaming {
        let g = gaming::build(pool, fy).await?;
        let checks: Vec<Value> = g
            .checks
            .iter()
            .filter(|c| c.level != Level::Info)
            .map(|c| json!({ "level": c.level.css(), "message": c.message }))
            .collect();
        json!({ "rows": gaming_summary_rows(&g), "checks": checks })
    } else {
        Value::Null
    };

    let mut figures = Vec::new();
    for f in &funds {
        if f.fund == crate::models::Fund::General || f.has_activity() {
            figures.push(figure(f.title(), f.closing, "balance at year end"));
        }
    }
    figures.push(figure("Income", funds.iter().map(|f| f.income_ytd()).sum(), "for the year"));
    figures.push(figure("Expenses", funds.iter().map(|f| f.expense_ytd()).sum(), "for the year"));

    let mut m = base(&settings);
    m.insert("fy_label".into(), json!(fy.label));
    m.insert("period".into(), json!(fy.period()));
    m.insert("year_end".into(), json!(long_date(fy.end_date)));
    m.insert("figures".into(), json!(figures));
    m.insert("summary".into(), json!(summary));
    m.insert("funds".into(), json!(fund_json));
    m.insert("accounts".into(), json!(account_rows));
    m.insert("months".into(), json!(month_rows));
    m.insert("gaming".into(), gaming_json);
    m.insert("notes".into(), json!(fy.notes));
    m.insert("flags".into(), json!(flags(&txs)));
    m.insert("treasurer".into(), json!(settings.treasurer_name));
    m.insert("chair".into(), json!(settings.chair_name));
    Ok(Value::Object(m))
}

pub async fn budget(pool: &SqlitePool, p: &Proposal) -> anyhow::Result<Value> {
    let settings = Settings::load(pool).await?;
    let mut fund_json = Vec::new();
    for f in &p.funds {
        if f.income.is_empty() && f.expense.is_empty() {
            continue;
        }
        let line = |l: &super::proposal::ProposalLine| {
            row(vec![
                l.name.clone(),
                acc(l.prior_budget),
                acc(l.prior_actual),
                acc(l.proposed),
                acc(l.change()),
                l.notes.clone(),
            ])
        };
        let shown = |l: &&super::proposal::ProposalLine| {
            !(l.prior_budget.is_zero() && l.prior_actual.is_zero() && l.proposed.is_zero())
        };
        let mut rows = vec![section("Income")];
        rows.extend(f.income.iter().filter(shown).map(line));
        rows.push(subtotal(vec![
            "Total income".into(),
            acc(f.income_prior_budget()),
            acc(f.income_prior_actual()),
            acc(f.income_proposed()),
            acc(f.income_proposed() - f.income_prior_budget()),
            String::new(),
        ]));
        rows.push(section("Expenses"));
        rows.extend(f.expense.iter().filter(shown).map(line));
        rows.push(subtotal(vec![
            "Total expenses".into(),
            acc(f.expense_prior_budget()),
            acc(f.expense_prior_actual()),
            acc(f.expense_proposed()),
            acc(f.expense_proposed() - f.expense_prior_budget()),
            String::new(),
        ]));
        rows.push(total(vec![
            "Surplus / (deficit)".into(),
            acc(f.net_prior_budget()),
            acc(f.net_prior_actual()),
            acc(f.net_proposed()),
            acc(f.net_proposed() - f.net_prior_budget()),
            String::new(),
        ]));
        let deficit_note = if f.net_proposed().is_negative() {
            format!(" The deficit of {} is drawn from the fund balance.", f.net_proposed().abs())
        } else {
            String::new()
        };
        fund_json.push(json!({
            "title": f.title(),
            "rows": rows,
            "note": format!("Fund balance on {}: {}.{deficit_note}", long_date(p.actuals_to), f.balance_now),
        }));
    }
    let income: Money = p.funds.iter().map(|f| f.income_proposed()).sum();
    let expense: Money = p.funds.iter().map(|f| f.expense_proposed()).sum();
    let mut figures = vec![
        figure("Proposed income", income, ""),
        figure("Proposed expenses", expense, ""),
        figure(if income >= expense { "Surplus" } else { "Deficit" }, (income - expense).abs(), ""),
    ];
    if p.enrolment > 0 {
        figures.push(figure("Expected gaming grant", p.expected_grant, &format!("{} students", p.enrolment)));
    }
    let grant_note = if p.enrolment > 0 {
        format!(
            "Expected gaming grant: {} students × {} = {}. The grant uses enrolment at the start of the previous school year.",
            p.enrolment, p.gaming_rate, p.expected_grant
        )
    } else {
        String::new()
    };
    let mut m = base(&settings);
    m.insert("target_label".into(), json!(p.target.label));
    m.insert("target_period".into(), json!(p.target.period()));
    m.insert("base_label".into(), json!(p.base.label));
    m.insert("base_complete".into(), json!(p.base_complete));
    m.insert("actuals_to".into(), json!(long_date(p.actuals_to)));
    m.insert("status".into(), json!(p.target.budget_status));
    m.insert("figures".into(), json!(figures));
    m.insert("funds".into(), json!(fund_json));
    m.insert("grant_note".into(), json!(grant_note));
    m.insert("notes".into(), json!(p.target.notes));
    Ok(Value::Object(m))
}

pub async fn gasr(pool: &SqlitePool, g: &GamingReport) -> anyhow::Result<Value> {
    let settings = Settings::load(pool).await?;
    let (revenue, costs, proceeds, licences) = gasr_lines(g);

    let mut event_costs: Vec<Value> = g
        .event_costs
        .iter()
        .map(|c| {
            let licence = c.event.as_ref().map(|e| e.licence_number.clone()).unwrap_or_else(|| "not chosen".into());
            let cells = vec![
                gasr_date(c.tx.date),
                c.tx.reference.clone(),
                c.tx.display_name().to_string(),
                licence,
                c.kind_label().into(),
                acc(c.amount),
            ];
            if c.event.is_none() { flag(cells) } else { row(cells) }
        })
        .collect();
    if !event_costs.is_empty() {
        event_costs.push(total(vec![
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            "Total".into(),
            acc(g.prizes.total() + g.event_expenses.total()),
        ]));
    }

    let mut disb: Vec<Value> = g
        .disbursements
        .iter()
        .map(|d| {
            let cells = vec![
                gasr_date(d.tx.date),
                d.reference.clone(),
                d.disbursed_to.clone(),
                d.purpose.clone(),
                acc(d.amount),
            ];
            if d.receipt_required && !d.has_receipt && d.reimbursements.is_empty() { flag(cells) } else { row(cells) }
        })
        .collect();
    disb.push(total(vec![
        String::new(),
        String::new(),
        String::new(),
        "Total (line 46)".into(),
        acc(g.total_disbursed()),
    ]));

    let reimbursements: Vec<Value> = g
        .disbursements
        .iter()
        .filter(|d| !d.reimbursements.is_empty())
        .map(|d| {
            let mut rows: Vec<Value> = d
                .reimbursements
                .iter()
                .map(|r| {
                    let purpose = if r.purpose.is_empty() { r.description.clone() } else { r.purpose.clone() };
                    row(vec![gasr_date(r.date), r.display_name().to_string(), purpose, acc(-r.amount())])
                })
                .collect();
            rows.push(total(vec![String::new(), String::new(), "Total".into(), acc(d.reimbursement_total())]));
            if d.reimbursement_total() != d.amount {
                rows.push(note(&format!("Does not match the transfer of {}.", d.amount)));
            }
            json!({ "title": format!("{} transfer of {}", gasr_date(d.tx.date), d.amount), "rows": rows })
        })
        .collect();

    let mut accounts: Vec<Value> = g
        .accounts
        .iter()
        .map(|(a, b)| {
            let name =
                if a.account_number.is_empty() { a.name.clone() } else { format!("{} ({})", a.name, a.account_number) };
            row(vec![format!("{name}, {}", a.institution), acc(*b)])
        })
        .collect();
    accounts.push(total(vec!["Total (line 52, must equal line 47)".into(), acc(g.accounts_total)]));

    let mut deposits: Vec<Value> = g
        .deposits
        .iter()
        .map(|d| {
            let cells = vec![gasr_date(d.tx.date), d.tx.display_name().to_string(), d.lines.clone(), acc(d.amount)];
            if d.allowed { row(cells) } else { flag(cells) }
        })
        .collect();
    if deposits.is_empty() {
        deposits.push(note("No deposits in this fiscal year."));
    }

    let grants: Vec<Value> = g
        .grants
        .iter()
        .map(|r| row(vec![gasr_date(r.received), acc(r.amount), gasr_date(r.spend_by), acc(r.unspent)]))
        .collect();
    let checks: Vec<Value> = g.checks.iter().map(|c| json!({ "level": c.level.css(), "message": c.message })).collect();

    let mut m = base(&settings);
    m.insert("year_end".into(), json!(long_date(g.fy.end_date)));
    m.insert("due_date".into(), json!(long_date(g.due_date)));
    m.insert("file_number".into(), json!(settings.gaming_org_number));
    m.insert("revenue".into(), json!(revenue));
    m.insert("licences".into(), json!(licences));
    m.insert("costs".into(), json!(costs));
    m.insert("has_events".into(), json!(g.has_events()));
    m.insert("event_costs".into(), json!(event_costs));
    m.insert("proceeds".into(), json!(proceeds));
    m.insert("disbursements".into(), json!(disb));
    m.insert("reimbursements".into(), json!(reimbursements));
    m.insert("accounts".into(), json!(accounts));
    m.insert("deposits".into(), json!(deposits));
    m.insert("grants".into(), json!(grants));
    m.insert("checks".into(), json!(checks));
    Ok(Value::Object(m))
}
