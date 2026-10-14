-- PAC meetings. The treasurer's report for a meeting covers the days from the
-- previous meeting up to the day before it; see src/meetings.rs.
CREATE TABLE meetings (
    id   INTEGER PRIMARY KEY,
    date TEXT NOT NULL UNIQUE,
    note TEXT NOT NULL DEFAULT ''
);

-- The meeting rule fills in each fiscal year once; after that the treasurer
-- changes single dates.
ALTER TABLE fiscal_years ADD COLUMN meetings_filled INTEGER NOT NULL DEFAULT 0;
