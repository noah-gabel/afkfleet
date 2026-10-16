-- The agents' enrollment tokens (Plan.md P10.3, Appendix D; ADR-0019). Never edit this file
-- once it's on main: sqlx records its checksum, and `just migrations-check` refuses a change.
-- Add a new migration instead (ADR-0015).
--
-- A token stands for one new agent: the Owner creates it (or the server's host operator, with
-- `afkfleet-server agent create-enrollment-token`), it lives 15 minutes, and enrolling uses it
-- once. The token itself is never stored, only its SHA-256 hash. A used token keeps its row,
-- with the agent it enrolled.
CREATE TABLE enrollment_tokens (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- SHA-256 of the token's text (`afk_et_…`), prefix included. The token has 256 bits from the
    -- server's secure random source, so a plain hash is safe against a leaked database.
    token_hash BLOB NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    -- The Owner who created it; NULL when the server's host operator did, on the command line.
    -- No cascade: users are never deleted.
    created_by TEXT REFERENCES users (id) CHECK (
        created_by IS NULL OR (
            length(created_by) = 36
            AND substr(created_by, 9, 1) = '-'
            AND substr(created_by, 14, 1) = '-'
            AND substr(created_by, 19, 1) = '-'
            AND substr(created_by, 24, 1) = '-'
            AND length(replace(created_by, '-', '')) = 32
            AND replace(created_by, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- Expired from its exact millisecond on.
    expires_at TEXT NOT NULL CHECK (
        expires_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- When it enrolled an agent; NULL while it's unused.
    used_at TEXT CHECK (
        used_at IS NULL
        OR used_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- The agent it enrolled; NULL while it's unused. One token per agent.
    agent_id TEXT UNIQUE REFERENCES agents (id) CHECK (
        agent_id IS NULL OR (
            length(agent_id) = 36
            AND substr(agent_id, 9, 1) = '-'
            AND substr(agent_id, 14, 1) = '-'
            AND substr(agent_id, 19, 1) = '-'
            AND substr(agent_id, 24, 1) = '-'
            AND length(replace(agent_id, '-', '')) = 32
            AND replace(agent_id, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    -- A belt behind the service: the agent and the time of the use come together.
    CHECK ((used_at IS NULL) = (agent_id IS NULL)),
    -- It expires after it's created; the fixed time format compares as text.
    CHECK (expires_at > created_at)
) STRICT;

-- Counting the pending tokens (the cap), and P11.6's cleanup of expired ones.
CREATE INDEX enrollment_tokens_expires_at ON enrollment_tokens (expires_at);
