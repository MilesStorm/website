-- The Enshrouded server panel (/enshrouded): restart and force update, for the people
-- holding the `enshrouded` permission (given by the `enshrouded` role).
INSERT INTO permissions (name) VALUES ('enshrouded');

INSERT INTO roles (name) VALUES ('enshrouded');

INSERT INTO role_permissions (role_id, permission_id)
SELECT r.id, p.id
FROM roles r, permissions p
WHERE r.name = 'enshrouded' AND p.name = 'enshrouded';

-- Who restarted or updated the server, and when: shown on the panel, and the cooldown
-- between commands. A deleted account's commands stay, without a name.
CREATE TABLE enshrouded_actions (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT REFERENCES users(id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX enshrouded_actions_created_at ON enshrouded_actions (created_at DESC);
