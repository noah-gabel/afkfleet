-- The bots: one per linked Minecraft account (Plan.md P11.1, P10.5, Appendix D; ADR-0019,
-- S33–S40). Never edit this file once it's on main: sqlx records its checksum, and
-- `just migrations-check` refuses a change. Add a new migration instead (ADR-0015).
--
-- A bot is created with its account, on every path that creates one, and deleted with it.
-- It stores what the bot should do (the server it joins, its mode, whether it should run),
-- which agent the server assigned it to, and the last state an agent reported for it. Only
-- the server's reconciler assigns a bot, and only once its row says it has no agent: that's
-- what keeps two agents off one account (ADR-0019, S32). Auto-start arrives with its first
-- writer, P11.2's PATCH (S34).
CREATE TABLE bots (
    id TEXT NOT NULL PRIMARY KEY CHECK (
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND length(replace(id, '-', '')) = 32
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- Its account: one bot each, deleted with it.
    account_id TEXT NOT NULL UNIQUE REFERENCES mc_accounts (id) ON DELETE CASCADE CHECK (
        length(account_id) = 36
        AND substr(account_id, 9, 1) = '-'
        AND substr(account_id, 14, 1) = '-'
        AND substr(account_id, 19, 1) = '-'
        AND substr(account_id, 24, 1) = '-'
        AND length(replace(account_id, '-', '')) = 32
        AND replace(account_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- The server it joins, in fleet-core's ServerAddress form, which checks it when the row is
    -- read; NULL until someone sets one (P11.2). A bot without one is never assigned.
    server_address TEXT,
    -- What it does online. No ON DELETE action: deleting a mode in use is refused (P11.4).
    mode_id TEXT NOT NULL REFERENCES modes (id) CHECK (
        length(mode_id) = 36
        AND substr(mode_id, 9, 1) = '-'
        AND substr(mode_id, 14, 1) = '-'
        AND substr(mode_id, 19, 1) = '-'
        AND substr(mode_id, 24, 1) = '-'
        AND length(replace(mode_id, '-', '')) = 32
        AND replace(mode_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- Whether it should run (fleet-core's DesiredRunState).
    desired_state TEXT NOT NULL CHECK (desired_state IN ('running', 'stopped')),
    -- The agent the server assigned it to; NULL while it has none. Set only by the reconciler,
    -- and only from NULL; cleared only once that agent can't still run the bot (S32). No
    -- cascade: agents are never deleted.
    agent_id TEXT REFERENCES agents (id) CHECK (
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
    -- The last state its agent reported (fleet-core's LastState): its kind, and the reason of a
    -- paused or failed state, with no CHECK on the values (the set may grow, and fleet-core
    -- refuses a name it doesn't know when it reads the row); NULL until a first report is
    -- stored (S40).
    last_state TEXT,
    last_state_reason TEXT,
    -- When the server received that report, by its own clock.
    last_state_at TEXT CHECK (
        last_state_at IS NULL
        OR last_state_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- A state comes with its time, and a reason only with a state.
    CHECK ((last_state IS NULL) = (last_state_at IS NULL)),
    CHECK (last_state IS NOT NULL OR last_state_reason IS NULL)
) STRICT;

-- The bots of a mode, for checking a mode's edit against each of them (P11.4).
CREATE INDEX bots_mode_id ON bots (mode_id);

-- An agent's bots, for counting them and for releasing a disabled agent's (S32).
CREATE INDEX bots_agent_id ON bots (agent_id);

-- Every account linked before this migration gets its bot: no server, the built-in afk mode,
-- stopped, unassigned. Its ID is the account's with the high bit of the last hex digit flipped
-- (S35): that digit holds only random bits, so the ID stays a version 7 UUID, unique and never
-- the account's own, without randomness the migration can't draw from the server's source.
INSERT INTO bots (id, account_id, server_address, mode_id, desired_state)
SELECT
    substr(id, 1, 35)
        || substr('89abcdef01234567', instr('0123456789abcdef', substr(id, 36, 1)), 1),
    id,
    NULL,
    '01a14202-2800-7000-8000-000000000001',
    'stopped'
FROM mc_accounts;
