// Stand-ins for what the server calls, for the end-to-end tests against the built image: auth
// (login, the token check, profiles, Ark), and SurrealDB (no pictures stored). Test code only:
// it runs on the CI runner beside the container, never in an image.
//
// Accounts: `tester-none`, `tester-all` and `tester-down`, password `right`. Their tokens are
// `none` (no permissions), `all` (every permission the site knows, plus one it doesn't) and
// `down`, whose check fails as if auth were down: logged in, with no permissions known. A new
// account gets token `none`.
//
// Emailed codes: `good` works, anything else has expired. Invite code `join` is for
// `arcane_user`. OAuth: the provider's page is skipped, auth sends the browser straight back to
// the site's callback (SITE_URL) with code `good`, which logs in as `tester-all`.
import { createServer } from 'node:http';

const port = Number(process.env.FAKE_PORT ?? 4010);
const site = process.env.SITE_URL ?? 'https://localhost:8443';
const PERMISSIONS = { all: ['llama', 'enshrouded', 'arcane', 'manage_permissions', 'brand_new'], none: [] };

const routes = {
  'POST /internal/token/exchange': ({ username, password }) =>
    password === 'right' && ['tester-none', 'tester-all', 'tester-down'].includes(username)
      ? [200, { token: username.slice('tester-'.length), username }]
      : [401, 'Invalid credentials'],
  'POST /internal/token/introspect': ({ token }) =>
    token === 'down'
      ? [500, {}]
      : token in PERMISSIONS
        ? [200, { permissions: PERMISSIONS[token] }]
        : [401, 'Invalid or expired token'],
  'POST /internal/profile': ({ token }) =>
    token in PERMISSIONS
      ? [200, { user_id: token === 'all' ? 1 : 2, username: `tester-${token}`, display_name: token === 'all' ? 'Tess All' : null, email: `${token}@example.test`, email_verified: true }]
      : [401, {}],
  'POST /internal/ark/num_players': ({ token }) =>
    token === 'all' ? [200, { restart_result: '', command_result: { NumPlayers: 3 } }] : [403, {}],
  'POST /internal/ark/command': ({ token }) =>
    token === 'all' ? [200, { restart_result: '', command_result: 'Started' }] : [403, {}],
  'POST /internal/register': ({ username }) =>
    username === 'taken' ? [409, 'User already exists'] : [200, { token: 'none', username }],
  'POST /internal/oauth/start': ({ provider }) =>
    [200, { auth_url: `${site}/oauth/callback/${provider}?code=good&state=e2e-state`, state: 'e2e-state' }],
  'POST /internal/oauth/exchange': ({ code }) =>
    code === 'good' ? [200, { token: 'all', username: 'tester-all' }] : [400, 'bad code'],
  'POST /internal/password/forgot': () => [200, {}],
  'POST /internal/password/reset': ({ code }) =>
    code === 'good' ? [200, { username: 'tester-none' }] : [404, { error: 'invalid_code' }],
  'POST /internal/email/verify/confirm': ({ code }) =>
    code === 'good' ? [200, { username: 'tester-none' }] : [404, { error: 'invalid_code' }],
  'POST /internal/account/delete/check': ({ code }) =>
    code === 'good' ? [200, { username: 'tester-none', user_id: 2 }] : [404, { error: 'invalid_code' }],
  'POST /internal/account/delete/confirm': () => [200, {}],
  'POST /internal/invite/check': ({ code }) => (code === 'join' ? [200, { role: 'arcane_user' }] : [404, {}]),
  'POST /internal/invite/redeem': ({ code, token }) =>
    !(token in PERMISSIONS) ? [401, {}] : code === 'join' ? [200, { role: 'arcane_user' }] : [404, {}],
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
