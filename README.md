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

The app updates itself. It looks for a new version when it starts and once a day, downloads it from the GitHub releases, and checks its signature. The new version installs when you close PAC Keeper. **Settings → Updates** checks at once, installs a downloaded version now, and turns automatic updates off. An update keeps your records. PAC Keeper 0.4.0 and older do not update themselves: install the next version by hand once.

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
5. Open **Reports** and make the treasurer's report for the meeting.

**Reports between meetings.** A PAC that meets on a set day, such as the 2nd Wednesday of each month, sets the rule under **Settings → Meeting dates**, with the months that have no meeting. The rule fills in each fiscal year's meetings once; then you can move or remove a single meeting, or add one. The report for a meeting covers the days from the last meeting to the day before this one, so nothing falls between two reports. The first report of a fiscal year starts on its first day; the days after the last meeting of the year before are in the annual report. Without meeting dates, the report covers a calendar month.

The dashboard shows what needs attention. The bank balance from each import is compared with the books. A difference usually means a missing import or a wrong opening balance.

## Fundraisers

A fundraiser is not tied to a year. Add "Purdy's chocolates" once; next year, its new orders go on the same fundraiser, and its page shows both years side by side.

- Fundraisers are for general-account money. Gaming funds cannot pay fundraising costs. Record a licensed raffle or bingo as a licensed gaming event.
- Income is the money in and costs are the money out, of the parts that carry the fundraiser. Net is income less costs. A goal is the net to raise in one fiscal year.
- Fundraisers are a second view of the same money. Each part still counts on its line item in the budget and the reports.
- A general-fund line item can name a fundraiser (**Settings → Line items → Edit**). Then every part on that line item belongs to the fundraiser, unless you choose another fundraiser for the part. This includes the parts that use the line item already, new ones, and imports that a rule assigns. When the line item of a part changes, a fundraiser that came from the old line item goes with it.
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

**Settings → Restore a backup** puts such a zip back, in any copy of PAC Keeper: the desktop app, your own server, or the hosted app. So you can keep the books on your computer, then move them to a hosted PAC (or back). The restore replaces the PAC's records: transactions, budgets, receipts, bank files, and settings. Sign-in users and hosted members stay. Before it starts, PAC Keeper keeps a copy of the records you had, which **Download the records from before the restore** gives back.

A restore takes only a zip that PAC Keeper made, from this version or an older one: the database must pass SQLite's integrity check, have exactly PAC Keeper's tables, and name only plain receipt files. On a hosted server the receipts must fit the PAC's storage, and hot lunch children and orders of parents who are not in that PAC are left out.

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

### Encrypted records

With `PAC_DATA_KEY`, the hosted server encrypts the records on disk. SQLCipher encrypts `control.db` and each PAC's `pac.db`. AES-256-GCM seals the receipts, the bank files, and the copy from before a restore. Each PAC has its own keys, which come from the data key.

1. Make a key: `pac-keeper new-data-key`. It writes 64 hex digits.
2. Keep a copy in your password manager, away from the server. **If you lose the key, nobody can read the records or the backups. That includes you.**
3. Put the key in the server's settings, outside the data folder: `PAC_DATA_KEY="..."`. Only the environment can give the key. There is no command-line option, so the key does not show in the list of running programs.
4. Stop the server, then start it (on Opalstack: `~/apps/pk_app/stop && ~/apps/pk_app/start`). At start, it encrypts all records that are still plain, so you can turn this on for a server that has PACs already. It does not encrypt a database that another program has open. Do this as its own step, after the upgrade to a version with encryption. If the encryption stops with an error, fix the cause and start again: it continues where it stopped. The operator page (`/operator`) shows whether each database on disk is encrypted.

What this protects:

- A copy of the data folder, of a backup, or of the disk is useless without the key.
- It does not keep out a person who can use the server's account. The running server has the key, and that person can read it from the settings.
- A PAC's own download (**Settings → Download all records**) is a plain zip, so the PAC can open it in the desktop app. A restore also takes a plain zip. While it makes or restores a zip, the server keeps a plain copy for a short time, then deletes it.

The `backup`, `operator`, and `stats` commands need `PAC_DATA_KEY` too. Backups stay encrypted with the same key. To restore one, copy it back and start the server with the same key. Copies from before you set the key stay plain: your own backups, and the host's backups of the disk. Delete your old backups when the encrypted backups work. A single-PAC folder and the desktop app are not encrypted, and they refuse `PAC_DATA_KEY`.

### Visit counts

The operator page links to **Visits** (`/operator/stats`), which only operators can open. It shows visits to the website, the demo, and the app, sign-ups and sign-ins, where visitors came from, the demo pages they opened, downloads of the desktop apps, and the number of accounts and PACs.

The server reads the web server's access logs (nginx's "combined" format) every hour and saves a count for each day, so the counts stay after the logs rotate. Set the logs and the GitHub repository it reads:

```sh
PAC_STATS_SITE_LOG=/home/pk_user/logs/sites/packeeper/access.log
PAC_STATS_DEMO_LOG=/home/pk_user/logs/sites/pk_demo/access.log
PAC_STATS_APP_LOG=/home/pk_user/logs/sites/pk_app/access.log
PAC_STATS_GITHUB=sjhewitt/packeeper
```

- **Counts only.** No address, browser, or app path is saved. A visit is one address with one browser on one day.
- **Left out:** bots (browser names that are not a person's, requests that only attacks send, and three or more missing pages) and you: any browser that opened `/operator` on the app.
- **Days are in UTC.** The first day in a log can be there only in part, so its counts never go down. Later days are read again in full every hour.
- **On the command line:** `pac-keeper --data-dir DIR stats` prints the same counts. Add `--now` to read the logs first (it needs the `PAC_STATS_*` settings).

**Backups are your job as the operator.** `pac-keeper --data-dir DIR backup --to BACKUP_DIR` copies every database, receipt, and bank file into a dated folder. It is safe while the server runs. Run it every night (from cron or a systemd timer), copy the result off the server to encrypted storage, and test a restore. On Opalstack, the `backup` script does this; see [Backups to S3](#backups-to-s3). Before you take real PAC data, also:

- publish a privacy policy and terms of service
- get advice on your obligations under BC's privacy law (PIPA)

## Hot lunch (preview)

Hot lunch ordering runs on a hosted server, behind a flag that is off by default. Start the server with `--hot-lunch` (or `PAC_HOT_LUNCH=1`). Each PAC's treasurer then turns it on under **Settings → Hot lunch**.

- **The PAC** sets a menu and its divisions (classes), adds ordering periods with lunch days, and shares a sign-up link with families. **Hot lunch** shows what to buy each day and for which division, prints a delivery sheet for each day, and lists every order. The treasurer marks e-transfer and cash orders paid, and cancels or refunds orders.
- **Parents** sign up with the PAC's link, add their children with their divisions, and order a lunch for each child on each day of a period, until the period's deadline. A parent's account opens only the hot lunch pages (`/family`), never the PAC's books. Parents are linked to a PAC in `control.db` (`families`), not as members.
- **Payment** is by card, Interac e-Transfer, or cash. Each PAC chooses its ways to pay. Prices come from each day's menu, so an order keeps the prices that the parent saw.
- **The books:** the money reaches the bank as Stripe payouts or e-transfers. Import the bank file as usual, and give those deposits the hot lunch line item and fundraiser.

### Card payments (Stripe Connect)

Card details go to Stripe Checkout, never to PAC Keeper. A PAC takes cards in one of two ways:

- **Its own Stripe account** (Connect Standard). The treasurer connects it under **Hot lunch → Payments and families**. Payments go to that account, and Stripe charges its fees there. PAC Keeper can add a fee of its own with `PAC_STANDARD_FEE_BP` and `PAC_STANDARD_FEE_CENTS` (none by default).
- **Through PAC Keeper** (Connect Express). PAC Keeper opens a Stripe account for the PAC. Stripe checks the PAC's details and pays its bank. The PAC is the merchant on the card statement. Each payment carries an application fee that covers what the platform pays Stripe, and the surcharge. The PAC pays the fee from the lunch money; parents pay only the menu prices, so no card-surcharge rules apply.

The application fee on an Express payment, with each part rounded up to a cent:

| Part | Default | Setting |
|---|---|---|
| Card fee | 2.9% + C$0.30 | `PAC_STRIPE_CARD_BP`, `PAC_STRIPE_CARD_CENTS` |
| Payout fee, on each payment | 0.25% | `PAC_STRIPE_PAYOUT_BP` |
| Account fee, on the first card payment each month | C$2.00 | `PAC_STRIPE_MONTHLY_CENTS` |
| Fee for each payout, for each payout day in the month, on the first card payment each month | C$0.25 | `PAC_STRIPE_PAYOUT_CENTS` |
| Your surcharge | none | `PAC_SURCHARGE_BP`, `PAC_SURCHARGE_CENTS` |

**Payouts.** Stripe pays each Express PAC once a week, on Monday (`PAC_STRIPE_PAYOUT_DAY`, any weekday). The PAC gives Stripe its bank account during the set-up, and sees its payouts in its Stripe Express dashboard. A month has at most one payout for each payout day in it, so the month's charge covers every possible payout: C$2.00 and C$0.25 for each Monday, or C$3.00 to C$3.25. An account made before weekly payouts gets the weekly schedule the next time its treasurer returns from Stripe's set-up page.

Every request to Stripe names API version `2025-06-30.basil`, so the calls work the same whatever version your Stripe account defaults to.

Rates are in basis points: 290 is 2.9%. Check them against Stripe's current prices for Canada. International cards cost more, so a small surcharge leaves room for them. On Express payments the platform account answers for disputes first; a refund from PAC Keeper takes the money back from the PAC's account, and the fees are not returned.

To turn card payments on, set these for the hosted server:

- `PAC_STRIPE_SECRET_KEY`: the platform account's secret key. Turn on Connect in that account.
- `PAC_STRIPE_WEBHOOK_SECRETS`: in Stripe, add two webhook endpoints for `https://app.packeeper.ca/stripe/webhook`, one for **events on your account** and one for **events on connected accounts**. Send `checkout.session.completed` and `checkout.session.async_payment_succeeded` to each. Put both signing secrets here, separated by a comma.
- `PAC_STRIPE_CLIENT_ID`: the Connect client id (`ca_...`), so that PACs can connect their own accounts. In Stripe's Connect settings, add the redirect address `https://app.packeeper.ca/lunch/setup/standard/return`. Without it, PACs can use only Express.

Without `PAC_STRIPE_SECRET_KEY`, the Stripe choices do not show. PACs can still take cards with Helcim, or use e-transfer and cash.

### Card payments (Helcim)

A PAC with its own [Helcim](https://www.helcim.com) account can take cards there instead of Stripe. The server needs no settings for it.

- **Connect.** In Helcim, the treasurer opens **All Tools → Integrations → API Access Configurations** and adds a configuration with **Transaction Processing: Admin** (Admin lets refunds work) and no other access. They paste its API token under **Hot lunch → Payments and families**. PAC Keeper checks the token with Helcim, and keeps it encrypted with a key made from the server's `secret.key`. A copy of `control.db` alone does not show it.
- **Paying.** The parent pays in Helcim's card form (HelcimPay.js) on the order's card page. Each checkout has a new invoice number. The server then asks Helcim for that invoice's transactions. Only an approved purchase of the order's amount in CAD pays the order. The server also checks waiting card orders of the last 3 days every 5 minutes, so an order is paid even when the parent closes the page early.
- **Fees.** Helcim charges its fees in the PAC's account. PAC Keeper takes no fee. If you are a Helcim integration partner, set `PAC_HELCIM_PARTNER_TOKEN` and every request carries it, for Helcim's revenue share.
- **Refunds** go back through Helcim when the token has Admin access. Otherwise refund in Helcim.
- HelcimPay.js needs an HTTPS address that the public can reach. It does not work on `127.0.0.1`. Test it with a Helcim developer test account.

A PAC uses one card company at a time: Stripe or Helcim.

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

`site/` is the static website for packeeper.ca: one HTML page and its images, with no build step. Upload the folder to any static host, such as Opalstack (see below), Cloudflare Pages, Netlify, or GitHub Pages.

- It links to the demo at `https://demo.packeeper.ca` and shows the demo login.
- It names `hello@packeeper.ca` as the contact address.
- The screenshots in `site/img/` come from the demo data. To refresh them, run the demo and capture the pages again.

## Hosting on Opalstack

[Opalstack](https://www.opalstack.com) can run the website, the demo, and the hosted server on one account, without Docker. The website is a static app. The demo and the hosted server are each a "Proxied Port" app: the Linux program listens on `127.0.0.1` at the app's port, and Opalstack's web server passes it the requests over HTTPS.

1. **The program.** Each release has `pac-keeper-linux-x86_64.tar.gz`: one static program that runs on any 64-bit Linux, and the scripts in `deploy/opalstack/`. Before the first release, download the `pac-keeper-linux-x86_64` artifact from a run of the "Release builds" workflow. On the server, signed in with SSH as the apps' user:

   ```sh
   mkdir -p ~/bin ~/downloads && cd ~/downloads
   curl -LO https://github.com/sjhewitt/packeeper/releases/latest/download/pac-keeper-linux-x86_64.tar.gz
   tar -xzf pac-keeper-linux-x86_64.tar.gz
   cp pac-keeper/pac-keeper ~/bin/ && ~/bin/pac-keeper --version
   install -m 755 pac-keeper/backup ~/bin/pac-keeper-backup
   ```

2. **Domains.** In the dashboard, add `packeeper.ca` and `packeeper.com`, with the subdomains `www`, `demo`, and `app`. Point their DNS at your Opalstack server.

3. **Apps.** Create three apps. Write down the port of each Proxied Port app.
   - `pk_site`, type **Static only**, for the website
   - `pk_demo`, type **Proxied port**, for the demo
   - `pk_app`, type **Proxied port**, for the hosted server

4. **The website.** From this repository, copy `site/` into the static app. Use the server name from the dashboard:

   ```sh
   rsync -av --delete site/ USER@SERVER.opalstack.com:apps/pk_site/
   ```

5. **The scripts and settings.** For each Proxied Port app, copy the scripts into its folder. They replace any `start` and `stop` that Opalstack made:

   ```sh
   for app in pk_demo pk_app; do
     cp ~/downloads/pac-keeper/start ~/downloads/pac-keeper/stop ~/apps/$app/
     cp ~/downloads/pac-keeper/env.example ~/apps/$app/env
   done
   ```

   Then edit each `env` file:
   - `~/apps/pk_demo/env`: the app's port, `PAC_DATA_DIR="$HOME/pac-keeper-data/demo"`, and `PAC_DEMO=1`.
   - `~/apps/pk_app/env`: the app's port, `PAC_DATA_DIR="$HOME/pac-keeper-data/hosted"`, `PAC_HOSTED=1`, `PAC_BASE_URL`, and the email settings. First make a mailbox for `hello@packeeper.ca` in the dashboard. The SMTP user is the mailbox name, not the email address.
   - Also in `~/apps/pk_app/env`: `PAC_DATA_KEY`, from `~/bin/pac-keeper new-data-key`. Keep a copy in your password manager. See [Encrypted records](#encrypted-records).
   - Run `chmod 600 ~/apps/*/env`. The files hold passwords and the data key.

6. **The operator.** Make yourself the hosted server's operator. The command reads the app's settings, for the data folder and the data key:

   ```sh
   (set -a; . ~/apps/pk_app/env; ~/bin/pac-keeper operator you@example.org --name "Your Name")
   ```

7. **Start, and keep running.** Run `~/apps/pk_demo/start` and `~/apps/pk_app/start`. Each prints its log file if the server stops at start. Then add these lines with `crontab -e`. Every 10 minutes, cron starts a server that is not running, for example after a server restart:

   ```
   */10 * * * * $HOME/apps/pk_demo/start > /dev/null 2>&1
   */10 * * * * $HOME/apps/pk_app/start > /dev/null 2>&1
   ```

   Then set up the nightly backup: see [Backups to S3](#backups-to-s3).

8. **Sites.** In the dashboard, create one site for each address. Turn on the Let's Encrypt certificate and the redirect to HTTPS for each.
   - `packeeper.ca` and `www.packeeper.ca`: app `pk_site` at `/`
   - `demo.packeeper.ca`: app `pk_demo` at `/`
   - `app.packeeper.ca`: app `pk_app` at `/`
   - `packeeper.com`: a redirect to `https://packeeper.ca`. Use the domain registrar's forwarding, or follow Opalstack's guide to URL redirects.

9. **Check.** On the demo, sign in and change a line item. This checks that form posts pass through the proxy. On the hosted server, sign up with a second email address, check that the email arrives, and approve the PAC on `/operator`.

**Upgrades.** Publish a release, and the Deploy workflow installs it (see below). By hand: download the release as in step 1, then run `~/downloads/pac-keeper/upgrade ~/downloads/pac-keeper`. The script backs up each hosted server's records to `~/pac-keeper-backups/APPNAME/`, installs the new program and scripts, restarts every PAC Keeper app in `~/apps`, and checks that each one answers. If one does not, it puts the previous program back.

### Backups to S3

`~/bin/pac-keeper-backup` (the `backup` script, which `upgrade` installs) backs up every app that is not the demo. For each app, it puts a dated copy in `~/pac-keeper-backups/APPNAME/` and keeps the newest 7. Then it sends the new copies to an S3 bucket with [restic](https://restic.net). Restic encrypts the backups before they leave the server. It stores a file that does not change only once, so each receipt uses space one time. The script keeps 14 daily, 8 weekly, and 12 monthly snapshots.

Set it up once:

1. **The bucket.** Run these on your computer, with the AWS CLI signed in to your AWS account. Use a bucket name of your own. Versioning keeps each deleted or replaced file for 30 more days, so a stolen key cannot destroy the backups. New buckets block public access.

   ```sh
   BUCKET=pac-keeper-backups-EXAMPLE
   aws s3api create-bucket --bucket $BUCKET --region ca-central-1 \
     --create-bucket-configuration LocationConstraint=ca-central-1
   aws s3api put-bucket-versioning --bucket $BUCKET --versioning-configuration Status=Enabled
   aws s3api put-bucket-lifecycle-configuration --bucket $BUCKET --lifecycle-configuration '{"Rules": [{
     "ID": "old-versions", "Status": "Enabled", "Filter": {},
     "NoncurrentVersionExpiration": {"NoncurrentDays": 30},
     "Expiration": {"ExpiredObjectDeleteMarker": true},
     "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": 7}}]}'
   ```

2. **A user for the server.** It can read and write files in the bucket only. It cannot delete old versions or change the bucket's settings. The last command shows the access key.

   ```sh
   aws iam create-user --user-name pac-keeper-backup
   aws iam put-user-policy --user-name pac-keeper-backup --policy-name bucket --policy-document '{
     "Version": "2012-10-17",
     "Statement": [
       {"Effect": "Allow", "Action": ["s3:ListBucket", "s3:GetBucketLocation"], "Resource": "arn:aws:s3:::'$BUCKET'"},
       {"Effect": "Allow", "Action": ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"], "Resource": "arn:aws:s3:::'$BUCKET'/*"}]}'
   aws iam create-access-key --user-name pac-keeper-backup
   ```

3. **Restic.** On the server, install the program. `self-update` then gets the newest release.

   ```sh
   cd ~/downloads
   curl -LO https://github.com/restic/restic/releases/download/v0.18.0/restic_0.18.0_linux_amd64.bz2
   bunzip2 restic_0.18.0_linux_amd64.bz2
   install -m 755 restic_0.18.0_linux_amd64 ~/bin/restic
   ~/bin/restic self-update
   ```

4. **The settings.** Copy `backup.env.example` from the release package in `~/downloads` to `~/pac-keeper-backup.env`, and run `chmod 600 ~/pac-keeper-backup.env`. Fill in the bucket name, the access key, and a new password (`openssl rand -base64 32` makes one). Then make the repository and run the first backup:

   ```sh
   (set -a; . ~/pac-keeper-backup.env; ~/bin/restic init)
   ~/bin/pac-keeper-backup
   ```

5. **Your password manager.** The backups do not hold these things. Keep them there:
   - `RESTIC_PASSWORD`. Without it, nobody can read the backups.
   - `PAC_DATA_KEY`, from `~/apps/pk_app/env`, if the records are encrypted (see [Encrypted records](#encrypted-records)). Restic needs `RESTIC_PASSWORD`, and the records in the backups also need this key.
   - The server's `secret.key`, as text: `base64 ~/pac-keeper-data/hosted/secret.key`. The backup leaves it out on purpose, because it opens the PACs' sealed Helcim tokens. Without it, a restored server works, but every person must sign in again and every PAC must enter its Helcim token again.
   - The secrets in `~/apps/pk_app/env`, such as the SMTP password and the Stripe keys.

6. **An alert.** On [Healthchecks.io](https://healthchecks.io), make a check with a period of 1 day and a grace time of 2 hours. Put its ping URL in `PAC_BACKUP_PING_URL`. You then get an email when a backup fails or does not run.

7. **Every night.** Add this line with `crontab -e`:

   ```
   30 22 * * * $HOME/bin/pac-keeper-backup >> $HOME/pac-keeper-backups/backup.log 2>&1
   ```

   Cron uses the server's time zone (`date` shows it). The line expects UTC. Opalstack makes its own copy of the server at about 23:00 UTC, so that copy also gets the new backup.

**Restore.** List the snapshots, and get the newest one back into `~/restore`:

```sh
(set -a; . ~/pac-keeper-backup.env; ~/bin/restic snapshots)
(set -a; . ~/pac-keeper-backup.env; ~/bin/restic restore latest --target ~/restore)
```

The records are in `~/restore/home/USER/pac-keeper-backups/pk_app/DATE/`: `control.db`, and `pacs/` with each PAC's database, receipts, and bank files. To put back all the records, stop the app, move its data folder away, and copy the restored folder to its place. Then put back `secret.key` (`base64 -d > secret.key` and `chmod 600 secret.key`), and start the app. For encrypted records, the app's `env` must hold the same `PAC_DATA_KEY` as when the backup was made. To put back one PAC only, stop the app and replace only that PAC's folder in `pacs/`.

Test a restore every few months. Restore as above, then run `(set -a; . ~/apps/pk_app/env; ~/bin/pac-keeper --data-dir ~/restore/home/USER/pac-keeper-backups/pk_app/DATE stats)`. It opens `control.db` with the app's data key and prints the counts. Then delete `~/restore`.

### Automatic deploys

`.github/workflows/deploy.yml` deploys each release when you publish it on GitHub. It copies `site/` to `pk_site`, then runs the `upgrade` script on the server, and last checks the three public addresses. You can also run it from the Actions tab for any release tag.

Set it up once:

1. Make a key for GitHub only, with no passphrase:

   ```sh
   ssh-keygen -t ed25519 -N "" -C "packeeper github deploy" -f ~/.ssh/packeeper_deploy
   ```

2. Let the key into the server, limited to commands and file copies (`restrict` turns off shells with a terminal and port forwarding):

   ```sh
   (printf 'restrict '; cat ~/.ssh/packeeper_deploy.pub) | ssh pk_user@opal1.opalstack.com 'cat >> ~/.ssh/authorized_keys'
   ```

3. Give the private key to GitHub, then delete your copy:

   ```sh
   gh secret set OPALSTACK_SSH_KEY --repo sjhewitt/packeeper < ~/.ssh/packeeper_deploy
   rm ~/.ssh/packeeper_deploy
   ```

The workflow connects only to the server whose key it pins (`KNOWN_HOST` in the workflow). To approve each deploy before it runs, add a required reviewer under **Settings → Environments → opalstack**.

## Command reference

```
pac-keeper [--data-dir DIR] [serve [--bind ADDR] [--auth] [--demo [--demo-reset-minutes N]]]
pac-keeper --data-dir DIR serve --hosted --base-url URL [--quota-mb N]
pac-keeper --data-dir DIR operator EMAIL [--name NAME]
pac-keeper --data-dir DIR backup --to BACKUP_DIR
pac-keeper --data-dir DIR stats [--now]
pac-keeper new-data-key
pac-keeper user add NAME [--role treasurer|viewer] [--name "Display name"]
pac-keeper user list
pac-keeper user remove NAME
pac-keeper pdf monthly 2026-09 [-o file.pdf]
pac-keeper pdf meeting 2026-10-14 [-o file.pdf]
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

A build also makes the update files, so it needs the updater's private key. Set `TAURI_SIGNING_PRIVATE_KEY` to the key file (or its text) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` to its password. Without the key, add `--config '{"bundle":{"createUpdaterArtifacts":false}}'` to the build command. A development build (`tauri dev`) looks for new versions when you click Check now, but never installs them.

`.github/workflows/release.yml` ("Release builds") builds both apps, and the Linux server program, on GitHub Actions. Run it from the Actions tab to get the files as build artifacts. Push a tag such as `v0.2.0` to also make a draft release with the files attached. Check the draft, then publish it.

Publishing a release also sends it to the installed apps: they read `latest.json` from the newest published release. The workflow signs the update files with the key in the secrets `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Its public half is `plugins.updater.pubkey` in `desktop/tauri.conf.json`. Keep a copy of the key and its password in a safe place. If you lose them, the installed apps cannot update, and each treasurer must install the next version by hand.

To sign the apps later, add an Apple Developer ID certificate and notarization secrets for macOS, and a code-signing certificate for Windows. See the Tauri guides on [macOS signing](https://tauri.app/distribute/sign/macos/) and [Windows signing](https://tauri.app/distribute/sign/windows/).

See `CLAUDE.md` for the code layout.
