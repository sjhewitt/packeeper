-- Licensed gaming events (bingo, ticket raffles, and others) for lines 6 to 13
-- and Section 4 of the Gaming Account Summary Report.
CREATE TABLE gaming_events (
    id             INTEGER PRIMARY KEY,
    name           TEXT NOT NULL,
    licence_number TEXT NOT NULL,
    licence_class  TEXT NOT NULL CHECK (licence_class IN ('A', 'B', 'C', 'D')),
    event_type     TEXT NOT NULL CHECK (event_type IN ('bingo', 'ticket_raffle', 'other')),
    other_type     TEXT NOT NULL DEFAULT '',
    start_date     TEXT NOT NULL,
    end_date       TEXT NOT NULL,
    notes          TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL,
    CHECK (start_date <= end_date)
);

-- The licensed event that a gaming-account transaction belongs to: its revenue,
-- a prize, or an expense. The line item's gasr_line says which.
ALTER TABLE transactions ADD COLUMN gaming_event_id INTEGER REFERENCES gaming_events (id) ON DELETE SET NULL;
CREATE INDEX transactions_gaming_event ON transactions (gaming_event_id);
