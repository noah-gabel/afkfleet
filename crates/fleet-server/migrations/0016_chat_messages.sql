-- The bots' chat, received and sent (Plan.md P11.1, P10.8, Appendix D; ADR-0019, S5, S6, S28,
-- S70). Never edit this file once it's on main: sqlx records its checksum, and
-- `just migrations-check` refuses a change. Add a new migration instead (ADR-0015).
--
-- Append-only through the code: the server's ingest inserts rows in batches (U17's EventBatch),
-- and P11.6's retention deletes old ones. A row is one message a bot received ('in') or its mode
-- sent ('out'). The text is what fleet-core rebuilt, cleaned and capped (S6), stored and shown as
-- plain text. Listed by ID only.
CREATE TABLE chat_messages (
    -- An append-only log's ID: with one writer, ID order is commit order, which is the order the
    -- server received the messages in; AUTOINCREMENT never reuses an ID after retention deletes.
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- The bot; its chat is deleted with it, and so with its account.
    bot_id TEXT NOT NULL REFERENCES bots (id) ON DELETE CASCADE CHECK (
        length(bot_id) = 36
        AND substr(bot_id, 9, 1) = '-'
        AND substr(bot_id, 14, 1) = '-'
        AND substr(bot_id, 19, 1) = '-'
        AND substr(bot_id, 24, 1) = '-'
        AND length(replace(bot_id, '-', '')) = 32
        AND replace(bot_id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    -- Received by the bot, or sent by it: a closed set.
    direction TEXT NOT NULL CHECK (direction IN ('in', 'out')),
    -- For 'in', fleet-core's ChatKind name (chat, emote, whisper, announcement, system); for 'out',
    -- 'mode'. No CHECK: later phases add kinds (P11.3's user chat), and the Rust types refuse a
    -- name they don't know when they read the row.
    kind TEXT NOT NULL,
    -- Who sent a player message, as the Minecraft server claims it (never verified, ADR-0011): the
    -- cleaned display name, and the player's UUID when the server gave one. NULL for a system
    -- message and for everything the bot sent.
    sender_name TEXT,
    sender_uuid TEXT CHECK (
        sender_uuid IS NULL OR (
            length(sender_uuid) = 36
            AND substr(sender_uuid, 9, 1) = '-'
            AND substr(sender_uuid, 14, 1) = '-'
            AND substr(sender_uuid, 19, 1) = '-'
            AND substr(sender_uuid, 24, 1) = '-'
            AND length(replace(sender_uuid, '-', '')) = 32
            AND replace(sender_uuid, '-', '') NOT GLOB '*[^0-9a-f]*'
        )
    ),
    -- No length CHECK: the caps live in Rust and may change (ADR-0015's rule for metadata_json).
    text TEXT NOT NULL,
    -- Whether the text was cut: the agent's flag or the server's own cut (S6).
    truncated INTEGER NOT NULL CHECK (truncated IN (0, 1)),
    -- When the server received it, by its own clock (S5, S40), never the agent's: no report can
    -- place itself outside the retention window. Informational: the order is the ID.
    at TEXT NOT NULL CHECK (
        at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z'
    ),
    -- A UUID comes with a name.
    CHECK (sender_uuid IS NULL OR sender_name IS NOT NULL),
    -- What a bot sent has no sender and is never cut.
    CHECK (direction = 'in' OR (sender_name IS NULL AND truncated = 0))
) STRICT;

-- One bot's chat, newest first, page by page (P11.3's `?before=`), and the cascade from bots.
CREATE INDEX chat_messages_bot_id ON chat_messages (bot_id, id);

-- The rows retention deletes (P11.6).
CREATE INDEX chat_messages_at ON chat_messages (at);
