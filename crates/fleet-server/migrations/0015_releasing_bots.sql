-- The release records: agents that may still run a bot whose row no longer says so (Plan.md
-- P10.5; ADR-0019, S39, S43, R30). Never edit this file once it's on main: sqlx records its
-- checksum, and `just migrations-check` refuses a change. Add a new migration instead
-- (ADR-0015).
--
-- Deleting an account deletes its bot with it, but the agent its bot was assigned to may still
-- run it (a stale agent, a disabled one, or one whose Removed hasn't arrived). A hello may also
-- list a bot its agent wasn't given (a restored backup). Each such case keeps a record here,
-- with the bot's Minecraft account as a player sees it, until that agent no longer runs the bot:
-- its Removed, a hello that doesn't list the bot, or the host's `agent release`. While a record
-- is open, no bot of that account is placed on any agent, so two agents never run one account.
-- A record is keyed by the bot and the agent: two agents that both list a bot each keep theirs.
CREATE TABLE releasing_bots (
    -- The bot. No foreign key: a deleted account's bot is gone.
    bot_id TEXT NOT NULL CHECK (
        length(bot_id) = 36
        AND substr(bot_id, 9, 1) = '-'
        AND substr(bot_id, 14, 1) = '-'
        AND substr(bot_id, 19, 1) = '-'
        AND substr(bot_id, 24, 1) = '-'
        AND length(replace(bot_id, '-', '')) = 32
        AND replace(bot_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- The agent that may still run it. No cascade: agents are never deleted.
    agent_id TEXT NOT NULL REFERENCES agents (id) CHECK (
        length(agent_id) = 36
        AND substr(agent_id, 9, 1) = '-'
        AND substr(agent_id, 14, 1) = '-'
        AND substr(agent_id, 19, 1) = '-'
        AND substr(agent_id, 24, 1) = '-'
        AND length(replace(agent_id, '-', '')) = 32
        AND replace(agent_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- The account the bot played, which may be gone too. No foreign key.
    account_id TEXT NOT NULL CHECK (
        length(account_id) = 36
        AND substr(account_id, 9, 1) = '-'
        AND substr(account_id, 14, 1) = '-'
        AND substr(account_id, 19, 1) = '-'
        AND substr(account_id, 24, 1) = '-'
        AND length(replace(account_id, '-', '')) = 32
        AND replace(account_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- The Minecraft account as a player sees it, copied from mc_accounts: a Microsoft
    -- profile's UUID, or an offline name, compared ignoring case.
    kind TEXT NOT NULL CHECK (kind IN ('microsoft', 'offline')),
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
    mc_name TEXT NOT NULL,
    -- When the record was written, by the server's clock.
    created_at TEXT NOT NULL CHECK (
        created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    PRIMARY KEY (bot_id, agent_id),
    -- Only a Microsoft account has a UUID.
    CHECK ((kind = 'microsoft') = (mc_uuid IS NOT NULL))
) STRICT;

-- The records of an agent: closing them at its hello, releasing them, counting them against
-- its capacity.
CREATE INDEX releasing_bots_agent_id ON releasing_bots (agent_id);
