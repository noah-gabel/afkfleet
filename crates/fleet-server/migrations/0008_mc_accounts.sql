-- The linked Minecraft accounts (Plan.md P9.1, §7.5; ADR-0018). Never edit this file once
-- it's on main: sqlx records its checksum, and `just migrations-check` refuses a change. Add
-- a new migration instead (ADR-0015).
--
-- A Microsoft account signs in through Microsoft: the server holds its refresh token, sealed
-- by the vault, and never hands it out (security rule 6). An offline account is only a name,
-- for offline-mode servers in development (P9.7, `[accounts] allow_offline`). Whoever links
-- an account owns it. A Minecraft account is linked at most once, whoever linked it, so a
-- second link of an account someone already holds is refused (ADR-0018, device-code
-- phishing). The grants that share an account arrive with their own migration (group E).
CREATE TABLE mc_accounts (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- Who linked it. No cascade: users are never deleted, as for the audit log's actor.
    owner_user_id TEXT NOT NULL REFERENCES users (id) CHECK (
        length(owner_user_id) = 36
        AND substr(owner_user_id, 9, 1) = '-'
        AND substr(owner_user_id, 14, 1) = '-'
        AND substr(owner_user_id, 19, 1) = '-'
        AND substr(owner_user_id, 24, 1) = '-'
        AND length(replace(owner_user_id, '-', '')) = 32
        AND replace(owner_user_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    kind TEXT NOT NULL CHECK (kind IN ('microsoft', 'offline')),
    -- The Minecraft profile's UUID, lowercase and hyphenated, of any version; NULL for an
    -- offline account, whose UUID the game derives from its exact name.
    mc_uuid TEXT CHECK (
        mc_uuid IS NULL OR (
            length(mc_uuid) = 36
            AND substr(mc_uuid, 9, 1) = '-'
            AND substr(mc_uuid, 14, 1) = '-'
            AND substr(mc_uuid, 19, 1) = '-'
            AND substr(mc_uuid, 24, 1) = '-'
            AND length(replace(mc_uuid, '-', '')) = 32
            AND replace(mc_uuid, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    -- The Minecraft name, in the case Minecraft shows it. Its length and characters stay in
    -- Rust (fleet-core's McUsername), as for users.username.
    mc_name TEXT NOT NULL,
    -- The Microsoft refresh token, sealed by the vault: a version 1 envelope (45 bytes of
    -- header, nonce and tag around the token) whose AAD binds it to this row's ID and the
    -- purpose ms_refresh, so it can't be moved to another account. No key_id column: the
    -- envelope's header names its key (as in user_mfa), which `vault status` and
    -- `vault rotate-key` read. NULL for an offline account.
    refresh_envelope BLOB CHECK (refresh_envelope IS NULL OR length(refresh_envelope) > 45),
    -- 'pending' (waiting for approval), 'active' or 'reauth_required' (Microsoft refused the
    -- refresh token). No CHECK on the values: the set may grow, and fleet-core's
    -- AccountStatus refuses a name it doesn't know when it reads the row.
    status TEXT NOT NULL,
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- Only a Microsoft account has a UUID and a refresh token.
    CHECK ((kind = 'microsoft') = (mc_uuid IS NOT NULL)),
    CHECK ((kind = 'microsoft') = (refresh_envelope IS NOT NULL)),
    -- An offline account needs no approval and has no token to refuse.
    CHECK (kind = 'microsoft' OR status = 'active')
) STRICT;

-- A Microsoft account is linked once, pending ones included. NULLs (offline accounts) never
-- collide.
CREATE UNIQUE INDEX mc_accounts_mc_uuid ON mc_accounts (mc_uuid);

-- An offline account's name is linked once, whatever its case: Minecraft names compare
-- without case.
CREATE UNIQUE INDEX mc_accounts_offline_name ON mc_accounts (lower(mc_name))
WHERE kind = 'offline';

-- An owner's accounts, for listing them and counting them against the quota.
CREATE INDEX mc_accounts_owner_user_id ON mc_accounts (owner_user_id);
