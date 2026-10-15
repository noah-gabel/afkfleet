-- The grants that share a linked account (Plan.md P9.1, P9.6, §7.3; ADR-0010, ADR-0018).
-- Never edit this file once it's on main: sqlx records its checksum, and
-- `just migrations-check` refuses a change. Add a new migration instead (ADR-0015).
--
-- A grant gives one user View, Control or Manage on one account. The account's owner has
-- Manage through ownership and never a row here (ADR-0018, group D), and nobody grants to
-- themselves (ADR-0010). One row per account and user: changing a grant rewrites its row,
-- so the rows are bounded by accounts times users and need no cap (group E). They go with
-- their account.
CREATE TABLE account_grants (
    account_id TEXT NOT NULL REFERENCES mc_accounts (id) ON DELETE CASCADE CHECK (
        length(account_id) = 36
        AND substr(account_id, 9, 1) = '-'
        AND substr(account_id, 14, 1) = '-'
        AND substr(account_id, 19, 1) = '-'
        AND substr(account_id, 24, 1) = '-'
        AND length(replace(account_id, '-', '')) = 32
        AND replace(account_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- The grantee. No cascade: users are never deleted, as for the audit log's actor.
    user_id TEXT NOT NULL REFERENCES users (id) CHECK (
        length(user_id) = 36
        AND substr(user_id, 9, 1) = '-'
        AND substr(user_id, 14, 1) = '-'
        AND substr(user_id, 19, 1) = '-'
        AND substr(user_id, 24, 1) = '-'
        AND length(replace(user_id, '-', '')) = 32
        AND replace(user_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- fleet-core's GrantLevel: a closed set, as for users.role.
    level TEXT NOT NULL CHECK (level IN ('view', 'control', 'manage')),
    -- Who set it last. No cascade, as above.
    granted_by TEXT NOT NULL REFERENCES users (id) CHECK (
        length(granted_by) = 36
        AND substr(granted_by, 9, 1) = '-'
        AND substr(granted_by, 14, 1) = '-'
        AND substr(granted_by, 19, 1) = '-'
        AND substr(granted_by, 24, 1) = '-'
        AND length(replace(granted_by, '-', '')) = 32
        AND replace(granted_by, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- When it was set last.
    granted_at TEXT NOT NULL CHECK (
        granted_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    PRIMARY KEY (account_id, user_id),
    -- A belt behind authorize(): nobody grants to themselves.
    CHECK (user_id <> granted_by)
) STRICT;

-- A user's grants: the accounts they may see.
CREATE INDEX account_grants_user_id ON account_grants (user_id);
