-- A PAC's own Helcim account for hot lunch card payments. The API token is
-- sealed with a key made from the server's secret key; see lunch::helcim.
CREATE TABLE pac_helcim (
    pac_id       INTEGER PRIMARY KEY REFERENCES pacs (id) ON DELETE CASCADE,
    token_sealed TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
