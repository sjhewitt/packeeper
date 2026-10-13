-- Hot lunch ordering. It is behind the hot lunch feature flag and runs on a
-- hosted server. Parents are people in the server's control.db: person_id
-- here is their id there.

CREATE TABLE lunch_divisions (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE COLLATE NOCASE,
    sort_order INTEGER NOT NULL DEFAULT 0,
    active     INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE lunch_items (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    price_cents INTEGER NOT NULL CHECK (price_cents >= 0),
    active      INTEGER NOT NULL DEFAULT 1
);

-- An ordering period, such as "January 2027": its lunch days and the last
-- day to order.
CREATE TABLE lunch_periods (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL,
    order_deadline TEXT NOT NULL,
    status         TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'open', 'closed')),
    notes          TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL
);

CREATE TABLE lunch_days (
    id        INTEGER PRIMARY KEY,
    period_id INTEGER NOT NULL REFERENCES lunch_periods (id) ON DELETE CASCADE,
    date      TEXT NOT NULL,
    UNIQUE (period_id, date)
);

-- The items on offer on one day, at that day's price.
CREATE TABLE lunch_day_items (
    day_id      INTEGER NOT NULL REFERENCES lunch_days (id) ON DELETE CASCADE,
    item_id     INTEGER NOT NULL REFERENCES lunch_items (id),
    price_cents INTEGER NOT NULL CHECK (price_cents >= 0),
    PRIMARY KEY (day_id, item_id)
);

CREATE TABLE lunch_children (
    id          INTEGER PRIMARY KEY,
    person_id   INTEGER NOT NULL,
    name        TEXT NOT NULL,
    division_id INTEGER REFERENCES lunch_divisions (id) ON DELETE SET NULL,
    notes       TEXT NOT NULL DEFAULT '',
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL
);
CREATE INDEX lunch_children_person ON lunch_children (person_id);

-- One order by one parent in one period. A card order is pending until
-- Stripe confirms the payment; an e-transfer or cash order until the
-- treasurer marks it paid.
CREATE TABLE lunch_orders (
    id                    INTEGER PRIMARY KEY,
    person_id             INTEGER NOT NULL,
    period_id             INTEGER NOT NULL REFERENCES lunch_periods (id),
    method                TEXT NOT NULL CHECK (method IN ('card', 'etransfer', 'cash')),
    status                TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'paid', 'cancelled', 'refunded')),
    total_cents           INTEGER NOT NULL CHECK (total_cents >= 0),
    -- Card orders: the fee that PAC Keeper's Stripe takes from the payment.
    fee_cents             INTEGER NOT NULL DEFAULT 0,
    -- The fee includes the month's charge for the PAC's Stripe Express account.
    monthly_fee_included  INTEGER NOT NULL DEFAULT 0,
    stripe_session_id     TEXT,
    stripe_payment_intent TEXT,
    created_at            TEXT NOT NULL,
    paid_at               TEXT
);
CREATE INDEX lunch_orders_period ON lunch_orders (period_id);
CREATE INDEX lunch_orders_person ON lunch_orders (person_id);
CREATE UNIQUE INDEX lunch_orders_session ON lunch_orders (stripe_session_id);

CREATE TABLE lunch_order_lines (
    id          INTEGER PRIMARY KEY,
    order_id    INTEGER NOT NULL REFERENCES lunch_orders (id) ON DELETE CASCADE,
    child_id    INTEGER NOT NULL REFERENCES lunch_children (id),
    day_id      INTEGER NOT NULL REFERENCES lunch_days (id),
    item_id     INTEGER NOT NULL REFERENCES lunch_items (id),
    quantity    INTEGER NOT NULL DEFAULT 1 CHECK (quantity > 0),
    price_cents INTEGER NOT NULL CHECK (price_cents >= 0)
);
CREATE INDEX lunch_order_lines_order ON lunch_order_lines (order_id);
CREATE INDEX lunch_order_lines_day ON lunch_order_lines (day_id);
