# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project

**PAC Keeper** (<https://packeeper.ca>; the binary is `pac-keeper`): bookkeeping and PDF reports for a BC school Parent Advisory Committee. Rust, axum, askama templates, htmx, SQLite through sqlx, and Typst built in for PDFs. One binary plus a data folder. It runs locally with no sign-in; `--auth` turns on sign-in for a hosted install. `desktop/` wraps the same server in a Tauri 2 app for macOS and Windows.

## Commands

```sh
cargo run -- --data-dir demo-data demo     # load the example PAC into an empty folder
cargo run -- --data-dir demo-data          # serve on 127.0.0.1:8080
cargo run -- --data-dir demo-data pdf monthly 2026-09 -o /tmp/m.pdf
cargo test                                 # includes report balance checks on the demo data
cargo clippy --all-targets && cargo fmt
cd desktop && npm ci && npm run tauri dev  # the desktop app; set PAC_DATA_DIR to a scratch folder
```

## Layout

- `migrations/` — SQLite schema, applied at start-up by `sqlx::migrate!`.
- `src/models.rs` — row structs (`FromRow`). `src/db.rs` — pool, `Settings`, seed line items, account balances.
- `src/fiscal.rs` — fiscal years are rows; the start month is a setting. `fiscal::ensure` creates a missing year.
- `src/ledger.rs` — transaction list query (`TxRow`), allocations (splits), counts.
- `src/import/` — `csv.rs` (column mapping, detection), `ofx.rs` (SGML and XML OFX), `mod.rs` (duplicates, manual-entry matching, rules).
- `src/reports/` — `mod.rs` (budget vs actual, fund and account summaries), `gaming.rs` (GASR and grant-condition checks), `fundraisers.rs` (status per fiscal year, goals, history), `proposal.rs`, `pdf.rs` (JSON for Typst).
- `typst/` — report templates. `lib.typ` has shared styles and `fin-table`. Data arrives as JSON in `sys.inputs.data`, money pre-formatted.
- `src/web/` — handlers, one module per area. `templates/` — askama HTML. `static/` — CSS and vendored htmx, embedded with `include_str!`.
- `src/demo.rs` — example data; also the fixture for the report tests.
- `src/meetings.rs` — meeting dates (a rule fills each fiscal year once; then single dates change) and the report period of each meeting: from the previous meeting to the day before, never before the fiscal year starts. `reports::pdf::ReportPeriod` gives the treasurer's report (`monthly.typ`) its dates and words, for a month or a meeting.
- `src/lib.rs` holds every module; `src/main.rs` is only the command line. `src/server.rs` starts each kind of server, including `start_desktop`.
- `desktop/` — the Tauri app (its own crate and `Cargo.lock`, sharing `../target`). `desktop/src/main.rs` starts the server and the window, and routes navigation: file addresses (`server::is_file_path`) are fetched in-process and handed to the computer, and outside links open in the browser. `desktop/dist/` is only the start-up error page.
- `static/app.js` — the `data-confirm` dialog for forms.
- `src/lunch/` — hot lunch (hosted, behind `--hot-lunch`): `mod.rs` (menu, periods, days, children, orders, counts), `fees.rs` (what a card payment costs, and the platform's application fee), `stripe.rs` (Checkout, Connect accounts, refunds, webhook signatures; HTTP by `ureq` in a blocking task), `helcim.rs` (a PAC's own Helcim account: HelcimPay.js checkouts, transactions by invoice number, refunds, the sealed API token). `src/web/lunch.rs` is the PAC's side (`/lunch`), `src/web/family.rs` the parents' side (`/family`, `/join/{code}`). `migrations/0004_hot_lunch.sql` holds the PAC records; `migrations_control/0002_families_and_payments.sql` holds parent links, sign-up codes, and each PAC's Stripe account; `0004_helcim.sql` holds each PAC's sealed Helcim token.
- `deploy/opalstack/` — start and stop scripts for an Opalstack "Proxied Port" app, and `upgrade`, which installs a release and restarts every app (with a backup first and a rollback if a server does not answer). `.github/workflows/release.yml` packs them with the static Linux (musl) build. `.github/workflows/deploy.yml` deploys a published release to Opalstack over SSH (secret `OPALSTACK_SSH_KEY`). The start and stop scripts run with `set -o pipefail`: never pipe into `grep -q` there.

## Rules that matter

- **Money is `i64` cents** (`Money`). Negative amounts are money out. Never use floats except in `demo.rs` literals.
- **Funds stay apart.** An account's `kind` (general or gaming) is its fund. `ledger::set_allocations` refuses a line item from the other fund. Keep that check on every new write path.
- **Reports must balance:** opening + accounts opened + income − expenses + transfers = closing, per fund. Uncategorized amounts show as their own line so the totals still balance. `reports::tests` checks this; add cases there when report logic changes.
- **Opening balances:** an account is not tracked before its `opening_date`. `db::balance_at` is end of day; `db::balance_before` is start of day.
- **The gaming report is bank basis:** it counts only `cleared = 1` transactions.
- **GASR lines come from line items.** `line_items.gasr_line` holds a code from `models::GASR_INCOME_LINES` or `GASR_EXPENSE_LINES`. Codes `6`, `prize`, and `event_expense` also need `transactions.gaming_event_id`, which picks the licence row (lines 6–13) or the bingo / ticket raffle / other split (lines 19–26). Validate codes with `models::valid_gasr_line`.
- **Fundraisers tag allocations** (`allocations.fundraiser_id`), not transactions, so each part of a split can have its own. They are not tied to a year: `reports::fundraisers` counts parts by transaction date, and goals are per fiscal year (`fundraiser_goals`). They are general-fund only; `ledger::set_allocations` refuses a fundraiser on a gaming account. Every write of allocations goes through `ledger::Part`; keep the fundraiser (and memo) when a handler rebuilds a single part, as `transactions::assign` and `transactions::set_fundraiser` do.
- **A line item can name a fundraiser** (`line_items.fundraiser_id`, general fund only). `ledger::set_allocations` gives a part with no fundraiser its line item's fundraiser, so "no fundraiser" on such a line means the line item's. `import::apply` writes allocations directly and does the same. `transactions::assign` drops a fundraiser that came from the old line item. `ledger::give_line_fundraiser` tags the existing parts when a line item gets a fundraiser.
- **New starter line items** go in `db::seed` behind a new `SEED_VERSION`, so older databases get them once. Never seed line items in a migration: it stops the full seed on a new database.
- **Ways to run:** a single-PAC folder (local, `--auth`, or `--demo`), the desktop app (a single-PAC folder behind the desktop checks), or a hosted root (`--hosted`) with `control.db` and `pacs/<folder>/pac.db`. Handlers take `web::Ctx` (the request's PAC database and folder), never `AppState` directly. The guard in `web::auth` builds `Ctx` for single servers; `web::hosted::guard` builds it from the session's PAC and membership. Never add a route that reads PAC records without `Ctx`, and never fall back to another PAC.
- **Hosted people and PACs live in `control.db`** (`migrations_control/`). PAC records never go there. One-time links store only a token hash. A PAC always keeps at least one treasurer.
- **Visit counts save counts only** (`hosted::stats`, `/operator/stats`, operators only). Never save an address, a browser, or an app path (paths can hold tokens). Demo pages are the only paths kept, with numbers replaced by `{id}`.
- **The desktop server answers only its own window.** It listens on `127.0.0.1:0`, checks the Host header (against DNS rebinding), and requires the `pk_desktop` cookie that `/__desktop?k=SECRET` sets; the secret changes on every start. Keep these checks in `web::auth::guard` first. Never open a receipt that is not a PDF or a picture: `DesktopServer::fetch` marks it to save, not to view.
- **Parents never reach the books.** A parent is a `families` link in `control.db`, never a membership. `hosted::guard` sends `/family` paths to `family_guard`, which puts a `FamilyCtx` (not a `Ctx`) on the request; a parent who opens any other PAC page goes to `/family`. Parent handlers take `FamilyCtx` and filter every record by `person_id`. `web::lunch_tests` checks this end to end; extend it when you add a page.
- **Hot lunch money rules.** Prices come from `lunch_day_items`, never from the form. A Stripe payment counts only when the session id, amount, and currency match the order (`family::confirm`), and `lunch::mark_paid` changes an order once. Every webhook is checked against its signature. A Helcim payment counts only when Helcim itself lists an approved purchase of the order's amount in CAD under the order's latest invoice number (`family::confirm_helcim`); never trust what the card page's script reports. A PAC's Helcim API token is stored only sealed (`Helcim::seal`), never as text, and never goes to a page or a log. `FeeConfig::express_fee` rounds each part up on its own so the fee always covers Stripe; keep its tests passing when you change it. Express accounts get weekly payouts on `FeeConfig::payout_day`, so the month's charge (`month_charge`) covers the account fee and one payout fee for each payout day in the month. Every Stripe request sends `Stripe-Version` (`stripe::API_VERSION`). With `AppState.lunch` None (no `--hot-lunch`), every hot lunch route answers 404.
- **No browser dialogs or new tabs.** The desktop webview does not show `confirm()` and handles new tabs badly. Ask with `data-confirm="Question?"` on the form (`static/app.js`), and use `target="{{ layout.new_tab() }}"` for links to files.
- **No user data inside inline JavaScript.** Put it in a `data-` attribute and read it from script (see `hosted/operator.html`).
- **Demo mode** (`serve --demo`) wipes and reloads the data folder on a schedule. `demo::reset` deletes tables children first; add any new table to its list in the right place. `demo::reset` refuses any folder that is not marked `demo_data` or empty. Keep that guard. Uploads are off in demo mode (`web::no_uploads_in_demo`); any new upload route needs the same check.
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
