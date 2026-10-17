DROP TABLE enshrouded_actions;

DELETE FROM role_permissions
WHERE role_id = (SELECT id FROM roles WHERE name = 'enshrouded')
  AND permission_id = (SELECT id FROM permissions WHERE name = 'enshrouded');

DELETE FROM roles WHERE name = 'enshrouded';

DELETE FROM permissions WHERE name = 'enshrouded';
