-- Visit counts for the operator, from the web server's logs and GitHub's
-- download counts. Counts only: no addresses, browsers, or paths that could
-- name a person.

CREATE TABLE stat_counts (
    -- A day in UTC, such as 2026-10-13.
    day     TEXT NOT NULL,
    -- site, demo, app, or downloads.
    log     TEXT NOT NULL,
    -- visits, views, phones, signins, inside, signup_views, source, or page;
    -- for downloads: mac, windows, or linux.
    measure TEXT NOT NULL,
    -- The referring host of a source, or the page; empty for other measures.
    name    TEXT NOT NULL DEFAULT '',
    n       INTEGER NOT NULL,
    PRIMARY KEY (day, log, measure, name)
);

-- When each log (or GitHub) was last read, and what went wrong, if anything.
CREATE TABLE stat_checks (
    source     TEXT PRIMARY KEY,
    checked_at TEXT NOT NULL,
    error      TEXT NOT NULL DEFAULT ''
);
