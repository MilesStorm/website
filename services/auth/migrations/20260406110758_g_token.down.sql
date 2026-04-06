-- Add down migration script here
ALTER TABLE users
DROP CONSTRAINT users_access_token_max_bytes;

ALTER TABLE users
ALTER COLUMN access_token TYPE VARCHAR(255);
