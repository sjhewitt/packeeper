<img src="static/logo.svg" alt="" width="64" height="64">

# PAC Keeper

Bookkeeping and reports for a BC school Parent Advisory Committee (PAC). <https://packeeper.ca>

PAC Keeper keeps the PAC's bank transactions, receipts, and budget in one place. It produces the monthly treasurer's report, the annual report for the AGM, the budget proposal, and a worksheet for the BC Community Gaming Grant *Gaming Account Summary Report*.

It is one program and one data folder. It runs on the treasurer's computer. It can run on a server later, with sign-in for the executive.

## What it does

- **Imports bank files** from Prospera online banking: CSV, OFX, QFX, or QBO.
  - You choose the CSV columns once for each account. The app remembers the choice.
  - Rows that are already imported are skipped, so date ranges can overlap.
  - A cheque that you enter by hand is matched to the bank row when the bank file arrives.
- **Keeps receipts and notes** on each transaction. Drop in photos or PDFs.
- **Assigns transactions to budget line items.**
  - One transaction can be split across several line items.
  - Rules assign repeat transactions automatically, such as bank fees.
- **Tracks fundraisers** that come back every year, such as hot lunch, Purdy's chocolates, or the fall fair. Add each fundraiser once and use it every year.
  - Give a transaction, or one part of a split transaction, its fundraiser: the sales, and the costs such as the supplier's bill.
  - Each fundraiser shows its income, costs, and net for the fiscal year, its progress toward that year's goal, and every year side by side. The dashboard shows every fundraiser's year to date.
  - Rules can set the fundraiser at import.
- **Keeps the gaming account apart.** Each account is *general* or *gaming*. A transaction can only use line items from its own fund. The app checks the gaming grant conditions:
  - It finds deposits that are not the grant, interest, or a GST rebate.
  - It finds payments with no receipt or no purpose.
  - It builds reimbursement summaries for transfers to the general account, and checks that they match the transfer exactly and are in the same fiscal year.
  - It shows the 24-month spending deadline for each grant.
  - It shows the date the report is due.
- **Records licensed gaming events** (ticket raffles, bingo, and others) with the licence number, class, and dates. Event revenue goes on lines 6 to 13 of the Gaming Account Summary Report. Prizes and event expenses go on lines 19 to 26. The app warns when a finished event cost more than it raised.
- **Makes PDF reports:**
  - **Monthly treasurer's report:** bank balances with a check against the bank, budget against actual for the month and year to date, fundraisers for the month and year to date, every transaction, and items to resolve.
  - **Annual report for the AGM:** fund balances, budget against actual with variances, each fundraiser's net against its goal and the year before, monthly totals, the gaming summary, the treasurer's notes, and signature lines.
  - **Fundraiser status report:** one fundraiser's income, costs, net, and goal for a year, by line item, with its transactions and every year side by side.
  - **Budget proposal:** next year's budget beside last year's budget and actuals, with an approval block. Start it from last year's budget or actuals.
  - **Gaming Account Summary worksheet:** the figures for each line of the provincial form, with every disbursement itemized.

## Desktop app

PAC Keeper runs as an app on a Mac or a Windows computer. Download it from the project's GitHub releases:

- **Mac:** the `.dmg` file, for Apple silicon and Intel Macs. Open it and drag PAC Keeper to Applications.
- **Windows:** the `-setup.exe` file. It installs for the current user, so it does not ask for an administrator.

The apps are not signed yet, so the computer warns you the first time:

- **Mac:** macOS says it cannot check PAC Keeper. Click Done. Open System Settings, then Privacy & Security. Scroll down and click Open Anyway next to PAC Keeper.
- **Windows:** "Windows protected your PC" shows. Click More info, then Run anyway.

The app keeps the records on the computer, in the app data folder:

- Mac: `~/Library/Application Support/PAC Keeper`
- Windows: `%LOCALAPPDATA%\PAC Keeper`

The records are not in Documents, because iCloud and OneDrive often sync Documents, and syncing can damage a database that is open. **Settings → Show the folder** opens the folder. **Settings → Save a backup** puts a zip in Downloads. To move the records to another computer, close PAC Keeper on both computers, then copy the folder.

The app is the same program as the server below, in a native window. Only the app window can open the records: the server listens on this computer only and needs a secret that changes on every start. PDFs and receipts open in the computer's viewer. Other attached files are saved in Downloads and never opened, so a file cannot run by a click.

## Start

To run PAC Keeper in a web browser, or on a server, build it from source. You need [Rust](https://rustup.rs).

```sh
cargo build --release
./target/release/pac-keeper
```

Open <http://127.0.0.1:8080>. The records go in the `data` folder in the current directory. Use `--data-dir` to put them somewhere else, such as a synced or backed-up folder:

```sh
./target/release/pac-keeper --data-dir ~/Documents/PAC-Keeper
```

### Try it with example data

```sh
./target/release/pac-keeper --data-dir demo-data demo
./target/release/pac-keeper --data-dir demo-data
```

The example PAC has a year and a half of activity, both kinds of account, a gaming reimbursement, and a cheque that has not cleared.

## First set-up

1. **Settings:** enter the PAC name, the school, the treasurer and chair, and the month the fiscal year starts. Check the PAC bylaws for the fiscal year. Most BC PACs use July 1 to June 30. For the gaming grant, enter the enrolment and the file number.
2. **Accounts:** add the general account and the gaming account. Enter each opening balance from a bank statement. Use the first day of the fiscal year where you start.
3. **Line items:** check the starter list and change it to match your budget.
4. **Budget:** enter this year's budget amounts.
5. **Import:** upload a bank file for each account.

## Monthly work

1. Download the account activity from Prospera for each account and import it.
2. Open **Transactions** and choose a line item for each new row. Rows with no line item are highlighted. For fundraiser money, choose the fundraiser in the next column. To give each part of a split its own fundraiser, open the transaction.
3. Open each payment and attach the receipt. For gaming payments, enter the purpose.
4. When the gaming account repays the general account, open the transfer and tick the general expenses it repays.
5. Open **Reports** and make the monthly report for the meeting.

The dashboard shows what needs attention. The bank balance from each import is compared with the books. A difference usually means a missing import or a wrong opening balance.

## Fundraisers

A fundraiser is not tied to a year. Add "Purdy's chocolates" once; next year, its new orders go on the same fundraiser, and its page shows both years side by side.

- Fundraisers are for general-account money. Gaming funds cannot pay fundraising costs. Record a licensed raffle or bingo as a licensed gaming event.
- Income is the money in and costs are the money out, of the parts that carry the fundraiser. Net is income less costs. A goal is the net to raise in one fiscal year.
- Fundraisers are a second view of the same money. Each part still counts on its line item in the budget and the reports.
- Mark a fundraiser inactive when the PAC stops running it. Its past years stay in the reports. A fundraiser with no transactions can be deleted.

## Exporting from Prospera

- Prefer **OFX**, **QFX**, or **QBO** if Prospera offers them. These carry the bank's own transaction IDs and the statement balance.
- A **CSV** works too. The preview shows how the app reads each column. Fix any column it guessed wrong. The choice is saved for that account.
- The exact Prospera export layout is not confirmed. Prospera merged with Coast Capital in 2026 and plans to move to Coast Capital's banking platform in 2027. The export layout may change then. If it does, fix the column choices on the preview page.
- A direct bank connection is not built in. Aggregators such as Plaid or Flinks need an approved business account and, for Flinks, a monthly fee. The file import avoids both.

## Data, backups, and handover

All records are in the data folder:

| Path | Contents |
|---|---|
| `pac.db` | The SQLite database |
| `receipts/` | Receipt files, by year |
| `imports/` | Every bank file that you uploaded |
| `secret.key` | The key for sign-in cookies |

**Settings → Download a backup** makes a zip of the database, receipts, and bank files. To hand over to the next treasurer, give them the data folder and the program.

The gaming grant conditions require you to keep receipts and bank statements for 5 years after the year the money was spent.

## Running on a server

Sign-in is off by default, because the app runs on one computer. To share it with the executive, run it on a server with sign-in on:

```sh
pac-keeper --data-dir /var/lib/pac-keeper user add sam --role treasurer --name "Sam Rivera"
pac-keeper --data-dir /var/lib/pac-keeper user add chair --role viewer
pac-keeper --data-dir /var/lib/pac-keeper serve --auth --bind 127.0.0.1:8080
```

- A **treasurer** can change records. A **viewer** can see everything and open reports, but cannot change anything or download backups.
- `user add` asks for the password. For scripted set-up, set `PAC_PASSWORD`.
- Put a reverse proxy with HTTPS in front, such as Caddy or nginx.
- Every option can also be set in the environment: `PAC_DATA_DIR`, `PAC_BIND`, `PAC_AUTH=1`.

### With Docker

```sh
docker build -t pac-keeper .
docker run -d -p 127.0.0.1:8080:8080 -v pac-data:/data -e PAC_AUTH=1 pac-keeper
docker exec -it <container> pac-keeper user add sam --role treasurer
```

The records live in the `/data` volume. Back it up.

## Hosting many PACs

`serve --hosted` runs one server for many PACs. PACs request an account on the sign-up page, you approve them, and each PAC's executive signs in by email.

```sh
pac-keeper --data-dir /var/lib/pac-keeper-hosted operator you@example.org --name "Your Name"
PAC_SMTP_URL='smtps://USER:PASSWORD@smtp.example.com:465' \
PAC_MAIL_FROM='PAC Keeper <hello@packeeper.ca>' \
pac-keeper --data-dir /var/lib/pac-keeper-hosted serve --hosted \
  --base-url https://app.packeeper.ca --bind 127.0.0.1:8080
```

How it works:

- **One database per PAC.** The data folder holds `control.db` (the PACs, the people, and their memberships) and a folder for each PAC under `pacs/`. Each PAC folder is laid out exactly like a single-PAC data folder, so one PAC's records cannot appear in another PAC's pages. A PAC that leaves takes its zip from **Settings** and can run PAC Keeper itself.
- **Joining.** A treasurer fills in `/signup` and confirms their email. The request appears on `/operator`, and the operator gets an email. When you approve it, the treasurer gets an email that the PAC is ready.
- **People.** Everyone signs in by email. One person can belong to several PACs and switch between them. Treasurers invite the executive from **Settings → Members**, as treasurers or viewers. A PAC always keeps at least one treasurer. Forgotten passwords are reset by email, and a reset signs out every other session.
- **Operators.** `pac-keeper operator EMAIL` makes someone an operator. Operators approve, reject, suspend, and reactivate PACs on `/operator`.
- **Limits.** Sign-in attempts, sign-ups, password resets, and invitations are rate-limited. Each PAC may store 1 GB of receipts (`--quota-mb`). Form posts from other sites are refused. Cookies are marked secure when `--base-url` starts with `https://`.
- **Email.** Set `PAC_SMTP_URL` and `PAC_MAIL_FROM` for any SMTP provider. Without them, each email is written to `outbox/` in the data folder, which is useful for testing.

Run it behind HTTPS, with Caddy for example:

```
app.packeeper.ca {
    reverse_proxy 127.0.0.1:8080
}
```

With Docker: `docker run -d -p 127.0.0.1:8080:8080 -v pac-hosted:/data -e PAC_HOSTED=1 -e PAC_BASE_URL=https://app.packeeper.ca -e PAC_SMTP_URL=... -e PAC_MAIL_FROM=... pac-keeper`

**Backups are your job as the operator.** `pac-keeper --data-dir DIR backup --to BACKUP_DIR` copies every database, receipt, and bank file into a dated folder. It is safe while the server runs. Run it every night (from cron or a systemd timer), copy the result off the server (for example with `rclone` to encrypted storage), and test a restore. Before you take real PAC data, also:

- host the server in Canada if you promise Canadian data residency
- publish a privacy policy and terms of service
- get advice on your obligations under BC's privacy law (PIPA)

## The public demo

`serve --demo` runs a public demo for the website:

```sh
pac-keeper --data-dir /var/lib/pac-keeper-demo serve --demo --bind 127.0.0.1:8080
# or
docker run -d -p 127.0.0.1:8080:8080 -e PAC_DEMO=1 pac-keeper
```

- It loads the example PAC, with dates moved forward so the data never looks old.
- Sign-in is on. The sign-in page shows the public login: `demo` / `packeeper`.
- The data resets every 60 minutes (`--demo-reset-minutes`, at least 5). A banner tells visitors.
- File uploads are off, because anyone can use the demo. The import page offers a built-in sample statement instead.
- A reset deletes every record, so demo mode refuses a data folder that holds real records. Use a separate folder or volume.

The website links to `https://demo.packeeper.ca`. Put the demo behind HTTPS there, for example with Caddy:

```
demo.packeeper.ca {
    reverse_proxy 127.0.0.1:8080
}
```

## The website

`site/` is the static website for packeeper.ca: one HTML page and its images, with no build step. Upload the folder to any static host, such as Cloudflare Pages, Netlify, or GitHub Pages.

- It links to the demo at `https://demo.packeeper.ca` and shows the demo login.
- It names `hello@packeeper.ca` as the contact address.
- The screenshots in `site/img/` come from the demo data. To refresh them, run the demo and capture the pages again.

## Command reference

```
pac-keeper [--data-dir DIR] [serve [--bind ADDR] [--auth] [--demo [--demo-reset-minutes N]]]
pac-keeper --data-dir DIR serve --hosted --base-url URL [--quota-mb N]
pac-keeper --data-dir DIR operator EMAIL [--name NAME]
pac-keeper --data-dir DIR backup --to BACKUP_DIR
pac-keeper user add NAME [--role treasurer|viewer] [--name "Display name"]
pac-keeper user list
pac-keeper user remove NAME
pac-keeper pdf monthly 2026-09 [-o file.pdf]
pac-keeper pdf annual [2025-26] [-o file.pdf]
pac-keeper pdf budget [2026-27] [-o file.pdf]
pac-keeper pdf gaming [2025-26] [-o file.pdf]
pac-keeper demo
```

## Gaming grant rules used

The checks follow the 2026 *PAC and DPAC Guidelines* and *Conditions for a PAC/DPAC Grant* from the BC Community Gaming Grants branch. Check the current documents before you rely on them:

- [PAC and DPAC Guidelines](https://www2.gov.bc.ca/assets/gov/sports-recreation-arts-and-culture/gambling/grants/guide-pac.pdf)
- [Conditions for a PAC/DPAC Grant](https://www2.gov.bc.ca/assets/gov/sports-recreation-arts-and-culture/gambling/grants/cond-pac.pdf)
- [Gaming Account Summary Report form](https://www2.gov.bc.ca/assets/gov/sports-recreation-arts-and-culture/gambling/grants/form-acct-summary-rpt.pdf)

The app does not decide whether an expense is eligible. The **Gaming** page lists the eligible and ineligible uses as a reminder.

### Gaming report lines

Each gaming line item has a *gaming report line* setting (**Settings → Line items**):

| Setting | Form lines | Starter line item |
|---|---|---|
| PAC/DPAC grant | 3 | PAC gaming grant |
| Community Gaming Grant | 2 | — |
| Other gaming revenue (pre-approved transfer or repayment) | 5 | — |
| Licensed gaming event revenue | 6 to 13, one line per licence | Licensed gaming event revenue |
| GST rebate, interest, or sale of assets | 14 | Interest and GST rebates |
| Gaming fund donation from another organization | 15 to 17, one line per organization | — |
| Prize costs for a licensed event | 19 to 22, split by bingo, ticket raffle, and other | Licensed gaming event prizes |
| Expenses for a licensed event | 23 to 26, split the same way | Licensed gaming event expenses |
| None (a gaming expense) | 30 to 45, itemized | Field trips, and so on |

Gaming income with no setting is money that may not go into the gaming account. The report puts it on line 5 and marks it as a problem.

To record a licensed event, add it under **Gaming → Licensed gaming events**. Then open each deposit, prize, or expense in the gaming account and choose the event. Lines 4 and 29 (Capital Project Grants) are not used by PACs.

## Development

```sh
cargo test     # unit tests and report balance checks on the example data
cargo clippy
cargo fmt
```

### Desktop app

The desktop app is the `desktop/` crate, built with [Tauri 2](https://tauri.app). It shares the `target/` folder with the main crate. You need Node.js for the Tauri command-line tool.

```sh
cd desktop
npm ci
npm run tauri dev                                  # run it
npm run tauri build -- --bundles app,dmg           # Mac: the .app and .dmg in target/release/bundle/
npm run tauri build -- --bundles nsis              # Windows: the installer
```

`PAC_DATA_DIR` makes the app use another folder, for example a copy of the example data.

`.github/workflows/desktop.yml` builds both apps on GitHub Actions. Run it from the Actions tab to get the files as build artifacts. Push a tag such as `v0.2.0` to also make a draft release with the files attached. Check the draft, then publish it.

To sign the apps later, add an Apple Developer ID certificate and notarization secrets for macOS, and a code-signing certificate for Windows. See the Tauri guides on [macOS signing](https://tauri.app/distribute/sign/macos/) and [Windows signing](https://tauri.app/distribute/sign/windows/).

See `CLAUDE.md` for the code layout.
