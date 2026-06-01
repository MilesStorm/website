INSERT INTO role_permissions (role_id, permission_id)
SELECT r.id, p.id
FROM roles r, permissions p
WHERE r.name = 'admin'
  AND p.name IN ('create_user', 'update_user', 'delete_user', 'mange_roles', 'manage_permissions')
ON CONFLICT DO NOTHING;
