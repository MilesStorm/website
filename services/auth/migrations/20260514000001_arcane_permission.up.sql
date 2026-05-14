INSERT INTO permissions (name) VALUES ('arcane');

INSERT INTO roles (name) VALUES ('arcane_user');

INSERT INTO role_permissions (role_id, permission_id)
SELECT r.id, p.id
FROM roles r, permissions p
WHERE r.name = 'arcane_user' AND p.name = 'arcane';
