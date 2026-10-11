-- The sessions (Plan.md P7.1, P7.3, §7.2; ADR-0006, ADR-0016). Never edit this file once
-- it's on main: sqlx records its checksum, and `just migrations-check` refuses a change.
-- Add a new migration instead (ADR-0015).
--
-- One row per login: a token family, whose ID stays the same across every refresh. The
-- row holds the SHA-256 hashes of the current access and refresh tokens, never a token.
-- No expiry is stored: fleet_core::auth computes it from created_at (absolute) and
-- refreshed_at (access and idle) with the running config's lifetimes. No client data
-- (no IP address, no user agent).
CREATE TABLE sessions (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- No cascade: users are never deleted, as for the audit log's actor.
    user_id TEXT NOT NULL REFERENCES users (id) CHECK (
        length(user_id) = 36
        AND substr(user_id, 9, 1) = '-'
        AND substr(user_id, 14, 1) = '-'
        AND substr(user_id, 19, 1) = '-'
        AND substr(user_id, 24, 1) = '-'
        AND length(replace(user_id, '-', '')) = 32
        AND replace(user_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- SHA-256 of the current access token's text, prefix included.
    access_hash BLOB NOT NULL UNIQUE CHECK (length(access_hash) = 32),
    -- SHA-256 of the current refresh token's text, prefix included.
    refresh_hash BLOB NOT NULL UNIQUE CHECK (length(refresh_hash) = 32),
    -- When the session was created: the anchor of its absolute expiry.
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- When the current token pair was issued: the anchor of the access token's expiry
    -- and of the idle expiry.
    refreshed_at TEXT NOT NULL CHECK (
        refreshed_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- The last authenticated request, written at most once a minute. Display only.
    last_used_at TEXT NOT NULL CHECK (
        last_used_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- When the password was last entered again (step-up, §7.2); NULL until then.
    stepped_up_at TEXT CHECK (
        stepped_up_at IS NULL
        OR stepped_up_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- When the session was revoked; NULL while it's live. Why is in the audit log.
    revoked_at TEXT CHECK (
        revoked_at IS NULL
        OR revoked_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    )
) STRICT;

-- Revoking all of a user's sessions (P7.9) and listing them (/me/sessions).
CREATE INDEX sessions_user_id ON sessions (user_id);
-- The cleanup of sessions past their absolute lifetime (P11.6).
CREATE INDEX sessions_created_at ON sessions (created_at);

-- Every refresh token a session has replaced, so presenting one again is recognized as
-- reuse and revokes the whole family (§7.2). The rows go with their session.
CREATE TABLE rotated_refresh_tokens (
    -- SHA-256 of the replaced refresh token's text.
    hash BLOB NOT NULL PRIMARY KEY CHECK (length(hash) = 32),
    session_id TEXT NOT NULL REFERENCES sessions (id) ON DELETE CASCADE CHECK (
        length(session_id) = 36
        AND substr(session_id, 9, 1) = '-'
        AND substr(session_id, 14, 1) = '-'
        AND substr(session_id, 19, 1) = '-'
        AND substr(session_id, 24, 1) = '-'
        AND length(replace(session_id, '-', '')) = 32
        AND replace(session_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- When it was replaced.
    rotated_at TEXT NOT NULL CHECK (
        rotated_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    )
) STRICT;

-- The cascade from sessions, and a family's history.
CREATE INDEX rotated_refresh_tokens_session_id ON rotated_refresh_tokens (session_id);
