-- The failed logins per username (Plan.md P7.1, P7.10, §7.4; ADR-0016). Never edit this
-- file once it's on main: sqlx records its checksum, and `just migrations-check` refuses a
-- change. Add a new migration instead (ADR-0015).
--
-- One row per username with a running failure streak. It's keyed by an HMAC of the
-- normalized name under the server's lockout key, never by the name itself: people type
-- their password into the username field. A name that belongs to no user gets a row exactly
-- like a real user's, so the lockout can't tell anyone which names exist; that's also why no
-- user ID is stored. fleet_core::auth decides when a streak locks and when it has expired,
-- and the server deletes expired rows.
CREATE TABLE login_attempts (
    -- HMAC-SHA256 of the normalized username.
    username_key BLOB NOT NULL PRIMARY KEY CHECK (length(username_key) = 32),
    -- The failures of the current streak.
    failures INTEGER NOT NULL CHECK (failures >= 1),
    -- The streak's last failure; the streak expires a fixed time after it.
    last_failure_at TEXT NOT NULL CHECK (
        last_failure_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- When the running lock ends; NULL until the streak reaches the threshold.
    locked_until TEXT CHECK (
        locked_until IS NULL
        OR locked_until GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    )
) STRICT;

-- The purge of expired streaks.
CREATE INDEX login_attempts_last_failure_at ON login_attempts (last_failure_at);
