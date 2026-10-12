use askama::Template;
use axum::extract::{Multipart, Path as UrlPath, State};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::{Form, Query};
use serde::Deserialize;

use super::{
    AppError, AppResult, AppState, Flash, Layout, UserExt, no_uploads_in_demo, redirect_err, redirect_ok, render,
    require_edit,
};
use crate::db;
use crate::import::csv::{self as csvimp, CsvMapping, DATE_FORMATS, RowError, Table};
use crate::import::{self, BankRow, RowStatus, ofx};
use crate::models::{Account, ImportBatch};
use crate::money::Money;

#[derive(Template)]
#[template(path = "import.html")]
struct ImportPage {
    layout: Layout,
    accounts: Vec<Account>,
    batches: Vec<(ImportBatch, String)>,
}

pub async fn page(State(state): State<AppState>, user: UserExt, Query(flash): Query<Flash>) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let batches: Vec<ImportBatch> =
        sqlx::query_as("SELECT * FROM import_batches WHERE status = 'done' ORDER BY id DESC LIMIT 30")
            .fetch_all(pool)
            .await?;
    let accounts = db::accounts(pool).await?;
    let batches = batches
        .into_iter()
        .map(|b| {
            let name = accounts.iter().find(|a| a.id == b.account_id).map(|a| a.name.clone()).unwrap_or_default();
            (b, name)
        })
        .collect();
    render(ImportPage {
        layout: Layout::new(&state, &user, "import", "Import", &flash).await?,
        accounts: accounts.into_iter().filter(|a| a.active).collect(),
        batches,
    })
}

pub async fn upload(State(state): State<AppState>, user: UserExt, mut multipart: Multipart) -> AppResult<Response> {
    require_edit(&user)?;
    if let Some(refused) = no_uploads_in_demo(&state, "/import") {
        return Ok(refused);
    }
    let mut account_id = None;
    let mut file: Option<(String, Vec<u8>)> = None;
    while let Some(field) = multipart.next_field().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
        match field.name() {
            Some("account_id") => account_id = field.text().await.ok().and_then(|t| t.parse::<i64>().ok()),
            Some("file") => {
                let name = field.file_name().unwrap_or("upload").to_string();
                let bytes = field.bytes().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
                if !bytes.is_empty() {
                    file = Some((name, bytes.to_vec()));
                }
            }
            _ => {}
        }
    }
    let Some(account_id) = account_id else {
        return Ok(redirect_err("/import", "Choose the account this file is for."));
    };
    let Some((filename, bytes)) = file else {
        return Ok(redirect_err("/import", "Choose a file to import."));
    };
    start_batch(&state, account_id, &filename, &bytes).await
}

/// On a public demo: import a small built-in statement into the general account.
pub async fn sample(State(state): State<AppState>, user: UserExt) -> AppResult<Response> {
    require_edit(&user)?;
    if state.demo.is_none() {
        return Err(AppError::NotFound);
    }
    let account_id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM accounts WHERE kind = 'general' AND active = 1 ORDER BY id LIMIT 1")
            .fetch_optional(&state.pool)
            .await?;
    let Some(account_id) = account_id else {
        return Ok(redirect_err("/import", "The demo has no general account."));
    };
    let statement = crate::demo::sample_statement(chrono::Local::now().date_naive());
    start_batch(&state, account_id, "sample-statement.qfx", statement.as_bytes()).await
}

/// Saves an uploaded bank file and opens its preview.
async fn start_batch(state: &AppState, account_id: i64, filename: &str, bytes: &[u8]) -> AppResult<Response> {
    let format = if ofx::looks_like_ofx(bytes) { "ofx" } else { "csv" };
    let check = if format == "ofx" { ofx::parse(bytes).map(|_| ()) } else { csvimp::read(bytes).map(|_| ()) };
    if let Err(e) = check {
        return Ok(redirect_err("/import", &format!("Cannot read {filename}: {e}")));
    }
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let safe: String =
        filename.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    let stored_name = format!("{stamp}-{safe}");
    let dir = state.data_dir.join("imports");
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::write(dir.join(&stored_name), bytes).await?;
    let id = sqlx::query(
        "INSERT INTO import_batches (account_id, filename, stored_name, format, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(account_id)
    .bind(filename)
    .bind(&stored_name)
    .bind(format)
    .bind(db::now())
    .execute(&state.pool)
    .await?
    .last_insert_rowid();
    Ok(axum::response::Redirect::to(&format!("/import/{id}")).into_response())
}

/// The mapping form. Every field is a string so that blank selects work.
#[derive(Debug, Default, Deserialize)]
pub struct MappingForm {
    account_id: Option<i64>,
    skip_rows: Option<String>,
    has_header: Option<String>,
    date_col: Option<String>,
    date_format: Option<String>,
    #[serde(default)]
    description_cols: Vec<String>,
    amount_mode: Option<String>,
    amount_col: Option<String>,
    debit_col: Option<String>,
    credit_col: Option<String>,
    balance_col: Option<String>,
    reference_col: Option<String>,
    invert_amount: Option<String>,
}

fn col(v: &Option<String>) -> Option<usize> {
    v.as_deref().and_then(|s| s.trim().parse().ok())
}

impl MappingForm {
    fn mapping(&self) -> CsvMapping {
        let split = self.amount_mode.as_deref() == Some("split");
        CsvMapping {
            skip_rows: col(&self.skip_rows).unwrap_or(0),
            has_header: self.has_header.is_some(),
            date_col: col(&self.date_col),
            date_format: self.date_format.clone().unwrap_or_else(|| "%Y-%m-%d".into()),
            description_cols: self.description_cols.iter().filter_map(|s| s.parse().ok()).collect(),
            amount_col: if split { None } else { col(&self.amount_col) },
            debit_col: if split { col(&self.debit_col) } else { None },
            credit_col: if split { col(&self.credit_col) } else { None },
            balance_col: col(&self.balance_col),
            reference_col: col(&self.reference_col),
            invert_amount: !split && self.invert_amount.is_some(),
        }
    }
}

struct Parsed {
    rows: Vec<BankRow>,
    errors: Vec<RowError>,
    table: Option<Table>,
    mapping: Option<CsvMapping>,
    ofx_account: Option<String>,
    ofx_balance: Option<(chrono::NaiveDate, Money)>,
}

async fn parse_batch(
    state: &AppState,
    batch: &ImportBatch,
    account: &Account,
    form: Option<&MappingForm>,
) -> AppResult<Parsed> {
    let bytes = tokio::fs::read(state.data_dir.join("imports").join(&batch.stored_name)).await?;
    if batch.format == "ofx" {
        let st = ofx::parse(&bytes)?;
        return Ok(Parsed {
            rows: st.rows,
            errors: vec![],
            table: None,
            mapping: None,
            ofx_account: Some(st.account_id).filter(|s| !s.is_empty()),
            ofx_balance: st.ledger_balance_date.zip(st.ledger_balance_cents.map(Money)),
        });
    }
    let table = csvimp::read(&bytes)?;
    let mapping = match form {
        Some(f) => f.mapping(),
        None => account
            .csv_mapping
            .as_deref()
            .and_then(|j| serde_json::from_str::<CsvMapping>(j).ok())
            .filter(|m| fits(m, &table))
            .unwrap_or_else(|| csvimp::detect(&table)),
    };
    let (rows, errors) = csvimp::parse_rows(&table, &mapping);
    Ok(Parsed { rows, errors, table: Some(table), mapping: Some(mapping), ofx_account: None, ofx_balance: None })
}

/// A saved mapping fits a file when every mapped column exists and dates parse.
fn fits(m: &CsvMapping, table: &Table) -> bool {
    let width = table.width();
    let cols = [m.date_col, m.amount_col, m.debit_col, m.credit_col, m.balance_col, m.reference_col];
    if cols.iter().flatten().any(|c| *c >= width) || m.description_cols.iter().any(|c| *c >= width) {
        return false;
    }
    let (rows, errors) = csvimp::parse_rows(table, m);
    !rows.is_empty() && errors.len() * 5 < rows.len().max(1)
}

/// If running balances are present, check that the amounts move them. When the
/// amounts move them the wrong way, the sign is probably inverted.
fn balance_hint(rows: &[BankRow]) -> Option<String> {
    let pairs: Vec<(&BankRow, &BankRow)> = rows
        .windows(2)
        .filter_map(|w| match (w[0].balance_cents, w[1].balance_cents) {
            (Some(_), Some(_)) => Some((&w[0], &w[1])),
            _ => None,
        })
        .collect();
    if pairs.len() < 2 {
        return None;
    }
    let mut ok = 0;
    let mut inverted = 0;
    for (a, b) in &pairs {
        let (ab, bb) = (a.balance_cents.unwrap(), b.balance_cents.unwrap());
        // Oldest first: b = a + b.amount. Newest first: a = b + a.amount.
        if bb - ab == b.amount_cents || ab - bb == a.amount_cents {
            ok += 1;
        } else if bb - ab == -b.amount_cents || ab - bb == -a.amount_cents {
            inverted += 1;
        }
    }
    if inverted > ok {
        Some("The running balance moves the other way from the amounts. The amount sign is probably reversed: tick \"Flip the sign\".".into())
    } else if ok * 2 < pairs.len() {
        Some("The running balance does not follow the amounts. Check the amount and balance columns.".into())
    } else {
        None
    }
}

#[derive(Template)]
#[template(path = "import_preview.html")]
struct PreviewPage {
    layout: Layout,
    batch: ImportBatch,
    account: Account,
    accounts: Vec<Account>,
    t: PreviewTable,
}

/// The part of the preview page that htmx replaces when a column choice changes.
#[derive(Template)]
#[template(path = "partials/import_body.html")]
struct PreviewTable {
    is_csv: bool,
    mapping: CsvMapping,
    date_formats: Vec<(&'static str, &'static str)>,
    raw: Vec<Vec<String>>,
    columns: Vec<String>,
    rows: Vec<(BankRow, RowStatus)>,
    errors: Vec<RowError>,
    new: usize,
    duplicate: usize,
    matched: usize,
    before_opening: usize,
    hints: Vec<String>,
    notes: Vec<String>,
    total_in: Money,
    total_out: Money,
}

impl PreviewTable {
    /// A column choice. Column names come from the file, so they are escaped.
    fn col_select(&self, name: &str, current: &Option<usize>) -> String {
        let mut html = format!("<select name=\"{name}\"><option value=\"\">— none —</option>");
        for (i, c) in self.columns.iter().enumerate() {
            let selected = if *current == Some(i) { " selected" } else { "" };
            html.push_str(&format!("<option value=\"{i}\"{selected}>{}</option>", escape(c)));
        }
        html.push_str("</select>");
        html
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

async fn build_table(state: &AppState, account: &Account, p: &Parsed) -> AppResult<PreviewTable> {
    let statuses = import::classify(&state.pool, account, &p.rows).await?;
    let count = |s: fn(&RowStatus) -> bool| statuses.iter().filter(|x| s(x)).count();
    let mut hints = Vec::new();
    if let Some(h) = balance_hint(&p.rows) {
        hints.push(h);
    }
    if let Some(acct) = &p.ofx_account
        && !account.account_number.is_empty()
        && !acct.ends_with(account.account_number.trim())
    {
        hints.push(format!(
            "This file is for account {acct}, but {} has the number {}. Check that you chose the right account.",
            account.name, account.account_number
        ));
    }
    let mut notes = Vec::new();
    if let Some((d, b)) = p.ofx_balance {
        notes.push(format!(
            "The bank reports a balance of {b} on {d}. The app records it to check the books against the bank."
        ));
    }
    let (columns, raw) = match (&p.table, &p.mapping) {
        (Some(t), Some(m)) => (t.column_names(m), t.data_rows(m).iter().take(4).cloned().collect()),
        _ => (vec![], vec![]),
    };
    Ok(PreviewTable {
        is_csv: p.table.is_some(),
        mapping: p.mapping.clone().unwrap_or_default(),
        date_formats: DATE_FORMATS.to_vec(),
        raw,
        columns,
        new: count(|s| *s == RowStatus::New),
        duplicate: count(|s| *s == RowStatus::Duplicate),
        matched: count(|s| matches!(s, RowStatus::MatchesManual(_))),
        before_opening: count(|s| *s == RowStatus::BeforeOpening),
        total_in: Money(p.rows.iter().filter(|r| r.amount_cents > 0).map(|r| r.amount_cents).sum()),
        total_out: Money(p.rows.iter().filter(|r| r.amount_cents < 0).map(|r| r.amount_cents).sum()),
        rows: p.rows.iter().cloned().zip(statuses).collect(),
        errors: p.errors.clone(),
        hints,
        notes,
    })
}

async fn load(state: &AppState, id: i64) -> AppResult<ImportBatch> {
    sqlx::query_as("SELECT * FROM import_batches WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)
}

#[derive(Template)]
#[template(path = "import_done.html")]
struct DonePage {
    layout: Layout,
    batch: ImportBatch,
    account: Account,
}

pub async fn preview(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Query(flash): Query<Flash>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let batch = load(&state, id).await?;
    let account = db::account(&state.pool, batch.account_id).await?.ok_or(AppError::NotFound)?;
    if batch.status == "done" {
        return render(DonePage {
            layout: Layout::new(&state, &user, "import", "Import", &flash).await?,
            batch,
            account,
        });
    }
    let p = parse_batch(&state, &batch, &account, None).await?;
    let t = build_table(&state, &account, &p).await?;
    render(PreviewPage {
        layout: Layout::new(&state, &user, "import", "Check the import", &flash).await?,
        accounts: db::accounts(&state.pool).await?.into_iter().filter(|a| a.active).collect(),
        batch,
        account,
        t,
    })
}

/// htmx: re-parse with the changed mapping and return the preview table.
pub async fn preview_table(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(form): Form<MappingForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let batch = load(&state, id).await?;
    let account_id = form.account_id.unwrap_or(batch.account_id);
    let account = db::account(&state.pool, account_id).await?.ok_or(AppError::NotFound)?;
    let p = parse_batch(&state, &batch, &account, (batch.format == "csv").then_some(&form)).await?;
    render(build_table(&state, &account, &p).await?)
}

pub async fn commit(
    State(state): State<AppState>,
    user: UserExt,
    UrlPath(id): UrlPath<i64>,
    Form(form): Form<MappingForm>,
) -> AppResult<Response> {
    require_edit(&user)?;
    let batch = load(&state, id).await?;
    if batch.status == "done" {
        return Ok(redirect_err(&format!("/import/{id}"), "This file is already imported."));
    }
    let account_id = form.account_id.unwrap_or(batch.account_id);
    let account = db::account(&state.pool, account_id).await?.ok_or(AppError::NotFound)?;
    let p = parse_batch(&state, &batch, &account, (batch.format == "csv").then_some(&form)).await?;
    if p.rows.is_empty() {
        return Ok(redirect_err(&format!("/import/{id}"), "No rows could be read. Check the column choices."));
    }
    sqlx::query("UPDATE import_batches SET account_id = ? WHERE id = ?")
        .bind(account.id)
        .bind(id)
        .execute(&state.pool)
        .await?;
    if let Some(m) = &p.mapping {
        sqlx::query("UPDATE accounts SET csv_mapping = ? WHERE id = ?")
            .bind(serde_json::to_string(m)?)
            .bind(account.id)
            .execute(&state.pool)
            .await?;
    }
    let r = import::apply(&state.pool, &account, id, &p.rows, p.ofx_balance.map(|(d, b)| (d, b.cents()))).await?;
    let mut msg = format!("Imported {} new transaction(s)", r.new);
    if r.auto_assigned > 0 {
        msg.push_str(&format!(", {} assigned by rules", r.auto_assigned));
    }
    if r.matched > 0 {
        msg.push_str(&format!(", {} matched to entries made by hand", r.matched));
    }
    if r.duplicate > 0 {
        msg.push_str(&format!(". {} were already imported", r.duplicate));
    }
    if r.before_opening > 0 {
        msg.push_str(&format!(". {} were before the opening balance date and were skipped", r.before_opening));
    }
    msg.push('.');
    Ok(redirect_ok(&format!("/import/{id}"), &msg))
}

/// Removes the transactions a batch created. Entries made by hand that the
/// batch matched go back to "not cleared".
pub async fn undo(State(state): State<AppState>, user: UserExt, UrlPath(id): UrlPath<i64>) -> AppResult<Response> {
    require_edit(&user)?;
    let pool = &state.pool;
    let files: Vec<String> = sqlx::query_scalar(
        "SELECT stored_name FROM attachments WHERE transaction_id IN
           (SELECT id FROM transactions WHERE import_batch_id = ? AND source = 'import')",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE transactions SET reimbursed_by_id = NULL WHERE reimbursed_by_id IN (SELECT id FROM transactions WHERE import_batch_id = ? AND source = 'import')")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let removed = sqlx::query("DELETE FROM transactions WHERE import_batch_id = ? AND source = 'import'")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    sqlx::query("UPDATE transactions SET cleared = 0, bank_ref = NULL, bank_balance_cents = NULL, import_batch_id = NULL WHERE import_batch_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM import_batches WHERE id = ?").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    for f in files {
        let _ = tokio::fs::remove_file(state.data_dir.join("receipts").join(f)).await;
    }
    Ok(redirect_ok("/import", &format!("Import undone. {removed} transaction(s) removed.")))
}
