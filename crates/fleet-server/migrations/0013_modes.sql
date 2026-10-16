-- The modes: what a bot does while it's online (Plan.md P11.1, Appendix D; ADR-0010, ADR-0019).
-- Never edit this file once it's on main: sqlx records its checksum, and `just migrations-check`
-- refuses a change. Add a new migration instead (ADR-0015).
--
-- A mode is a list of steps, stored as the JSON fleet-core's ModeDefinition writes and read
-- back only through its validated deserializer. The built-in modes have no owner and are
-- read-only for everyone; the users' own modes arrive with their routes (P11.4), which also
-- decide their names' rules. This migration seeds the two built-in modes with the fixed IDs
-- fleet-core's ModePreset::id names.
CREATE TABLE modes (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- Who made it; NULL for a built-in mode. No cascade: users are never deleted.
    owner_user_id TEXT REFERENCES users (id) CHECK (
        owner_user_id IS NULL OR (
            length(owner_user_id) = 36
            AND substr(owner_user_id, 9, 1) = '-'
            AND substr(owner_user_id, 14, 1) = '-'
            AND substr(owner_user_id, 19, 1) = '-'
            AND substr(owner_user_id, 24, 1) = '-'
            AND length(replace(owner_user_id, '-', '')) = 32
            AND replace(owner_user_id, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    -- Its name. Its rules (length, characters, uniqueness) arrive with P11.4.
    name TEXT NOT NULL,
    -- Who may see it (fleet-core's ModeVisibility): a closed set that authorize() matches
    -- exhaustively, so a third value needs a new authorization decision anyway.
    visibility TEXT NOT NULL CHECK (visibility IN ('private', 'shared')),
    -- The mode's steps, as fleet-core's ModeDefinition writes them.
    definition_json TEXT NOT NULL CHECK (
        json_valid(definition_json) AND json_type(definition_json) = 'object'
    ),
    -- 1 when it's created, one more on every change.
    version INTEGER NOT NULL CHECK (version >= 1),
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    updated_at TEXT NOT NULL CHECK (
        updated_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- A built-in mode is seen by everyone.
    CHECK (owner_user_id IS NOT NULL OR visibility = 'shared'),
    -- The fixed time format compares as text.
    CHECK (updated_at >= created_at)
) STRICT;

-- A user's modes, for listing them (P11.4).
CREATE INDEX modes_owner_user_id ON modes (owner_user_id);

-- The built-in modes: ModeDefinition::afk() and farm() exactly as serde_json writes them, which
-- fleet-server's modes_repository test checks, with the IDs ModePreset::id names
-- (2026-10-16T00:00:00.000Z, then a counter).
INSERT INTO modes (id, owner_user_id, name, visibility, definition_json, version, created_at, updated_at)
VALUES
    (
        '01a14202-2800-7000-8000-000000000001',
        NULL,
        'afk',
        'shared',
        '{"steps":[{"action":{"type":"rotate_random","max_yaw":30.0,"max_pitch":10.0},"schedule":{"type":"every","interval_ms":45000,"jitter_ms":75000},"probability":100},{"action":{"type":"swing_arm"},"schedule":{"type":"every","interval_ms":20000,"jitter_ms":20000},"probability":100}]}',
        1,
        '2026-10-16T00:00:00.000Z',
        '2026-10-16T00:00:00.000Z'
    ),
    (
        '01a14202-2800-7000-8000-000000000002',
        NULL,
        'farm',
        'shared',
        '{"steps":[{"action":{"type":"select_hotbar_slot","slot":0},"schedule":{"type":"at_start"},"probability":100},{"action":{"type":"attack_facing_entity"},"schedule":{"type":"every","interval_ms":650,"jitter_ms":150},"probability":100}]}',
        1,
        '2026-10-16T00:00:00.000Z',
        '2026-10-16T00:00:00.000Z'
    );
