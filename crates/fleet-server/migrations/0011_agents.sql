-- The agents and their certificates (Plan.md P10.2, P10.3, Appendix D; ADR-0019). Never edit
-- this file once it's on main: sqlx records its checksum, and `just migrations-check` refuses
-- a change. Add a new migration instead (ADR-0015).
--
-- An agent gets its identity by enrolling: the server signs a certificate for the key the agent
-- made, with the agent's ID from this table in its SAN, and keeps the certificate's SHA-256
-- here. Connecting (group C) needs a certificate whose fingerprint is listed, unrevoked and
-- unexpired, of an enabled agent. Rows are never deleted: disabling an agent revokes its
-- certificates and keeps the history.
CREATE TABLE agents (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- The agent's own config name, sent when it enrolled. Its length and characters stay in
    -- Rust (fleet-core's AgentName, ASCII only), as for users.username.
    name TEXT NOT NULL,
    -- 1 while the agent may connect; disabling it (P10.10) sets 0.
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    -- What the agent's last hello reported (group C): its capacity, NULL until the first hello,
    -- with no range CHECK (fleet-core's AgentCapacity reads it back; ADR-0019, S8), and its
    -- version, also NULL until then.
    capacity INTEGER,
    version TEXT,
    -- When the server last heard from it, by the server's own clock; NULL until it connects.
    last_seen_at TEXT CHECK (
        last_seen_at IS NULL
        OR last_seen_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    )
) STRICT;

-- Two enabled agents never share a name, whatever its case, so an Admin can tell them apart and
-- disable the right one. A disabled agent frees its name for the one that replaces it.
CREATE UNIQUE INDEX agents_enabled_name ON agents (lower(name)) WHERE enabled = 1;

-- The certificates the server signed for its agents, by fingerprint.
CREATE TABLE agent_certs (
    -- SHA-256 of the certificate's DER: what a connecting agent's certificate is looked up by.
    fingerprint_sha256 BLOB NOT NULL PRIMARY KEY CHECK (length(fingerprint_sha256) = 32),
    -- The agent the certificate names in its SAN. Not unique: a renewing agent holds two
    -- certificates for a while (P13.6; ADR-0019, S1). No cascade: agents are never deleted.
    agent_id TEXT NOT NULL REFERENCES agents (id) CHECK (
        length(agent_id) = 36
        AND substr(agent_id, 9, 1) = '-'
        AND substr(agent_id, 14, 1) = '-'
        AND substr(agent_id, 19, 1) = '-'
        AND substr(agent_id, 24, 1) = '-'
        AND length(replace(agent_id, '-', '')) = 32
        AND replace(agent_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- Exactly the certificate's notAfter, in whole seconds; from then on it's expired.
    not_after TEXT NOT NULL CHECK (
        not_after GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- When it was revoked (P10.10); NULL unless it was. Who and why is in the audit log.
    revoked_at TEXT CHECK (
        revoked_at IS NULL
        OR revoked_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- It expires after it's stored; the fixed time format compares as text.
    CHECK (not_after > created_at)
) STRICT;

-- An agent's certificates: listing them, and revoking every one when it's disabled.
CREATE INDEX agent_certs_agent_id ON agent_certs (agent_id);
