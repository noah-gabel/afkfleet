-- The audit log (Plan.md Appendix D, P6.4, P6.8). Never edit this file once it's on main:
-- sqlx records its checksum, and `just migrations-check` refuses a change. Add a new
-- migration instead (ADR-0015).
--
-- Append-only through the code (the ports offer only record and list). No trigger
-- blocks DELETE: P11.6's retention job deletes old entries.
CREATE TABLE audit_log (
    -- An append-only log's ID: with one writer, the order of IDs is exactly the order of
    -- commits, and AUTOINCREMENT never reuses an ID after retention deletes (ADR-0015).
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- When it happened, from the server's Clock. Informational: the order is the ID.
    at TEXT NOT NULL CHECK (
        at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- Who did it; NULL for the system or a login with an unknown username. A user who
    -- appears here can't be deleted (no cascade).
    actor_user_id TEXT REFERENCES users (id) CHECK (
        actor_user_id IS NULL OR (
            length(actor_user_id) = 36
            AND substr(actor_user_id, 9, 1) = '-'
            AND substr(actor_user_id, 14, 1) = '-'
            AND substr(actor_user_id, 19, 1) = '-'
            AND substr(actor_user_id, 24, 1) = '-'
            AND length(replace(actor_user_id, '-', '')) = 32
            AND replace(actor_user_id, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    -- The client's canonical IP address (an IPv4-mapped address as IPv4).
    actor_ip TEXT CHECK (actor_ip IS NULL OR length(actor_ip) BETWEEN 3 AND 45),
    -- No CHECK on action and target_type: every later phase adds values, and the Rust
    -- types validate them.
    action TEXT NOT NULL,
    target_type TEXT,
    target_id TEXT CHECK (
        target_id IS NULL OR (
            length(target_id) = 36
            AND substr(target_id, 9, 1) = '-'
            AND substr(target_id, 14, 1) = '-'
            AND substr(target_id, 19, 1) = '-'
            AND substr(target_id, 24, 1) = '-'
            AND length(replace(target_id, '-', '')) = 32
            AND replace(target_id, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'failure', 'denied')),
    -- A JSON object, or NULL for none. Its 4 KiB limit is checked in Rust, so it can
    -- change without rebuilding the table.
    metadata_json TEXT CHECK (
        metadata_json IS NULL
        OR (json_valid(metadata_json) AND json_type(metadata_json) = 'object')
    ),
    CHECK ((target_type IS NULL) = (target_id IS NULL))
) STRICT;
