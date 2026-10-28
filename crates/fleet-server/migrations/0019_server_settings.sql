-- The servers' settings: what every bot on one Minecraft server shares, whoever's bot it is
-- (Plan.md P11.9, Appendix D; ADR-0020, S74-S78). Never edit this file once it's on main: sqlx
-- records its checksum, and `just migrations-check` refuses a change. Add a new migration instead
-- (ADR-0015).
--
-- A row is one server's, keyed by fleet-core's ServerKey text: the host and a port that's always
-- written, lowercase (S74), read back only when it's exactly that canonical form. Its conflict
-- texts are fleet-core's ConflictTexts as a JSON array of strings, sorted and without duplicates
-- as the set iterates, read back only through its constructor. The fleet holds at most 100 rows
-- (fleet-core's MAX_SERVER_SETTINGS), which a create counts in its own transaction. A row always
-- holds something: the routes refuse a write that leaves it empty, and only a delete ends a row
-- (S78). No CHECK pins that, since what "something" means grows with the chat format.
CREATE TABLE server_settings (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- The server's key: `host:port`, or `[ipv6]:port`. One row per server.
    server TEXT NOT NULL UNIQUE CHECK (server = lower(server)),
    -- The kick texts that count as someone logging in with a bot's account (ADR-0013).
    conflict_texts TEXT NOT NULL CHECK (
        json_valid(conflict_texts) AND json_type(conflict_texts) = 'array'
    ),
    -- 1 when it's created, one more on every change.
    version INTEGER NOT NULL CHECK (version >= 1),
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    updated_at TEXT NOT NULL CHECK (
        updated_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- The fixed time format compares as text.
    CHECK (updated_at >= created_at)
) STRICT;
