// Stand-ins for what the server calls, for the end-to-end tests against the built image: auth
// (login, the token check, profiles, emails, invites, roll-sharing consent) and Ark. SurrealDB is
// a real one (e2e/serve.sh). Test code only:
// it runs on the CI runner beside the container, never in an image.
//
// Accounts: `tester-none`, `tester-all` and `tester-down`, password `right`, and any
// `tester-<kind>-<suffix>` of those kinds (a fresh account for a test that changes it, so tests
// running at once don't meet). A token is the name after `tester-`; its kind is the part before
// the first `-`. The kinds' tokens are
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
/** The Enshrouded commands sent, newest first. */
const enshroudedRecent = [];
/** Display names set, and the roll-sharing choice, by token. */
const names = {};
const sharing = {};

/** User ids, by token, given out as accounts are first seen. */
const userIds = { all: 1, none: 2 };
let nextUserId = 3;
const userId = (token) => (userIds[token] ??= nextUserId++);
/** A known token's profile; a new account (token `none`, any username) shares `none`'s. */
const profile = (token) => ({
  user_id: userId(token),
  username: `tester-${token}`,
  display_name: token in names ? names[token] : token === 'all' ? 'Tess All' : null,
  email: `${token}@example.test`,
  // The `all` kind's address is confirmed (a deletion goes by email), `none`'s isn't.
  email_verified: kind(token) === 'all',
});

const PERMISSIONS = { all: ['llama', 'enshrouded', 'arcane', 'manage_permissions', 'brand_new'], none: [] };
const kind = (token) => String(token).split('-')[0];
const known = (token) => kind(token) in PERMISSIONS;

const routes = {
  'POST /internal/token/exchange': ({ username, password }) =>
    password === 'right' && /^tester-(none|all|down)(-[a-z0-9]+)?$/.test(username)
      ? [200, { token: username.slice('tester-'.length), username }]
      : [401, 'Invalid credentials'],
  'POST /internal/token/introspect': ({ token }) =>
    kind(token) === 'down'
      ? [500, {}]
      : known(token)
        ? [200, { permissions: PERMISSIONS[kind(token)] }]
        : [401, 'Invalid or expired token'],
  'POST /internal/profile': ({ token }) => (known(token) ? [200, profile(token)] : [401, {}]),
  'POST /internal/profile/display_name': ({ token, display_name }) => {
    if (!known(token)) return [401, {}];
    if ((display_name ?? '').length > 40) return [400, {}];
    names[token] = display_name || null;
    return [200, profile(token)];
  },
  'POST /internal/email/verify/send': ({ token }) =>
    known(token) ? [200, { email: `${token}@example.test` }] : [401, { error: 'logged_out' }],
  'POST /internal/account/delete/request': ({ token }) =>
    known(token) ? [200, { email: `${token}@example.test` }] : [401, { error: 'logged_out' }],
  'POST /internal/dataset/consent': ({ token }) => [200, { share: !!sharing[token] }],
  'POST /internal/dataset/consent/set': ({ token, share }) => {
    sharing[token] = share;
    return [200, {}];
  },
  'POST /internal/ark/num_players': ({ token }) =>
    kind(token) === 'all' ? [200, { restart_result: '', command_result: { NumPlayers: 3 } }] : [403, {}],
  // Enshrouded: online, one player, a newer build on Steam. Commands go into the recent list,
  // newest first; an account whose suffix starts with `busy` is told one is already under way.
  'POST /internal/enshrouded/status': ({ token }) =>
    kind(token) === 'all'
      ? [
          200,
          {
            state: 'Online',
            players: { online: 1, max: 16, sessions: [600] },
            version: '1269579',
            uptime_secs: 90_000,
            latest_build_at: Math.floor(Date.now() / 1000) - 9_000,
            update_pending: true,
            update_queued: false,
            can_control: true,
            recent: enshroudedRecent.slice(0, 10),
            next_command_at: null,
          },
        ]
      : [403, { error: 'forbidden' }],
  'POST /internal/enshrouded/command': ({ token, cmd }) => {
    if (kind(token) !== 'all') return [403, { error: 'forbidden' }];
    if (String(token).startsWith('all-busy')) return [409, { error: 'busy' }];
    enshroudedRecent.unshift({ name: `tester-${token}`, action: cmd, at: Math.floor(Date.now() / 1000) });
    return [200, {}];
  },
  'POST /internal/ark/command': ({ token }) =>
    kind(token) === 'all' ? [200, { restart_result: '', command_result: 'Started' }] : [403, {}],
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
    !known(token) ? [401, {}] : code === 'join' ? [200, { role: 'arcane_user' }] : [404, {}],
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
