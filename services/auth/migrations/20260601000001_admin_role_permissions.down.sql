DELETE FROM role_permissions
WHERE role_id = (SELECT id FROM roles WHERE name = 'admin')
  AND permission_id IN (
    SELECT id FROM permissions
    WHERE name IN ('create_user', 'update_user', 'delete_user', 'mange_roles', 'manage_permissions')
  );
