-- Mode names' uniqueness (Plan.md P11.4, Appendix D; ADR-0020, S13, U8). Never edit this file
-- once it's on main: sqlx records its checksum, and `just migrations-check` refuses a change. Add a
-- new migration instead (ADR-0015).
--
-- A name is fleet-core's ModeName: trimmed, 1 to 32 letters, digits, plain spaces and punctuation
-- (U8), which the server checks before it writes. Two rules keep names apart, comparing them
-- without ASCII case, as SQLite's lower() and ModeName::same_name do: one owner's modes, private
-- and shared together, never share a name, and neither do any two shared modes, the built-in ones
-- included. A private mode may take a shared one's name, and two owners' private modes may share
-- one: whoever could meet either rule sees every mode it compares with, so it reveals nothing.
-- A private mode can't take a built-in mode's name either, which no index here can enforce: the
-- request's own check refuses it, ASCII case ignored, before anything is written (fleet-api-types'
-- valid_mode_name; ADR-0020, R4).

-- One owner's names. The built-in modes have no owner, and a unique index keeps rows whose owner
-- is NULL apart, so they never clash here. It also serves every lookup by owner.
CREATE UNIQUE INDEX modes_owner_name ON modes (owner_user_id, lower(name));

-- The shared modes' names, the built-in ones included, which are shared.
CREATE UNIQUE INDEX modes_shared_name ON modes (lower(name)) WHERE visibility = 'shared';

-- The index above starts with the owner, so this one only cost writes.
DROP INDEX modes_owner_user_id;
