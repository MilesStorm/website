-- A name the user picks for themselves, shown on the website instead of the username.
-- The username stays fixed: GitHub logins find their account by it. NULL = not set.
ALTER TABLE users ADD COLUMN display_name VARCHAR(64);
