-- Whether a user shares pictures of their dice rolls to train the dice reader
-- (the switch on the website's profile page). Removed together with the account.
-- The pictures themselves live in SurrealDB (services/frontend/surreal).
CREATE TABLE dataset_consent (
    user_id BIGINT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    share BOOLEAN NOT NULL,
    -- Which wording of the consent text the user saw (the website bumps it on change).
    consent_version TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
