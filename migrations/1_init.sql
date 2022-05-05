CREATE TABLE IF NOT EXISTS items
(
    id integer primary key autoincrement,
    name text,
    calories real,
    multiplier real,
    date text, -- stored as 'YYYY-MM-DD'
    timestamp integer
);

CREATE INDEX IF NOT EXISTS idx_items_date on items (date);