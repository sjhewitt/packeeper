-- Parents who order hot lunch from a PAC. A family link opens only the
-- parent pages (/family), never the PAC's books: those need a membership.
CREATE TABLE families (
    pac_id     INTEGER NOT NULL REFERENCES pacs (id) ON DELETE CASCADE,
    person_id  INTEGER NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (pac_id, person_id)
);
CREATE INDEX families_person ON families (person_id);

-- The sign-up link that a PAC shares with its families. One per PAC; a new
-- one replaces the old.
CREATE TABLE family_codes (
    code       TEXT PRIMARY KEY,
    pac_id     INTEGER NOT NULL UNIQUE REFERENCES pacs (id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
);

-- The Stripe account that takes a PAC's hot lunch payments: the PAC's own
-- (standard, connected by OAuth) or one made under the platform (express).
CREATE TABLE pac_stripe (
    pac_id          INTEGER PRIMARY KEY REFERENCES pacs (id) ON DELETE CASCADE,
    account_id      TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('standard', 'express')),
    charges_enabled INTEGER NOT NULL DEFAULT 0,
    updated_at      TEXT NOT NULL
);
