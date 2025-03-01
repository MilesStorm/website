-- Add migration script here
INSERT INTO roles (name, description) VALUES 
  ('llama', 'Member of SS llama'),
  ('photoview', 'People interested in photography');

INSERT INTO permissions (name, description) VALUES 
  ('llama', 'Can do everything llama related'),
  ('photoview', 'Can do everything photoview related');

INSERT INTO role_permissions (role_id, permission_id) VALUES 
  ((SELECT id FROM roles WHERE name = 'llama'), SELECT id FROM permissions WHERE name = 'llama')),
  ((SELECT id FROM roles WHERE name = 'photoview'), SELECT id FROM permissions WHERE name = 'photoview'));
