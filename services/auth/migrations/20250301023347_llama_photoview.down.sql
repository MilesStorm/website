-- First remove the role-permission mappings
DELETE FROM role_permissions
WHERE (role_id = (SELECT id FROM roles WHERE name = 'llama') AND permission_id = (SELECT id FROM permissions WHERE name = 'llama'))
   OR (role_id = (SELECT id FROM roles WHERE name = 'photoview') AND permission_id = (SELECT id FROM permissions WHERE name = 'photoview'));

-- Then remove the permissions
DELETE FROM permissions
WHERE name IN ('llama', 'photoview');

-- Finally remove the roles
DELETE FROM roles
WHERE name IN ('llama', 'photoview');

