-- Who sent a user's chat line (Plan.md P11.3, Appendix D; ADR-0020, S6, S7, U3). Never edit
-- this file once it's on main: sqlx records its checksum, and `just migrations-check` refuses a
-- change. Add a new migration instead (ADR-0015).
--
-- From P11.3 on, a line a user sends a bot through the API is an 'out' row of the kind 'user',
-- appended in the send's audited transaction before it's sent, and never changed afterwards
-- (S7). Its sender is the afkfleet user who sent it, never a Minecraft player: the history shows
-- their name only to the viewers who see the account's owner, and marks each viewer's own lines
-- (U3). A deleted user's lines keep no sender.
ALTER TABLE chat_messages ADD COLUMN sent_by TEXT REFERENCES users (id) ON DELETE SET NULL CHECK (
    sent_by IS NULL OR (
        direction = 'out'
        AND kind = 'user'
        AND length(sent_by) = 36
        AND substr(sent_by, 9, 1) = '-'
        AND substr(sent_by, 14, 1) = '-'
        AND substr(sent_by, 19, 1) = '-'
        AND substr(sent_by, 24, 1) = '-'
        AND length(replace(sent_by, '-', '')) = 32
        AND replace(sent_by, '-', '') NOT GLOB '*[^0-9a-f]*'
    )
);

-- A user's lines, so deleting the user finds them without reading every row. Only lines with a
-- sender are indexed: received and mode chat, nearly every row, never has one.
CREATE INDEX chat_messages_sent_by ON chat_messages (sent_by) WHERE sent_by IS NOT NULL;
