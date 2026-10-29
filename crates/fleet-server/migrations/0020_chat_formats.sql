-- A server's chat formats (Plan.md P11.9, Appendix D; ADR-0020, S79, S80, R139). Never edit
-- this file once it's on main: sqlx records its checksum, and `just migrations-check` refuses a
-- change. Add a new migration instead (ADR-0015).
--
-- fleet-core's ChatFormats as a JSON array of strings, the formats as written, in order,
-- duplicates kept, read back only through its constructor and only when it gives back exactly
-- that list. A row that had none before holds none. Nothing reads them but the chat's reads:
-- they never reach an agent (ADR-0013).
--
-- No CHECK pins that a row holds a text or a format: the routes refuse a write that leaves it
-- empty (S78), and what "something" means grows again with the later per-server display rules
-- (U60), which a CHECK here would make a table rebuild.
ALTER TABLE server_settings ADD COLUMN chat_formats TEXT NOT NULL DEFAULT '[]' CHECK (
    json_valid(chat_formats) AND json_type(chat_formats) = 'array'
);
