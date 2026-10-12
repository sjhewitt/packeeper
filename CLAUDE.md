# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project

**PAC Keeper** (<https://packeeper.ca>; the binary is `pac-keeper`): bookkeeping and PDF reports for a BC school Parent Advisory Committee. Rust, axum, askama templates, htmx, SQLite through sqlx, and Typst built in for PDFs. One binary plus a data folder. It runs locally with no sign-in; `--auth` turns on sign-in for a hosted install.

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
- **GASR lines come from line items.** `line_items.gasr_line` holds a code from `models::GASR_INCOME_LINES` or `GASR_EXPENSE_LINES`. Codes `6`, `prize`, and `event_expense` also need `transactions.gaming_event_id`, which picks the licence row (lines 6–13) or the bingo / ticket raffle / other split (lines 19–26). Validate codes with `models::valid_gasr_line`.
- **New starter line items** go in `db::seed` behind a new `SEED_VERSION`, so older databases get them once. Never seed line items in a migration: it stops the full seed on a new database.
- **Three ways to run:** a single-PAC folder (local, `--auth`, or `--demo`), or a hosted root (`--hosted`) with `control.db` and `pacs/<folder>/pac.db`. Handlers take `web::Ctx` (the request's PAC database and folder), never `AppState` directly. The guard in `web::auth` builds `Ctx` for single servers; `web::hosted::guard` builds it from the session's PAC and membership. Never add a route that reads PAC records without `Ctx`, and never fall back to another PAC.
- **Hosted people and PACs live in `control.db`** (`migrations_control/`). PAC records never go there. One-time links store only a token hash. A PAC always keeps at least one treasurer.
- **No user data inside inline JavaScript.** Put it in a `data-` attribute and read it from script (see `hosted/operator.html`).
- **Demo mode** (`serve --demo`) wipes and reloads the data folder on a schedule. `demo::reset` refuses any folder that is not marked `demo_data` or empty. Keep that guard. Uploads are off in demo mode (`web::no_uploads_in_demo`); any new upload route needs the same check.
- **Demo dates shift** by whole years from `demo::WRITTEN_ON`. Tests call `demo::load_shifted(.., 0)` so their fixed dates hold.
- **`site/`** is the static marketing site. Its claims must match what the app does; check them when features change.
- **Never edit an applied migration.** sqlx checks migration checksums. Add a new numbered file.
- **Dates** are ISO text in SQLite and `chrono::NaiveDate` in Rust. Fiscal year dates are never hard-coded.

## Library quirks

- askama passes call arguments by reference. Template helper methods take `&T` (for example `fn is_line(&self, id: &i64)`). Do not write `Some(x.field)` or `Money(x)` in templates; add a method instead. For loops yield references, so use `*flag` for a `bool` from a tuple.
- sqlx 0.9 rejects `format!` SQL. Wrap it in `AssertSqlSafe(...)` only when every interpolated part is a constant.
- Forms and queries use `axum_extra::extract::{Form, Query}` (serde_html_form), so repeated keys become `Vec` and blank values become `None`.
- Typst compiles in `spawn_blocking`. The `Renderer` is built once at start-up and shared.

## Style

Comments and user-facing text use Simplified Technical English: short sentences, active voice, one word for one meaning.
