-- All money is stored as integer cents. All dates are ISO 'YYYY-MM-DD' text.

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- A bank account. 'gaming' accounts hold BC Community Gaming Grant funds and
-- may only use line items from the gaming fund.
CREATE TABLE accounts (
    id                    INTEGER PRIMARY KEY,
    name                  TEXT NOT NULL,
    kind                  TEXT NOT NULL CHECK (kind IN ('general', 'gaming')),
    institution           TEXT NOT NULL DEFAULT 'Prospera Credit Union',
    account_number        TEXT NOT NULL DEFAULT '',
    opening_date          TEXT NOT NULL,
    opening_balance_cents INTEGER NOT NULL DEFAULT 0,
    csv_mapping           TEXT,
    active                INTEGER NOT NULL DEFAULT 1,
    notes                 TEXT NOT NULL DEFAULT ''
);

CREATE TABLE fiscal_years (
    id                 INTEGER PRIMARY KEY,
    label              TEXT NOT NULL UNIQUE,
    start_date         TEXT NOT NULL,
    end_date           TEXT NOT NULL,
    budget_status      TEXT NOT NULL DEFAULT 'draft'
                       CHECK (budget_status IN ('draft', 'proposed', 'approved')),
    budget_approved_on TEXT,
    notes              TEXT NOT NULL DEFAULT '',
    CHECK (start_date < end_date)
);

-- A budget line. Line items persist across years so that years compare.
-- 'transfer' lines move money between accounts of the same fund. They are not
-- income or expense. gasr_line maps gaming income to the Gaming Account Summary
-- Report: '3' is the PAC grant, '14' is GST rebates, interest, and asset sales.
CREATE TABLE line_items (
    id              INTEGER PRIMARY KEY,
    name            TEXT NOT NULL,
    fund            TEXT NOT NULL CHECK (fund IN ('general', 'gaming')),
    kind            TEXT NOT NULL CHECK (kind IN ('income', 'expense', 'transfer')),
    gasr_line       TEXT,
    receipt_required INTEGER NOT NULL DEFAULT 1,
    description     TEXT NOT NULL DEFAULT '',
    sort_order      INTEGER NOT NULL DEFAULT 0,
    active          INTEGER NOT NULL DEFAULT 1,
    UNIQUE (fund, name)
);

-- The budgeted amount for one line item in one fiscal year. Always >= 0.
CREATE TABLE budget_amounts (
    fiscal_year_id INTEGER NOT NULL REFERENCES fiscal_years (id) ON DELETE CASCADE,
    line_item_id   INTEGER NOT NULL REFERENCES line_items (id) ON DELETE CASCADE,
    amount_cents   INTEGER NOT NULL DEFAULT 0,
    notes          TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (fiscal_year_id, line_item_id)
);

CREATE TABLE import_batches (
    id             INTEGER PRIMARY KEY,
    account_id     INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    filename       TEXT NOT NULL,
    stored_name    TEXT NOT NULL,
    format         TEXT NOT NULL CHECK (format IN ('csv', 'ofx')),
    status         TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'done')),
    created_at     TEXT NOT NULL,
    rows_total     INTEGER NOT NULL DEFAULT 0,
    rows_new       INTEGER NOT NULL DEFAULT 0,
    rows_duplicate INTEGER NOT NULL DEFAULT 0,
    rows_matched   INTEGER NOT NULL DEFAULT 0
);

-- amount_cents is negative for money out of the account.
-- reference is the cheque number, e-transfer or EFT reference.
-- purpose is the short reason for the payment. The gaming report prints it.
-- bank_ref is the OFX FITID or a fingerprint of the CSV row. It stops duplicates.
-- cleared = 0 marks a manual entry (such as a cheque) that the bank has not shown yet.
-- reimbursed_by_id links a general-account expense to the gaming-account
-- transfer that repaid it. The gaming report prints these as the reimbursement summary.
CREATE TABLE transactions (
    id                 INTEGER PRIMARY KEY,
    account_id         INTEGER NOT NULL REFERENCES accounts (id),
    date               TEXT NOT NULL,
    description        TEXT NOT NULL,
    payee              TEXT NOT NULL DEFAULT '',
    amount_cents       INTEGER NOT NULL,
    reference          TEXT NOT NULL DEFAULT '',
    purpose            TEXT NOT NULL DEFAULT '',
    bank_balance_cents INTEGER,
    source             TEXT NOT NULL CHECK (source IN ('import', 'manual')),
    import_batch_id    INTEGER REFERENCES import_batches (id) ON DELETE SET NULL,
    bank_ref           TEXT,
    cleared            INTEGER NOT NULL DEFAULT 1,
    reimbursed_by_id   INTEGER REFERENCES transactions (id) ON DELETE SET NULL,
    notes              TEXT NOT NULL DEFAULT '',
    created_at         TEXT NOT NULL,
    UNIQUE (account_id, bank_ref)
);
CREATE INDEX transactions_reimbursed_by ON transactions (reimbursed_by_id);
CREATE INDEX transactions_account_date ON transactions (account_id, date);
CREATE INDEX transactions_date ON transactions (date);

-- Assigns all or part of a transaction to a line item. Same sign as the transaction.
CREATE TABLE allocations (
    id             INTEGER PRIMARY KEY,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    line_item_id   INTEGER NOT NULL REFERENCES line_items (id),
    amount_cents   INTEGER NOT NULL,
    memo           TEXT NOT NULL DEFAULT ''
);
CREATE INDEX allocations_transaction ON allocations (transaction_id);
CREATE INDEX allocations_line_item ON allocations (line_item_id);

-- Receipts and other documents. Files live in <data>/receipts/<stored_name>.
CREATE TABLE attachments (
    id             INTEGER PRIMARY KEY,
    transaction_id INTEGER NOT NULL REFERENCES transactions (id) ON DELETE CASCADE,
    filename       TEXT NOT NULL,
    stored_name    TEXT NOT NULL,
    content_type   TEXT NOT NULL,
    size_bytes     INTEGER NOT NULL,
    sha256         TEXT NOT NULL,
    uploaded_at    TEXT NOT NULL
);
CREATE INDEX attachments_transaction ON attachments (transaction_id);

-- Auto-assign rules. pattern is a case-insensitive substring of the description.
CREATE TABLE rules (
    id           INTEGER PRIMARY KEY,
    pattern      TEXT NOT NULL,
    account_id   INTEGER REFERENCES accounts (id) ON DELETE CASCADE,
    line_item_id INTEGER NOT NULL REFERENCES line_items (id) ON DELETE CASCADE,
    payee        TEXT NOT NULL DEFAULT ''
);

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL DEFAULT '',
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL CHECK (role IN ('treasurer', 'viewer')),
    created_at    TEXT NOT NULL
);
