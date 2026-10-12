# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project

Bookkeeping and PDF reports for a BC school Parent Advisory Committee. Rust, axum, askama templates, htmx, SQLite through sqlx, and Typst built in for PDFs. One binary plus a data folder. It runs locally with no sign-in; `--auth` turns on sign-in for a hosted install.

## Commands

```sh
cargo run -- --data-dir demo-data demo     # load the example PAC into an empty folder
cargo run -- --data-dir demo-data          # serve on 127.0.0.1:8080
cargo run -- --data-dir demo-data pdf monthly 2026-09 -o /tmp/m.pdf
cargo test                                 # includes report balance checks on the demo data
cargo clippy --all-targets && cargo fmt
```

## Layout

- `migrations/` — SQLite schema, applied at start-up by `sqlx::migrate!`.
- `src/models.rs` — row structs (`FromRow`). `src/db.rs` — pool, `Settings`, seed line items, account balances.
- `src/fiscal.rs` — fiscal years are rows; the start month is a setting. `fiscal::ensure` creates a missing year.
- `src/ledger.rs` — transaction list query (`TxRow`), allocations (splits), counts.
- `src/import/` — `csv.rs` (column mapping, detection), `ofx.rs` (SGML and XML OFX), `mod.rs` (duplicates, manual-entry matching, rules).
- `src/reports/` — `mod.rs` (budget vs actual, fund and account summaries), `gaming.rs` (GASR and grant-condition checks), `proposal.rs`, `pdf.rs` (JSON for Typst).
- `typst/` — report templates. `lib.typ` has shared styles and `fin-table`. Data arrives as JSON in `sys.inputs.data`, money pre-formatted.
- `src/web/` — handlers, one module per area. `templates/` — askama HTML. `static/` — CSS and vendored htmx, embedded with `include_str!`.
- `src/demo.rs` — example data; also the fixture for the report tests.

## Rules that matter

- **Money is `i64` cents** (`Money`). Negative amounts are money out. Never use floats except in `demo.rs` literals.
- **Funds stay apart.** An account's `kind` (general or gaming) is its fund. `ledger::set_allocations` refuses a line item from the other fund. Keep that check on every new write path.
- **Reports must balance:** opening + accounts opened + income − expenses + transfers = closing, per fund. Uncategorized amounts show as their own line so the totals still balance. `reports::tests` checks this; add cases there when report logic changes.
- **Opening balances:** an account is not tracked before its `opening_date`. `db::balance_at` is end of day; `db::balance_before` is start of day.
- **The gaming report is bank basis:** it counts only `cleared = 1` transactions.
- **Dates** are ISO text in SQLite and `chrono::NaiveDate` in Rust. Fiscal year dates are never hard-coded.

## Library quirks

- askama passes call arguments by reference. Template helper methods take `&T` (for example `fn is_line(&self, id: &i64)`). Do not write `Some(x.field)` or `Money(x)` in templates; add a method instead. For loops yield references, so use `*flag` for a `bool` from a tuple.
- sqlx 0.9 rejects `format!` SQL. Wrap it in `AssertSqlSafe(...)` only when every interpolated part is a constant.
- Forms and queries use `axum_extra::extract::{Form, Query}` (serde_html_form), so repeated keys become `Vec` and blank values become `None`.
- Typst compiles in `spawn_blocking`. The `Renderer` is built once at start-up and shared.

## Style

Comments and user-facing text use Simplified Technical English: short sentences, active voice, one word for one meaning.
