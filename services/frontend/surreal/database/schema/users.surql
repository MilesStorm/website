-- Logins limited to this database. Passwords come from Infisical at sync time
-- (SURREALKIT_VAR_DICE_PASSWORD / SURREALKIT_VAR_DICE_READER_PASSWORD); surrealkit
-- stores only a hash of each statement, never the filled-in password.
--
-- surrealkit re-applies a file only when its text changes, not when a password
-- does. To change a password: update it in Infisical, change this date, run sync.
-- Passwords last set: 2026-09-25

-- The website: saves samples and deletes a user's own on request.
DEFINE USER dice ON DATABASE PASSWORD '${DICE_PASSWORD}' ROLES EDITOR;
-- The training pull tool (tools/pull_dataset.py): read only.
DEFINE USER dice_reader ON DATABASE PASSWORD '${DICE_READER_PASSWORD}' ROLES VIEWER;
