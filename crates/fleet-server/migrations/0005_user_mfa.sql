-- The users' second factor (Plan.md P7.1, P7.7, §7.2; ADR-0016). Never edit this file once
-- it's on main: sqlx records its checksum, and `just migrations-check` refuses a change. Add
-- a new migration instead (ADR-0015).
--
-- 2FA is mandatory for every user: a user has 2FA exactly when their user_mfa row exists,
-- and until then the password step answers mfa_setup_required. A row is only ever written
-- by a confirmed enrollment; a pending one waits in the server's memory. Replacing the
-- authenticator replaces the row and every recovery code in one transaction, so a user who
-- has 2FA never loses it.
CREATE TABLE user_mfa (
    -- No cascade: users are never deleted, as for the audit log's actor.
    user_id TEXT NOT NULL PRIMARY KEY REFERENCES users (id) CHECK (
        length(user_id) = 36
        AND substr(user_id, 9, 1) = '-'
        AND substr(user_id, 14, 1) = '-'
        AND substr(user_id, 19, 1) = '-'
        AND substr(user_id, 24, 1) = '-'
        AND length(replace(user_id, '-', '')) = 32
        AND replace(user_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- The 20-byte TOTP secret, sealed by the vault: a version 1 envelope (1 + 4 + 24 bytes
    -- of header, the secret, a 16-byte tag), whose AAD binds it to this user's ID and the
    -- purpose totp_secret, so it can't be moved to another user's row. The length pins
    -- version 1; another envelope version comes with its own migration.
    secret_envelope BLOB NOT NULL CHECK (length(secret_envelope) = 65),
    -- The last time step a code was accepted for. Only a later step counts, so a code
    -- can't be used twice (replay protection, RFC 6238 section 5.2). Confirming sets it to
    -- the confirming code's step.
    last_used_step INTEGER NOT NULL CHECK (last_used_step >= 0),
    -- When the current authenticator was confirmed. A login reads it with the secret and
    -- accepts a code only while it's unchanged, so a code of a just-replaced secret can't
    -- log in.
    confirmed_at TEXT NOT NULL CHECK (
        confirmed_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    )
) STRICT;

-- The single-use recovery codes, 10 per confirmed authenticator, shown once by confirm.
--
-- Each is hashed with plain SHA-256 of its normalized text (20 lowercase base32
-- characters). That's safe only because every code carries 100 bits from the server's
-- secure random source, so a leaked database or backup can't be brute-forced offline.
-- The hash isn't keyed on purpose: a key derived from the vault's active key would make
-- every recovery code unusable after a key rotation (Phase 9), since the hashes can't be
-- recomputed without the codes, and unlike the lockout's counters that loss wouldn't be
-- harmless.
CREATE TABLE recovery_codes (
    -- SHA-256 of the normalized code.
    hash BLOB NOT NULL PRIMARY KEY CHECK (length(hash) = 32),
    -- No cascade, as above. Replacing the authenticator deletes the old codes.
    user_id TEXT NOT NULL REFERENCES users (id) CHECK (
        length(user_id) = 36
        AND substr(user_id, 9, 1) = '-'
        AND substr(user_id, 14, 1) = '-'
        AND substr(user_id, 19, 1) = '-'
        AND substr(user_id, 24, 1) = '-'
        AND length(replace(user_id, '-', '')) = 32
        AND replace(user_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- When the code was used to log in; NULL while it's unused. A used code keeps its row.
    used_at TEXT CHECK (
        used_at IS NULL
        OR used_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    )
) STRICT;

-- A user's codes: the login's lookup and the replacement's delete.
CREATE INDEX recovery_codes_user_id ON recovery_codes (user_id);
