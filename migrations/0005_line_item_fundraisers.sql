-- A general-fund line item can name a fundraiser. A part on that line item
-- belongs to the fundraiser unless the part names another one; see
-- ledger::set_allocations.
ALTER TABLE line_items ADD COLUMN fundraiser_id INTEGER REFERENCES fundraisers (id) ON DELETE SET NULL;
