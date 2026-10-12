# PAC Finances

Bookkeeping and reports for a BC school Parent Advisory Committee (PAC).

The app keeps the PAC's bank transactions, receipts, and budget in one place. It produces the monthly treasurer's report, the annual report for the AGM, the budget proposal, and a worksheet for the BC Community Gaming Grant *Gaming Account Summary Report*.

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
- **Keeps the gaming account apart.** Each account is *general* or *gaming*. A transaction can only use line items from its own fund. The app checks the gaming grant conditions:
  - It finds deposits that are not the grant, interest, or a GST rebate.
  - It finds payments with no receipt or no purpose.
  - It builds reimbursement summaries for transfers to the general account, and checks that they match the transfer exactly and are in the same fiscal year.
  - It shows the 24-month spending deadline for each grant.
  - It shows the date the report is due.
- **Records licensed gaming events** (ticket raffles, bingo, and others) with the licence number, class, and dates. Event revenue goes on lines 6 to 13 of the Gaming Account Summary Report. Prizes and event expenses go on lines 19 to 26. The app warns when a finished event cost more than it raised.
- **Makes PDF reports:**
  - **Monthly treasurer's report:** bank balances with a check against the bank, budget against actual for the month and year to date, every transaction, and items to resolve.
  - **Annual report for the AGM:** fund balances, budget against actual with variances, monthly totals, the gaming summary, the treasurer's notes, and signature lines.
  - **Budget proposal:** next year's budget beside last year's budget and actuals, with an approval block. Start it from last year's budget or actuals.
  - **Gaming Account Summary worksheet:** the figures for each line of the provincial form, with every disbursement itemized.

## Start

You need [Rust](https://rustup.rs).

```sh
cargo build --release
./target/release/pac-finances
```

Open <http://127.0.0.1:8080>. The records go in the `data` folder in the current directory. Use `--data-dir` to put them somewhere else, such as a synced or backed-up folder:

```sh
./target/release/pac-finances --data-dir ~/Documents/PAC-Finances
```

### Try it with example data

```sh
./target/release/pac-finances --data-dir demo-data demo
./target/release/pac-finances --data-dir demo-data
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
2. Open **Transactions** and choose a line item for each new row. Rows with no line item are highlighted.
3. Open each payment and attach the receipt. For gaming payments, enter the purpose.
4. When the gaming account repays the general account, open the transfer and tick the general expenses it repays.
5. Open **Reports** and make the monthly report for the meeting.

The dashboard shows what needs attention. The bank balance from each import is compared with the books. A difference usually means a missing import or a wrong opening balance.

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
pac-finances --data-dir /var/lib/pac-finances user add sam --role treasurer --name "Sam Rivera"
pac-finances --data-dir /var/lib/pac-finances user add chair --role viewer
pac-finances --data-dir /var/lib/pac-finances serve --auth --bind 127.0.0.1:8080
```

- A **treasurer** can change records. A **viewer** can see everything and open reports, but cannot change anything or download backups.
- `user add` asks for the password. For scripted set-up, set `PAC_PASSWORD`.
- Put a reverse proxy with HTTPS in front, such as Caddy or nginx.
- Every option can also be set in the environment: `PAC_DATA_DIR`, `PAC_BIND`, `PAC_AUTH=1`.

## Command reference

```
pac-finances [--data-dir DIR] [serve [--bind ADDR] [--auth]]
pac-finances user add NAME [--role treasurer|viewer] [--name "Display name"]
pac-finances user list
pac-finances user remove NAME
pac-finances pdf monthly 2026-09 [-o file.pdf]
pac-finances pdf annual [2025-26] [-o file.pdf]
pac-finances pdf budget [2026-27] [-o file.pdf]
pac-finances pdf gaming [2025-26] [-o file.pdf]
pac-finances demo
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

See `CLAUDE.md` for the code layout.
