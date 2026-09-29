-- Invite links: opening one gives the user a role (e.g. `arcane_user` for the dice
-- test group), after signing up first if they have no account (src/auth/invites.rs).

CREATE TABLE invites (
    id SERIAL PRIMARY KEY,
    -- Only the SHA-256 of the link's code is kept, so a copy of this table can't be
    -- used to join anything.
    code_hash TEXT NOT NULL UNIQUE,
    role_id INT NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    -- Who to show the link to in the admin panel ("for Sam", "Discord group").
    note TEXT,
    created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    -- NULL = any number of people.
    max_uses INT CHECK (max_uses IS NULL OR max_uses > 0),
    uses INT NOT NULL DEFAULT 0,
    revoked_at TIMESTAMPTZ
);

-- Who joined through which link; opening the same link again doesn't use it up again.
CREATE TABLE invite_redemptions (
    invite_id INT NOT NULL REFERENCES invites(id) ON DELETE CASCADE,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    redeemed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (invite_id, user_id)
);
