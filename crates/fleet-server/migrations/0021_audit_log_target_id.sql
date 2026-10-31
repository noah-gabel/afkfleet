-- A target's own audit entries, newest first (Plan.md P11.9; ADR-0020, U78, S89). Never edit
-- this file once it's on main: sqlx records its checksum, and `just migrations-check` refuses a
-- change. Add a new migration instead (ADR-0015).
--
-- `GET /audit?target_kind=&target_id=` walks one target's entries backward by ID. 0007's
-- `audit_log_target` holds `action` before `id`, so SQLite would read and sort all of a target's
-- rows for each page. That index stays for the counts by action.
CREATE INDEX audit_log_target_id ON audit_log (target_type, target_id, id);
