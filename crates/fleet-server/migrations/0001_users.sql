-- The users table (Plan.md Appendix D, P6.4). Never edit this file once it's on main:
-- sqlx records its checksum, and `just migrations-check` refuses a change. Add a new
-- migration instead (ADR-0015).
--
-- Conventions for every table (ADR-0015): STRICT. Entity IDs are lowercase, hyphenated
-- version 7 UUID text. Times are UTC text with milliseconds, 'YYYY-MM-DDTHH:MM:SS.mmmZ',
-- so text order is time order. CHECKs pin only shapes and closed sets that never change.
CREATE TABLE users (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- fleet-core's Username stores names lowercased; the CHECK keeps a hand-written
    -- insert from adding 'Alice' next to 'alice'. Length and characters stay in Rust.
    username TEXT NOT NULL UNIQUE CHECK (username = lower(username)),
    -- A PHC string: '$', an algorithm id of a-z, 0-9 and '-', '$', then the rest, so a
    -- plain password can't be stored by mistake. P7.2 owns the exact format.
    password_hash TEXT NOT NULL CHECK (
        substr(password_hash, 1, 1) = '$'
        AND instr(substr(password_hash, 2), '$') > 1
        AND substr(password_hash, 2, instr(substr(password_hash, 2), '$') - 1)
            NOT GLOB '*[^a-z0-9-]*'
    ),
    role TEXT NOT NULL CHECK (role IN ('member', 'admin', 'owner')),
    disabled INTEGER NOT NULL CHECK (disabled IN (0, 1)),
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- The time the current password was set: created_at for a new user.
    password_changed_at TEXT NOT NULL CHECK (
        password_changed_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    )
) STRICT;

-- Exactly one Owner (ADR-0010, Plan.md P7.1).
CREATE UNIQUE INDEX users_one_owner ON users (role) WHERE role = 'owner';
