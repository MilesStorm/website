// Stand-ins for what the server calls, for the end-to-end tests against the built image: auth
// (login, the token check, profiles, Ark), and SurrealDB (no pictures stored). Test code only:
// it runs on the CI runner beside the container, never in an image.
//
// Accounts: `tester-none` and `tester-all`, password `right`. Their tokens are `none` (no
// permissions) and `all` (every permission the site knows, plus one it doesn't).
import { createServer } from 'node:http';

const port = Number(process.env.FAKE_PORT ?? 4010);
const PERMISSIONS = { all: ['llama', 'enshrouded', 'arcane', 'manage_permissions', 'brand_new'], none: [] };

const routes = {
  'POST /internal/token/exchange': ({ username, password }) =>
    password === 'right' && ['tester-none', 'tester-all'].includes(username)
      ? [200, { token: username.slice('tester-'.length), username }]
      : [401, 'Invalid credentials'],
  'POST /internal/token/introspect': ({ token }) =>
    token in PERMISSIONS ? [200, { permissions: PERMISSIONS[token] }] : [401, 'Invalid or expired token'],
  'POST /internal/profile': ({ token }) =>
    token in PERMISSIONS
      ? [200, { user_id: token === 'all' ? 1 : 2, username: `tester-${token}`, display_name: token === 'all' ? 'Tess All' : null, email: `${token}@example.test`, email_verified: true }]
      : [401, {}],
  'POST /internal/ark/num_players': ({ token }) =>
    token === 'all' ? [200, { restart_result: '', command_result: { NumPlayers: 3 } }] : [403, {}],
  'POST /internal/ark/command': ({ token }) =>
    token === 'all' ? [200, { restart_result: '', command_result: 'Started' }] : [403, {}],
  // SurrealDB: signed in, and nothing stored.
  'POST /signin': () => [200, { code: 200, token: 'surreal-token' }],
  'POST /rpc': ({ id }) => [200, { id, result: [{ status: 'OK', result: null }] }],
};

createServer((req, res) => {
  let body = '';
  req.on('data', (chunk) => (body += chunk));
  req.on('end', () => {
    const route = routes[`${req.method} ${req.url.split('?')[0]}`];
    let parsed = {};
    try {
      parsed = body ? JSON.parse(body) : {};
    } catch {}
    const [status, reply] = route ? route(parsed) : [404, { error: 'not_faked' }];
    if (!route) console.error(`fake-upstreams: no stand-in for ${req.method} ${req.url}`);
    res.writeHead(status, { 'content-type': typeof reply === 'string' ? 'text/plain' : 'application/json' });
    res.end(typeof reply === 'string' ? reply : JSON.stringify(reply));
  });
}).listen(port, '127.0.0.1', () => console.log(`fake-upstreams on 127.0.0.1:${port}`));
