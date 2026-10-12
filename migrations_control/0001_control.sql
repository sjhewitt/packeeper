-- The control database of a hosted server: which PACs exist, who the people
-- are, and who belongs to which PAC. Each PAC's records live in their own
-- database under pacs/<folder>/pac.db.

CREATE TABLE people (
    id                   INTEGER PRIMARY KEY,
    email                TEXT NOT NULL UNIQUE COLLATE NOCASE,
    name                 TEXT NOT NULL,
    -- NULL until the person sets a password (for example, from an invitation).
    password_hash        TEXT,
    email_verified_at    TEXT,
    is_operator          INTEGER NOT NULL DEFAULT 0,
    -- Changing it signs the person out everywhere (after a password reset).
    session_version      INTEGER NOT NULL DEFAULT 1,
    created_at           TEXT NOT NULL
);

CREATE TABLE pacs (
    id           INTEGER PRIMARY KEY,
    -- The folder under pacs/. Random, so it reveals nothing.
    folder       TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    school       TEXT NOT NULL,
    district     TEXT NOT NULL DEFAULT '',
    status       TEXT NOT NULL CHECK (status IN ('requested', 'active', 'suspended', 'rejected')),
    requested_by INTEGER REFERENCES people (id) ON DELETE SET NULL,
    -- The applicant's message to the operator.
    message      TEXT NOT NULL DEFAULT '',
    created_at   TEXT NOT NULL,
    decided_at   TEXT
);

CREATE TABLE memberships (
    pac_id     INTEGER NOT NULL REFERENCES pacs (id) ON DELETE CASCADE,
    person_id  INTEGER NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    role       TEXT NOT NULL CHECK (role IN ('treasurer', 'viewer')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (pac_id, person_id)
);
CREATE INDEX memberships_person ON memberships (person_id);

-- One-time links sent by email. Only a hash of the token is stored.
CREATE TABLE tokens (
    token_hash TEXT PRIMARY KEY,
    purpose    TEXT NOT NULL CHECK (purpose IN ('verify', 'reset', 'invite')),
    person_id  INTEGER NOT NULL REFERENCES people (id) ON DELETE CASCADE,
    pac_id     INTEGER REFERENCES pacs (id) ON DELETE CASCADE,
    role       TEXT,
    expires_at TEXT NOT NULL,
    used_at    TEXT
);
