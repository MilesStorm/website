-- Emails the site sends about an account: confirm the address, reset the password,
-- confirm deleting the account (src/auth/account_email.rs).

-- When the user proved they own `email` (clicked a confirmation link, or reset their
-- password by email). NULL = not confirmed. Changing the email must clear it.
ALTER TABLE users ADD COLUMN email_verified_at TIMESTAMPTZ;

-- Google accounts: Google has already confirmed the address.
UPDATE users SET email_verified_at = NOW() WHERE email IS NOT NULL AND password IS NULL;

-- One row per emailed link. Only the SHA-256 of the link's code is kept, so a copy of
-- this table can't be used to reset anyone's password.
CREATE TABLE email_tokens (
    token_hash TEXT PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    purpose TEXT NOT NULL CHECK (purpose IN ('verify_email', 'reset_password', 'delete_account')),
    -- The address it was sent to: confirming only counts while it is still the user's.
    email VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    -- Set when the link is used, or replaced by a newer one of the same purpose.
    -- Kept (until a day after expiry) so sending can be rate limited.
    used_at TIMESTAMPTZ
);

CREATE INDEX email_tokens_user_purpose_idx ON email_tokens (user_id, purpose, created_at);
