-- An index on the audit log's target (Plan.md P7.6; ADR-0016). Never edit this file once
-- it's on main: sqlx records its checksum, and `just migrations-check` refuses a change. Add
-- a new migration instead (ADR-0015).
--
-- `GET /me` counts the wrong 2FA codes since the user's previous login: the `auth.login`
-- entries whose target is the user, by action and in ID order. Without this index each
-- request would scan the whole log.
CREATE INDEX audit_log_target ON audit_log (target_type, target_id, action, id);
