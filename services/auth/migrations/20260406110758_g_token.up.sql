-- Add up migration script here
ALTER TABLE users
ALTER COLUMN access_token TYPE TEXT;

ALTER TABLE users
ADD CONSTRAINT users_access_token_max_bytes
CHECK (octet_length(access_token) <= 2048);
