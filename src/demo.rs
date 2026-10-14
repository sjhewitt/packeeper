//! An example PAC with a year and a half of activity, for trying the app and
//! for testing the reports. All names are made up.

use chrono::{Datelike, Months, NaiveDate};
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::db::{self, Settings};
use crate::fiscal;

/// The demo data is written as of this date. Loading it later moves every
/// date forward by whole years, so the demo never looks stale.
const WRITTEN_ON: &str = "2026-10-12";

/// Whole years to add so that the demo's last day falls on or before `today`.
pub fn years_since_written(today: NaiveDate) -> u32 {
    let written = NaiveDate::parse_from_str(WRITTEN_ON, "%Y-%m-%d").expect("demo date");
    let mut years = 0;
    while written + Months::new(12 * (years + 1)) <= today {
        years += 1;
    }
    years
}

struct Demo<'a> {
    pool: &'a SqlitePool,
    receipts_dir: std::path::PathBuf,
    seq: i64,
    years: u32,
}

impl Demo<'_> {
    /// A demo date, moved forward by the demo's year shift.
    fn date(&self, s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("demo date") + Months::new(12 * self.years)
    }
}

#[derive(Default)]
struct Tx<'a> {
    payee: &'a str,
    purpose: &'a str,
    reference: &'a str,
    receipt: bool,
    cleared: Option<bool>,
    event: Option<i64>,
    /// The fundraiser of every part.
    fundraiser: Option<i64>,
}

impl Demo<'_> {
    async fn line(&self, fund: &str, name: &str) -> anyhow::Result<i64> {
        sqlx::query_scalar("SELECT id FROM line_items WHERE fund = ? AND name = ?")
            .bind(fund)
            .bind(name)
            .fetch_one(self.pool)
            .await
            .map_err(|e| anyhow::anyhow!("line item {fund}/{name}: {e}"))
    }

    /// Adds a transaction. `lines` is a list of (line item name, amount in dollars).
    #[allow(clippy::too_many_arguments)]
    async fn tx(
        &mut self,
        account: i64,
        fund: &str,
        date: &str,
        desc: &str,
        amount: f64,
        lines: &[(&str, f64)],
        o: Tx<'_>,
    ) -> anyhow::Result<i64> {
        self.seq += 1;
        let cents = (amount * 100.0).round() as i64;
        let manual = o.cleared.is_some();
        let when = self.date(date);
        let id = sqlx::query(
            "INSERT INTO transactions (account_id, date, description, payee, amount_cents, reference, purpose, source, bank_ref,
               cleared, gaming_event_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(account)
        .bind(when)
        .bind(desc)
        .bind(o.payee)
        .bind(cents)
        .bind(o.reference)
        .bind(o.purpose)
        .bind(if manual { "manual" } else { "import" })
        .bind(if manual { None } else { Some(format!("demo:{}", self.seq)) })
        .bind(o.cleared.unwrap_or(true))
        .bind(o.event)
        .bind(db::now())
        .execute(self.pool)
        .await?
        .last_insert_rowid();
        for (name, part) in lines {
            let li = self.line(fund, name).await?;
            sqlx::query(
                "INSERT INTO allocations (transaction_id, line_item_id, amount_cents, fundraiser_id) VALUES (?, ?, ?, ?)",
            )
            .bind(id)
            .bind(li)
            .bind((part * 100.0).round() as i64)
            .bind(o.fundraiser)
            .execute(self.pool)
            .await?;
        }
        if o.receipt {
            let label = if o.payee.is_empty() { desc } else { o.payee };
            let pdf = tiny_pdf(&format!("Receipt - {label} - {when} - ${:.2}", amount.abs()));
            let stored = format!("{}/{}-demo-receipt.pdf", when.year(), id);
            let path = self.receipts_dir.join(&stored);
            std::fs::create_dir_all(path.parent().expect("parent"))?;
            std::fs::write(&path, &pdf)?;
            sqlx::query(
                "INSERT INTO attachments (transaction_id, filename, stored_name, content_type, size_bytes, sha256, uploaded_at)
                 VALUES (?, ?, ?, 'application/pdf', ?, ?, ?)",
            )
            .bind(id)
            .bind(format!("receipt-{when}.pdf"))
            .bind(&stored)
            .bind(pdf.len() as i64)
            .bind(hex::encode(Sha256::digest(&pdf)))
            .bind(db::now())
            .execute(self.pool)
            .await?;
        }
        Ok(id)
    }

    async fn fundraiser(&self, name: &str, description: &str, goals: &[(i64, f64)]) -> anyhow::Result<i64> {
        let id = sqlx::query("INSERT INTO fundraisers (name, description, created_at) VALUES (?, ?, ?)")
            .bind(name)
            .bind(description)
            .bind(db::now())
            .execute(self.pool)
            .await?
            .last_insert_rowid();
        for (fy, goal) in goals {
            sqlx::query("INSERT INTO fundraiser_goals (fundraiser_id, fiscal_year_id, goal_cents) VALUES (?, ?, ?)")
                .bind(id)
                .bind(fy)
                .bind((goal * 100.0).round() as i64)
                .execute(self.pool)
                .await?;
        }
        Ok(id)
    }

    /// Gives one part of a split transaction its fundraiser.
    async fn part_fundraiser(&self, tx: i64, line: &str, fundraiser: i64) -> anyhow::Result<()> {
        let li = self.line("general", line).await?;
        sqlx::query("UPDATE allocations SET fundraiser_id = ? WHERE transaction_id = ? AND line_item_id = ?")
            .bind(fundraiser)
            .bind(tx)
            .bind(li)
            .execute(self.pool)
            .await?;
        Ok(())
    }

    async fn budget(&self, fy: i64, fund: &str, amounts: &[(&str, f64)]) -> anyhow::Result<()> {
        for (name, amount) in amounts {
            let li = self.line(fund, name).await?;
            sqlx::query(
                "INSERT OR REPLACE INTO budget_amounts (fiscal_year_id, line_item_id, amount_cents) VALUES (?, ?, ?)",
            )
            .bind(fy)
            .bind(li)
            .bind((amount * 100.0).round() as i64)
            .execute(self.pool)
            .await?;
        }
        Ok(())
    }
}

/// A one-page PDF with one line of text.
fn tiny_pdf(text: &str) -> Vec<u8> {
    let safe: String = text.chars().filter(|c| c.is_ascii() && !matches!(c, '(' | ')' | '\\')).collect();
    let stream = format!("BT /F1 14 Tf 40 760 Td ({safe}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for off in offsets {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
    );
    out
}

/// Loads the example PAC into an empty database, with dates moved up to today.
pub async fn load(pool: &SqlitePool, data_dir: &std::path::Path) -> anyhow::Result<()> {
    load_shifted(pool, data_dir, years_since_written(chrono::Local::now().date_naive())).await
}

/// Loads the example PAC with every date moved forward by `years`.
pub async fn load_shifted(pool: &SqlitePool, data_dir: &std::path::Path, years: u32) -> anyhow::Result<()> {
    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM accounts").fetch_one(pool).await?;
    if existing > 0 {
        anyhow::bail!("this data folder already has accounts; use an empty folder for the demo (--data-dir demo-data)");
    }
    let mut demo = Demo { pool, receipts_dir: data_dir.join("receipts"), seq: 0, years };
    let d = |s: &str| demo.date(s);
    Settings {
        pac_name: "Example Elementary PAC".into(),
        school_name: "Example Elementary School".into(),
        fy_start_month: 7,
        enrolment: 412,
        gaming_rate_cents: 2000,
        treasurer_name: "Sam Rivera".into(),
        chair_name: "Jordan Lee".into(),
        gaming_org_number: "PAC-000000".into(),
    }
    .save(pool)
    .await?;

    let general = sqlx::query(
        "INSERT INTO accounts (name, kind, account_number, opening_date, opening_balance_cents) VALUES ('General Chequing', 'general', '1234', ?, 841237)",
    )
    .bind(d("2025-07-01"))
    .execute(pool)
    .await?
    .last_insert_rowid();
    let gaming = sqlx::query(
        "INSERT INTO accounts (name, kind, account_number, opening_date, opening_balance_cents) VALUES ('Gaming Account', 'gaming', '5678', ?, 612055)",
    )
    .bind(d("2025-07-01"))
    .execute(pool)
    .await?
    .last_insert_rowid();

    let last = fiscal::ensure(pool, d("2025-07-01")).await?;
    let this = fiscal::ensure(pool, d("2026-07-01")).await?;
    sqlx::query("UPDATE fiscal_years SET budget_status = 'approved', budget_approved_on = ?, notes = ? WHERE id = ?")
        .bind(d("2025-06-11"))
        .bind("Hot lunch sales were higher than planned, and the spring carnival had record attendance. \
               The gaming grant paid for two field trips, a science presentation, new sports equipment, and playground balls.")
        .bind(last.id)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE fiscal_years SET budget_status = 'approved', budget_approved_on = ? WHERE id = ?")
        .bind(d("2026-06-10"))
        .bind(this.id)
        .execute(pool)
        .await?;

    let general_budget: &[(&str, f64)] = &[
        ("Fundraising – Hot lunch", 9000.0),
        ("Fundraising – Events", 4000.0),
        ("Donations", 1500.0),
        ("Interest", 20.0),
        ("Hot lunch costs", 6500.0),
        ("Fundraising event costs", 1500.0),
        ("Staff appreciation", 1200.0),
        ("Classroom grants", 3000.0),
        ("Hospitality and meetings", 400.0),
        ("Insurance", 450.0),
        ("Office and supplies", 250.0),
        ("Bank fees", 60.0),
    ];
    let gaming_budget: &[(&str, f64)] = &[
        ("PAC gaming grant", 8240.0),
        ("Interest and GST rebates", 30.0),
        ("Field trips (within BC)", 4000.0),
        ("Presentations and performances", 2500.0),
        ("School spirit and family events", 1500.0),
        ("Clubs and student competitions", 800.0),
        ("Sports equipment and uniforms", 1500.0),
        ("Playground and beautification", 2000.0),
        ("Graduation and student recognition", 600.0),
        ("BCCPAC membership and conference", 75.0),
        ("Emergency supplies", 500.0),
        ("Licensed gaming event revenue", 2000.0),
        ("Licensed gaming event prizes", 600.0),
        ("Licensed gaming event expenses", 150.0),
    ];
    demo.budget(last.id, "general", general_budget).await?;
    demo.budget(last.id, "gaming", gaming_budget).await?;
    let this_general: Vec<(&str, f64)> = general_budget
        .iter()
        .map(|(n, a)| {
            (
                *n,
                if *n == "Fundraising – Hot lunch" {
                    10000.0
                } else if *n == "Hot lunch costs" {
                    7200.0
                } else {
                    *a
                },
            )
        })
        .collect();
    demo.budget(this.id, "general", &this_general).await?;
    demo.budget(this.id, "gaming", gaming_budget).await?;

    // Fundraisers run every year. Each has a goal for each year.
    let hot_lunch = demo
        .fundraiser(
            "Hot lunch",
            "Munch A Lunch orders, September to June. The PAC keeps the difference between the price and the caterer's bill.",
            &[(last.id, 2500.0), (this.id, 2800.0)],
        )
        .await?;
    let purdys = demo
        .fundraiser(
            "Purdy's chocolates",
            "Holiday chocolate orders in November. Purdy's gives the PAC 25% of sales.",
            &[(last.id, 750.0), (this.id, 900.0)],
        )
        .await?;
    let fall_fair = demo
        .fundraiser(
            "Fall fair",
            "Games, a bake sale, and a barbecue on the first Saturday in October.",
            &[(last.id, 1200.0), (this.id, 1400.0)],
        )
        .await?;
    let movie_night =
        demo.fundraiser("Movie night", "A family movie in the gym, with popcorn.", &[(last.id, 400.0)]).await?;
    let carnival = demo
        .fundraiser(
            "Spring carnival",
            "Rides, games, and food on the field in May.",
            &[(last.id, 1500.0), (this.id, 1600.0)],
        )
        .await?;

    let g = general;
    let fee = |m: &'static str| (m, "SERVICE CHARGE");
    // Monthly bank fees, both years.
    for m in [
        "2025-07", "2025-08", "2025-09", "2025-10", "2025-11", "2025-12", "2026-01", "2026-02", "2026-03", "2026-04",
        "2026-05", "2026-06", "2026-07", "2026-08", "2026-09",
    ] {
        let (month, desc) = fee(m);
        demo.tx(g, "general", &format!("{month}-28"), desc, -5.0, &[("Bank fees", -5.0)], Tx::default()).await?;
    }
    // Hot lunch, September to June.
    let lunches = [
        ("2025-09", 845.50, 610.25),
        ("2025-10", 1012.00, 702.40),
        ("2025-11", 968.75, 688.10),
        ("2025-12", 702.00, 512.35),
        ("2026-01", 990.25, 701.80),
        ("2026-02", 921.50, 655.00),
        ("2026-03", 870.00, 640.15),
        ("2026-04", 1004.75, 720.90),
        ("2026-05", 1101.00, 790.45),
        ("2026-06", 640.50, 455.20),
        ("2026-09", 1120.00, 760.30),
    ];
    for (m, sales, cost) in lunches {
        demo.tx(
            g,
            "general",
            &format!("{m}-12"),
            "DEPOSIT MUNCH A LUNCH PAYOUT",
            sales,
            &[("Fundraising – Hot lunch", sales)],
            Tx { payee: "Munch A Lunch", fundraiser: Some(hot_lunch), ..Default::default() },
        )
        .await?;
        demo.tx(
            g,
            "general",
            &format!("{m}-20"),
            "BILL PAYMENT FRESH KITCHEN CATERING",
            -cost,
            &[("Hot lunch costs", -cost)],
            Tx {
                payee: "Fresh Kitchen Catering",
                purpose: "Hot lunch food",
                receipt: true,
                fundraiser: Some(hot_lunch),
                ..Default::default()
            },
        )
        .await?;
    }
    let general_txs: &[(&str, &str, f64, &str, &str, &str, bool)] = &[
        // (date, description, amount, line item, payee, purpose, receipt)
        (
            "2025-09-15",
            "POS PURCHASE TIM HORTONS #2231",
            -45.20,
            "Hospitality and meetings",
            "Tim Hortons",
            "Coffee for September PAC meeting",
            true,
        ),
        ("2025-09-18", "BILL PAYMENT BCCPAC", -425.00, "Insurance", "BCCPAC", "Directors and officers insurance", true),
        (
            "2025-10-05",
            "DEPOSIT FALL FAIR CASH",
            1850.25,
            "Fundraising – Events",
            "Fall Fair",
            "Fall Fair proceeds",
            false,
        ),
        (
            "2025-10-03",
            "POS PURCHASE COSTCO WHOLESALE #54",
            -420.18,
            "Fundraising event costs",
            "Costco",
            "Fall Fair supplies",
            true,
        ),
        (
            "2025-10-20",
            "CHQ#000211",
            -1000.00,
            "Classroom grants",
            "Classroom grants",
            "Fall classroom grants, 10 classes",
            true,
        ),
        (
            "2025-11-08",
            "POS PURCHASE STAPLES #112",
            -54.99,
            "Office and supplies",
            "Staples",
            "Printer paper and envelopes",
            true,
        ),
        (
            "2025-11-14",
            "POS PURCHASE AMAZON.CA",
            -312.40,
            "Gaming-eligible costs (to be reimbursed)",
            "Amazon",
            "Sports day ribbons",
            true,
        ),
        (
            "2025-11-21",
            "E-TRANSFER SENT KIDS MAGIC SHOWS",
            -187.60,
            "Gaming-eligible costs (to be reimbursed)",
            "Kids Magic Shows",
            "Deposit for winter assembly performer",
            true,
        ),
        ("2025-12-02", "DEPOSIT DONATION", 500.00, "Donations", "Local business donation", "", false),
        (
            "2025-12-12",
            "POS PURCHASE SAVE ON FOODS #9",
            -380.50,
            "Staff appreciation",
            "Save-On-Foods",
            "Staff holiday breakfast",
            true,
        ),
        (
            "2025-12-15",
            "TRANSFER FROM 5678",
            500.00,
            "Reimbursement from gaming account",
            "Gaming Account",
            "Reimbursement of gaming-eligible costs",
            false,
        ),
        ("2025-12-31", "INTEREST", 4.12, "Interest", "", "", false),
        (
            "2026-01-15",
            "POS PURCHASE TIM HORTONS #2231",
            -62.10,
            "Hospitality and meetings",
            "Tim Hortons",
            "Coffee for January PAC meeting",
            true,
        ),
        ("2026-01-22", "CHQ#000212", -1000.00, "Classroom grants", "Classroom grants", "Winter classroom grants", true),
        (
            "2026-02-10",
            "DEPOSIT MOVIE NIGHT",
            640.00,
            "Fundraising – Events",
            "Movie Night",
            "Movie night tickets and snacks",
            false,
        ),
        (
            "2026-02-09",
            "POS PURCHASE COSTCO WHOLESALE #54",
            -212.40,
            "Fundraising event costs",
            "Costco",
            "Movie night snacks",
            true,
        ),
        ("2026-03-11", "DEPOSIT DONATION", 250.00, "Donations", "Parent donation", "", false),
        ("2026-03-31", "INTEREST", 3.98, "Interest", "", "", false),
        ("2026-04-14", "CHQ#000213", -950.00, "Classroom grants", "Classroom grants", "Spring classroom grants", false),
        (
            "2026-05-16",
            "DEPOSIT SPRING CARNIVAL",
            2300.00,
            "Fundraising – Events",
            "Spring Carnival",
            "Carnival tickets and food",
            false,
        ),
        (
            "2026-05-14",
            "POS PURCHASE PARTY RENTALS LTD",
            -780.00,
            "Fundraising event costs",
            "Party Rentals Ltd",
            "Bouncy castle and games",
            true,
        ),
        (
            "2026-06-12",
            "POS PURCHASE SAVE ON FOODS #9",
            -650.75,
            "Staff appreciation",
            "Save-On-Foods",
            "Staff appreciation lunch",
            true,
        ),
        (
            "2026-06-15",
            "POS PURCHASE TIM HORTONS #2231",
            -88.30,
            "Hospitality and meetings",
            "Tim Hortons",
            "Coffee for AGM",
            true,
        ),
        ("2026-06-30", "INTEREST", 4.40, "Interest", "", "", false),
        ("2026-09-17", "BILL PAYMENT BCCPAC", -450.00, "Insurance", "BCCPAC", "Directors and officers insurance", true),
    ];
    // The event fundraisers' money in and costs, by date.
    let events = [
        ("2025-10-03", fall_fair),
        ("2025-10-05", fall_fair),
        ("2026-02-09", movie_night),
        ("2026-02-10", movie_night),
    ];
    let event_of = |date: &str| events.iter().find(|(d, _)| *d == date).map(|(_, f)| *f);
    let mut reimbursed = Vec::new();
    for (date, desc, amount, line, payee, purpose, receipt) in general_txs {
        let fundraiser =
            event_of(date).or((*payee == "Spring Carnival" || *payee == "Party Rentals Ltd").then_some(carnival));
        let id = demo
            .tx(
                g,
                "general",
                date,
                desc,
                *amount,
                &[(line, *amount)],
                Tx { payee, purpose, receipt: *receipt, fundraiser, ..Default::default() },
            )
            .await?;
        if line.starts_with("Gaming-eligible") {
            reimbursed.push(id);
        }
    }
    // Purdy's: last year's orders and bill; this year's first orders, before the bill.
    let purdy =
        |payee, purpose, receipt| Tx { payee, purpose, receipt, fundraiser: Some(purdys), ..Default::default() };
    demo.tx(
        g,
        "general",
        "2025-11-24",
        "DEPOSIT PURDYS ORDERS",
        3240.00,
        &[("Fundraising – Events", 3240.00)],
        purdy("Purdy's orders", "Holiday chocolate orders", false),
    )
    .await?;
    demo.tx(
        g,
        "general",
        "2025-12-01",
        "BILL PAYMENT PURDYS CHOCOLATIER",
        -2430.00,
        &[("Fundraising event costs", -2430.00)],
        purdy("Purdy's Chocolatier", "Holiday chocolate order", true),
    )
    .await?;
    demo.tx(
        g,
        "general",
        "2026-10-09",
        "E-TRANSFER RECEIVED PURDYS ORDERS",
        640.00,
        &[("Fundraising – Events", 640.00)],
        purdy("Purdy's orders", "Holiday chocolate orders, first week", false),
    )
    .await?;
    // One Costco trip for the carnival and the staff room: only one part is the carnival's.
    let costco = demo
        .tx(
            g,
            "general",
            "2026-05-12",
            "POS PURCHASE COSTCO WHOLESALE #54",
            -326.85,
            &[("Fundraising event costs", -214.60), ("Staff appreciation", -112.25)],
            Tx {
                payee: "Costco",
                purpose: "Carnival snacks and staff room coffee",
                receipt: true,
                ..Default::default()
            },
        )
        .await?;
    demo.part_fundraiser(costco, "Fundraising event costs", carnival).await?;
    // This year's fall fair.
    let fair =
        |payee, purpose, receipt| Tx { payee, purpose, receipt, fundraiser: Some(fall_fair), ..Default::default() };
    demo.tx(
        g,
        "general",
        "2026-10-01",
        "POS PURCHASE PARTY RENTALS LTD",
        -365.00,
        &[("Fundraising event costs", -365.00)],
        fair("Party Rentals Ltd", "Fall fair bouncy castle", true),
    )
    .await?;
    demo.tx(
        g,
        "general",
        "2026-10-05",
        "DEPOSIT FALL FAIR CASH",
        1975.50,
        &[("Fundraising – Events", 1975.50)],
        fair("Fall Fair", "Fall fair proceeds", false),
    )
    .await?;

    // Not yet assigned to a line item.
    demo.tx(g, "general", "2026-09-28", "E-TRANSFER RECEIVED A PARENT", 75.00, &[], Tx::default()).await?;
    demo.tx(g, "general", "2026-10-03", "POS PURCHASE COSTCO WHOLESALE #54", -210.44, &[], Tx::default()).await?;

    let gm = gaming;
    #[allow(clippy::type_complexity)]
    let gam: &[(&str, &str, f64, &str, &str, &str, &str, bool)] = &[
        // (date, description, amount, line item, payee, purpose, reference, receipt)
        ("2025-09-23", "DEPOSIT PROVINCE OF BC GAMING", 8240.00, "PAC gaming grant", "Province of BC", "", "", false),
        (
            "2025-09-30",
            "CHQ#000101",
            -75.00,
            "BCCPAC membership and conference",
            "BCCPAC",
            "2025–26 BCCPAC membership",
            "101",
            true,
        ),
        (
            "2025-10-16",
            "CHQ#000102",
            -1240.00,
            "Field trips (within BC)",
            "First Student Canada",
            "Bus for grade 4 trip to Science World",
            "102",
            true,
        ),
        (
            "2025-11-19",
            "CHQ#000103",
            -850.00,
            "Presentations and performances",
            "Science World on the Road",
            "Science assembly for all grades",
            "103",
            true,
        ),
        ("2025-12-31", "INTEREST", 3.21, "Interest and GST rebates", "", "", "", false),
        (
            "2026-02-11",
            "CHQ#000104",
            -1388.75,
            "Sports equipment and uniforms",
            "Sport Chek",
            "Basketballs, pinnies, and nets for lunch-hour sports",
            "104",
            true,
        ),
        (
            "2026-03-09",
            "CHQ#000105",
            -460.00,
            "Clubs and student competitions",
            "Robotics League",
            "Robotics club entry fees",
            "105",
            false,
        ),
        ("2026-03-31", "INTEREST", 2.87, "Interest and GST rebates", "", "", "", false),
        (
            "2026-04-22",
            "CHQ#000106",
            -1875.00,
            "Playground and beautification",
            "Playground Supply Co",
            "Playground balls and four-square court paint",
            "106",
            true,
        ),
        (
            "2026-05-20",
            "CHQ#000107",
            -1950.00,
            "Field trips (within BC)",
            "First Student Canada",
            "Buses for grade 6/7 trip to Fort Langley",
            "107",
            true,
        ),
        ("2026-06-10", "CHQ#000108", -545.32, "Graduation and student recognition", "Party City", "", "108", true),
        ("2026-06-30", "INTEREST", 2.40, "Interest and GST rebates", "", "", "", false),
        ("2026-09-22", "DEPOSIT PROVINCE OF BC GAMING", 8240.00, "PAC gaming grant", "Province of BC", "", "", false),
        (
            "2026-10-02",
            "CHQ#000109",
            -1300.00,
            "Field trips (within BC)",
            "First Student Canada",
            "Bus for grade 3 trip to the aquarium",
            "109",
            true,
        ),
    ];
    for (date, desc, amount, line, payee, purpose, reference, receipt) in gam {
        demo.tx(
            gm,
            "gaming",
            date,
            desc,
            *amount,
            &[(line, *amount)],
            Tx { payee, purpose, reference, receipt: *receipt, ..Default::default() },
        )
        .await?;
    }
    // The December reimbursement transfer, split across the two eligible costs.
    let transfer = demo
        .tx(
            gm,
            "gaming",
            "2025-12-15",
            "TRANSFER TO 1234",
            -500.00,
            &[("Sports equipment and uniforms", -312.40), ("Presentations and performances", -187.60)],
            Tx {
                payee: "PAC General Account",
                purpose: "Reimbursement: sports day ribbons and performer deposit (see summary)",
                reference: "EFT",
                ..Default::default()
            },
        )
        .await?;
    for id in reimbursed {
        sqlx::query("UPDATE transactions SET reimbursed_by_id = ? WHERE id = ?")
            .bind(transfer)
            .bind(id)
            .execute(pool)
            .await?;
    }
    // A licensed ticket raffle: revenue in, a prize and a printing cost out.
    let raffle = sqlx::query(
        "INSERT INTO gaming_events (name, licence_number, licence_class, event_type, start_date, end_date, notes, created_at)
         VALUES ('Spring raffle', 'D000000', 'D', 'ticket_raffle', ?, ?, 'Draw at the spring carnival.', ?)",
    )
    .bind(demo.date("2026-04-01"))
    .bind(demo.date("2026-05-15"))
    .bind(db::now())
    .execute(pool)
    .await?
    .last_insert_rowid();
    let ev = |payee, purpose, reference, receipt| Tx {
        payee,
        purpose,
        reference,
        receipt,
        event: Some(raffle),
        ..Default::default()
    };
    demo.tx(
        gm,
        "gaming",
        "2026-03-25",
        "CHQ#000111",
        -85.50,
        &[("Licensed gaming event expenses", -85.50)],
        ev("Print Shop", "Raffle tickets", "111", true),
    )
    .await?;
    demo.tx(
        gm,
        "gaming",
        "2026-05-19",
        "DEPOSIT RAFFLE PROCEEDS",
        1850.00,
        &[("Licensed gaming event revenue", 1850.00)],
        ev("Spring raffle ticket sales", "", "", false),
    )
    .await?;
    demo.tx(
        gm,
        "gaming",
        "2026-05-20",
        "CHQ#000112",
        -600.00,
        &[("Licensed gaming event prizes", -600.00)],
        ev("Raffle winner", "Grand prize: bicycle", "112", true),
    )
    .await?;

    // A cheque written but not cashed yet.
    demo.tx(
        gm,
        "gaming",
        "2026-10-08",
        "Mad Science of Vancouver",
        -620.00,
        &[("Presentations and performances", -620.00)],
        Tx {
            payee: "Mad Science of Vancouver",
            purpose: "Science assembly, grades K to 3",
            reference: "110",
            receipt: true,
            cleared: Some(false),
            ..Default::default()
        },
    )
    .await?;

    for (pattern, line, fundraiser) in [
        ("SERVICE CHARGE", "Bank fees", None),
        ("MUNCH A LUNCH", "Fundraising – Hot lunch", Some(hot_lunch)),
        ("FRESH KITCHEN", "Hot lunch costs", Some(hot_lunch)),
        ("PURDYS ORDERS", "Fundraising – Events", Some(purdys)),
        ("PURDYS CHOCOLATIER", "Fundraising event costs", Some(purdys)),
    ] {
        let li = demo.line("general", line).await?;
        sqlx::query("INSERT INTO rules (pattern, line_item_id, fundraiser_id) VALUES (?, ?, ?)")
            .bind(pattern)
            .bind(li)
            .bind(fundraiser)
            .execute(pool)
            .await?;
    }
    // The example PAC meets on the 2nd Wednesday, as many PACs do.
    crate::meetings::Rule { week: 2, weekday: chrono::Weekday::Wed, skip_months: vec![7, 8] }.save(pool).await?;
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, '1')").bind(DEMO_MARKER).execute(pool).await?;
    Ok(())
}

/// The public sign-in for a demo server. Anyone may use it.
pub const DEMO_USERNAME: &str = "demo";
pub const DEMO_PASSWORD: &str = "packeeper";

/// Set in the settings of a database that holds only example data.
const DEMO_MARKER: &str = "demo_data";

/// True when the folder holds example data, or nothing yet. Only such a
/// folder may be reset: a reset deletes every record.
pub async fn is_demo_folder(pool: &SqlitePool) -> anyhow::Result<bool> {
    let marked: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(DEMO_MARKER).fetch_optional(pool).await?;
    if marked.as_deref() == Some("1") {
        return Ok(true);
    }
    let records: i64 =
        sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM accounts) + (SELECT COUNT(*) FROM transactions)")
            .fetch_one(pool)
            .await?;
    Ok(records == 0)
}

/// Deletes every record and file, then loads the example PAC and the public
/// demo sign-in again. Refuses to touch a folder with real records.
pub async fn reset(pool: &SqlitePool, data_dir: &std::path::Path) -> anyhow::Result<()> {
    if !is_demo_folder(pool).await? {
        anyhow::bail!(
            "{} holds real records. A demo server resets its data folder, so it only runs on a demo folder.",
            data_dir.display()
        );
    }
    // Children first, for the foreign keys.
    for table in [
        "lunch_order_lines",
        "lunch_orders",
        "lunch_children",
        "lunch_day_items",
        "lunch_days",
        "lunch_periods",
        "lunch_items",
        "lunch_divisions",
        "allocations",
        "attachments",
        "rules",
        "budget_amounts",
        "fundraiser_goals",
        "meetings",
        "transactions",
        "import_batches",
        "gaming_events",
        "fundraisers",
        "line_items",
        "fiscal_years",
        "accounts",
        "settings",
        "users",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}"))).execute(pool).await?;
    }
    for dir in ["receipts", "imports", "backups"] {
        let path = data_dir.join(dir);
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
    }
    db::seed(pool).await?;
    load(pool, data_dir).await?;
    crate::web::add_user(pool, DEMO_USERNAME, "Demo treasurer", DEMO_PASSWORD, "treasurer").await?;
    Ok(())
}

/// A small bank statement for the demo's "sample statement" import. Dates are
/// a few days before `today`, so a second import shows the rows as duplicates.
pub fn sample_statement(today: NaiveDate) -> String {
    let rows = [
        (9, "-182.47", "POS PURCHASE COSTCO WHOLESALE #54"),
        (7, "1065.00", "DEPOSIT MUNCH A LUNCH PAYOUT"),
        (4, "40.00", "E-TRANSFER RECEIVED A PARENT"),
        (2, "-5.00", "SERVICE CHARGE"),
    ];
    let mut trn = String::new();
    for (days, amount, name) in rows {
        let date = today - chrono::Days::new(days);
        trn.push_str(&format!(
            "<STMTTRN><TRNTYPE>{}<DTPOSTED>{}<TRNAMT>{amount}<FITID>SAMPLE-{}<NAME>{name}</STMTTRN>\n",
            if amount.starts_with('-') { "DEBIT" } else { "CREDIT" },
            date.format("%Y%m%d"),
            date.format("%Y%m%d"),
        ));
    }
    format!(
        "OFXHEADER:100\nDATA:OFXSGML\nVERSION:102\n\n<OFX><BANKMSGSRSV1><STMTTRNRS><STMTRS><CURDEF>CAD\n\
         <BANKACCTFROM><BANKID>0000<ACCTID>00001234<ACCTTYPE>CHECKING</BANKACCTFROM>\n<BANKTRANLIST>\n{trn}</BANKTRANLIST>\n\
         </STMTRS></STMTTRNRS></BANKMSGSRSV1></OFX>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn year_shift() {
        let d = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(years_since_written(d("2026-10-12")), 0);
        assert_eq!(years_since_written(d("2027-10-11")), 0);
        assert_eq!(years_since_written(d("2027-10-12")), 1);
        assert_eq!(years_since_written(d("2029-01-01")), 2);
    }

    #[test]
    fn sample_statement_parses() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        let st = crate::import::ofx::parse(sample_statement(today).as_bytes()).unwrap();
        assert_eq!(st.rows.len(), 4);
        assert_eq!(st.rows[0].amount_cents, -18247);
        assert_eq!(st.account_id, "00001234");
    }
}
