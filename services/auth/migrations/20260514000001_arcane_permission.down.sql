DELETE FROM role_permissions
WHERE role_id = (SELECT id FROM roles WHERE name = 'arcane_user')
  AND permission_id = (SELECT id FROM permissions WHERE name = 'arcane');

DELETE FROM roles WHERE name = 'arcane_user';

DELETE FROM permissions WHERE name = 'arcane';
