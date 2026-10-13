-- Fundraisers that come back every year, such as Purdy's chocolates or hot
-- lunch. A fundraiser is not tied to a year: its status is the total of its
-- transactions in each fiscal year.
CREATE TABLE fundraisers (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    description TEXT NOT NULL DEFAULT '',
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL
);

-- The net amount a fundraiser aims to raise in one fiscal year. Optional.
CREATE TABLE fundraiser_goals (
    fundraiser_id  INTEGER NOT NULL REFERENCES fundraisers (id) ON DELETE CASCADE,
    fiscal_year_id INTEGER NOT NULL REFERENCES fiscal_years (id) ON DELETE CASCADE,
    goal_cents     INTEGER NOT NULL CHECK (goal_cents >= 0),
    PRIMARY KEY (fundraiser_id, fiscal_year_id)
);

-- The fundraiser that one part of a transaction belongs to.
ALTER TABLE allocations ADD COLUMN fundraiser_id INTEGER REFERENCES fundraisers (id) ON DELETE SET NULL;
CREATE INDEX allocations_fundraiser ON allocations (fundraiser_id);

-- A rule can also assign the fundraiser.
ALTER TABLE rules ADD COLUMN fundraiser_id INTEGER REFERENCES fundraisers (id) ON DELETE SET NULL;
