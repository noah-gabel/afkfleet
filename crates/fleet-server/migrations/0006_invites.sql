-- The invites (Plan.md P7.1, P7.9, §7.2; ADR-0016). Never edit this file once it's on main:
-- sqlx records its checksum, and `just migrations-check` refuses a change. Add a new
-- migration instead (ADR-0015).
--
-- Registration is invite-only. An invite is single-use, carries a fixed role and expires;
-- an Admin or the Owner creates it and sends its code to a friend. The code itself is never
-- stored, only its SHA-256 hash. A used invite keeps its row (who invited whom), and a
-- revoked one too: nothing here is deleted until P11.6's cleanup.
CREATE TABLE invites (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- SHA-256 of the code's text (`afk_iv_…`), prefix included. The code has 256 bits from
    -- the server's secure random source, so a plain hash is safe against a leaked database.
    code_hash BLOB NOT NULL UNIQUE CHECK (length(code_hash) = 32),
    -- The role the new user gets. Never the Owner: an Owner invite can't exist.
    role TEXT NOT NULL CHECK (role IN ('member', 'admin')),
    -- Who created it. No cascade: users are never deleted, as for the audit log's actor.
    created_by TEXT NOT NULL REFERENCES users (id) CHECK (
        length(created_by) = 36
        AND substr(created_by, 9, 1) = '-'
        AND substr(created_by, 14, 1) = '-'
        AND substr(created_by, 19, 1) = '-'
        AND substr(created_by, 24, 1) = '-'
        AND length(replace(created_by, '-', '')) = 32
        AND replace(created_by, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- Stored at creation from `[auth] invite_ttl_hours`, so a later config change doesn't
    -- move it. Expired from its exact millisecond on.
    expires_at TEXT NOT NULL CHECK (
        expires_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- The user it registered; NULL while it's unused. One invite per user.
    used_by TEXT UNIQUE REFERENCES users (id) CHECK (
        used_by IS NULL OR (
            length(used_by) = 36
            AND substr(used_by, 9, 1) = '-'
            AND substr(used_by, 14, 1) = '-'
            AND substr(used_by, 19, 1) = '-'
            AND substr(used_by, 24, 1) = '-'
            AND length(replace(used_by, '-', '')) = 32
            AND replace(used_by, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    -- When it registered that user; NULL while it's unused.
    used_at TEXT CHECK (
        used_at IS NULL
        OR used_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- When it was revoked; NULL unless it was. Who and why is in the audit log.
    revoked_at TEXT CHECK (
        revoked_at IS NULL
        OR revoked_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- A belt behind the services. A user and the time of the use come together.
    CHECK ((used_by IS NULL) = (used_at IS NULL)),
    -- Only an unused invite can be revoked, and a revoked one can't be used.
    CHECK (used_at IS NULL OR revoked_at IS NULL),
    -- It expires after it's created; the fixed time format compares as text.
    CHECK (expires_at > created_at)
) STRICT;

-- A creator's invites: counting their pending ones (the cap) and revoking them when they're
-- disabled or demoted.
CREATE INDEX invites_created_by ON invites (created_by);
-- P11.6's cleanup of expired invites.
CREATE INDEX invites_expires_at ON invites (expires_at);
