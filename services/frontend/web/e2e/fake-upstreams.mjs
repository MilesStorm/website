// Stand-ins for what the server calls, for the end-to-end tests against the built image: auth
// (login, the token check, profiles, emails, invites, roll-sharing consent, admin), Ark,
// Enshrouded, and ai_pipeline's camera WebSocket. SurrealDB is a real one (e2e/serve.sh). Test
// code only: it runs on the CI runner beside the container, never in an image.
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
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
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
  'POST /internal/token/exchange': ({ username, password }) => {
    if (password !== 'right' || !/^tester-(none|all|down)(-[a-z0-9]+)?$/.test(username)) {
      return [401, 'Invalid credentials'];
    }
    const token = username.slice('tester-'.length);
    seen(token);
    return [200, { token, username }];
  },
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

// ---- Admin (auth trusts the website to have checked `manage_permissions`) ----
//
// Every account that has logged in is a user, with 30 more (`player-01`…) so the list has pages.
// Roles: `admin` (every permission), `arcane_user`, and for each fresh account a `scratch_<suffix>`
// role of its own, so tests that change a role's permissions don't meet.

const ADMIN_PERMISSIONS = [
  { id: 1, name: 'llama' },
  { id: 2, name: 'enshrouded' },
  { id: 3, name: 'arcane' },
  { id: 4, name: 'manage_permissions' },
  { id: 9, name: 'brand_new' },
];
const adminRoles = [
  { id: 1, name: 'admin', permissions: [1, 2, 3, 4, 9] },
  { id: 2, name: 'arcane_user', permissions: [3] },
];
let nextRoleId = 10;
/** Role ids by user id: `tester-all` is an admin. */
const userRoles = { 1: [1], 2: [] };
const players = Array.from({ length: 30 }, (_, i) => ({
  id: 1001 + i,
  username: `player-${String(i + 1).padStart(2, '0')}`,
  email: null,
}));
const invites = [];
let nextInviteId = 1;

/** An account logging in: a user of the admin listing, and a fresh one's scratch role. */
function seen(token) {
  const id = userId(token);
  userRoles[id] ??= kind(token) === 'all' ? [1] : [];
  const suffix = token.split('-')[1];
  if (suffix && !adminRoles.some((r) => r.name === `scratch_${suffix}`)) {
    adminRoles.push({ id: nextRoleId++, name: `scratch_${suffix}`, permissions: [] });
  }
}

const roleRef = (id) => {
  const role = adminRoles.find((r) => r.id === id);
  return role && { id: role.id, name: role.name };
};
const adminUsers = () => [
  ...Object.entries(userIds).map(([token, id]) => ({ id, username: `tester-${token}`, email: `${token}@example.test` })),
  ...players,
].map((u) => ({ ...u, roles: (userRoles[u.id] ?? []).map(roleRef).filter(Boolean) }));

/** One page of `items` whose `name` has `search`, as auth answers a listing. */
function paged(items, query, name) {
  const page = Number(query.get('page') ?? 0);
  const limit = Number(query.get('limit') ?? 25);
  const search = (query.get('search') ?? '').toLowerCase();
  const found = items.filter((x) => name(x).toLowerCase().includes(search));
  return [200, { items: found.slice(page * limit, page * limit + limit), total: found.length }];
}

/** The admin routes with ids in their paths: `[method, pattern, handler(match, body)]`. */
const dynamicRoutes = [
  ['POST', /^\/internal\/admin\/users\/(\d+)\/roles\/(\d+)$/, ([, user, role]) => {
    if (!adminUsers().some((u) => u.id === Number(user)) || !roleRef(Number(role))) return [404, { error: 'not_found' }];
    const have = (userRoles[user] ??= []);
    if (!have.includes(Number(role))) have.push(Number(role));
    return [204, ''];
  }],
  ['DELETE', /^\/internal\/admin\/users\/(\d+)\/roles\/(\d+)$/, ([, user, role]) => {
    userRoles[user] = (userRoles[user] ?? []).filter((r) => r !== Number(role));
    return [204, ''];
  }],
  ['POST', /^\/internal\/admin\/roles\/(\d+)\/permissions\/(\d+)$/, ([, role, permission]) => {
    const found = adminRoles.find((r) => r.id === Number(role));
    if (!found) return [404, { error: 'not_found' }];
    if (!found.permissions.includes(Number(permission))) found.permissions.push(Number(permission));
    return [204, ''];
  }],
  ['DELETE', /^\/internal\/admin\/roles\/(\d+)\/permissions\/(\d+)$/, ([, role, permission]) => {
    const found = adminRoles.find((r) => r.id === Number(role));
    if (found) found.permissions = found.permissions.filter((p) => p !== Number(permission));
    return [204, ''];
  }],
  ['DELETE', /^\/internal\/admin\/invites\/(\d+)$/, ([, id]) => {
    const invite = invites.find((i) => i.id === Number(id));
    if (!invite) return [404, { error: 'no_invite' }];
    invite.revoked = true;
    invite.live = false;
    return [204, ''];
  }],
];

Object.assign(routes, {
  'GET /internal/admin/users': (_, query) => paged(adminUsers(), query, (u) => `${u.username} ${u.email ?? ''}`),
  'GET /internal/admin/roles': (_, query) =>
    paged(
      adminRoles.map((r) => ({ ...r, permissions: r.permissions.map((p) => ADMIN_PERMISSIONS.find((x) => x.id === p)) })),
      query,
      (r) => r.name,
    ),
  'GET /internal/admin/roles/all': () => [200, adminRoles.map((r) => roleRef(r.id))],
  'GET /internal/admin/permissions': () => [200, ADMIN_PERMISSIONS],
  'GET /internal/admin/invites': () => [200, [...invites].reverse().map(({ link, ...listed }) => listed)],
  'POST /internal/admin/invites': ({ token, role_id, days, max_uses, note }) => {
    if (!known(token)) return [401, {}];
    const role = adminRoles.find((r) => r.id === role_id);
    if (!role) return [404, { error: 'no_role' }];
    if (role.permissions.includes(4)) return [400, { error: 'admin_role' }];
    if (!(days >= 1 && days <= 365) || (max_uses !== null && !(max_uses >= 1 && max_uses <= 1000))) {
      return [400, { error: 'invalid_invite' }];
    }
    const now = new Date();
    const invite = {
      id: nextInviteId++,
      role_id,
      role: role.name,
      note: note ?? null,
      created_by: `tester-${token}`,
      created_at: now.toISOString(),
      expires_at: new Date(now.getTime() + days * 86_400_000).toISOString(),
      max_uses,
      uses: 0,
      revoked: false,
      live: true,
      joined: [],
      link: `${site}/invite#e2e${nextInviteId}`,
    };
    invites.push(invite);
    return [200, invite];
  },
});

createServer((req, res) => {
  let body = '';
  req.on('data', (chunk) => (body += chunk));
  req.on('end', () => {
    const url = new URL(req.url, 'http://fake');
    const dynamic = dynamicRoutes.find(([method, pattern]) => method === req.method && pattern.test(url.pathname));
    const route = routes[`${req.method} ${url.pathname}`] ?? (dynamic && ((b) => dynamic[2](url.pathname.match(dynamic[1]), b)));
    let parsed = {};
    try {
      parsed = body ? JSON.parse(body) : {};
    } catch {}
    const [status, reply] = route ? route(parsed, url.searchParams) : [404, { error: 'not_faked' }];
    if (!route) console.error(`fake-upstreams: no stand-in for ${req.method} ${req.url}`);
    res.writeHead(status, { 'content-type': typeof reply === 'string' ? 'text/plain' : 'application/json' });
    res.end(typeof reply === 'string' ? reply : JSON.stringify(reply));
  });
}).listen(port, '127.0.0.1', () => console.log(`fake-upstreams on 127.0.0.1:${port}`));

// ---- ai_pipeline (the camera WebSocket's upstream) ----
//
// Answers every camera frame with the contract's `frame` message (contracts/arcane), numbered
// as the frames come, and every fifth frame is also a settled `roll` (the contract's, with a
// roll id of its own). A minimal WebSocket server: the handshake, and unfragmented frames.

const aiPort = Number(process.env.FAKE_AI_PORT ?? 4011);
const contract = (name) => JSON.parse(readFileSync(new URL(`../../../../contracts/arcane/${name}.json`, import.meta.url), 'utf8'));
const FRAME = contract('frame');
const ROLL = contract('roll');
let connections = 0;

/** One unmasked text frame (server to client). */
function textFrame(text) {
  const payload = Buffer.from(text);
  const n = payload.length;
  const head = n < 126 ? Buffer.from([0x81, n]) : n < 65536 ? Buffer.from([0x81, 126, n >> 8, n & 0xff]) : null;
  if (!head) {
    const big = Buffer.alloc(10);
    big[0] = 0x81;
    big[1] = 127;
    big.writeBigUInt64BE(BigInt(n), 2);
    return Buffer.concat([big, payload]);
  }
  return Buffer.concat([head, payload]);
}

const ai = createServer((_, res) => res.writeHead(426).end());
ai.on('upgrade', (req, socket) => {
  const accept = createHash('sha1').update(`${req.headers['sec-websocket-key']}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  const conn = ++connections;
  let frames = 0;
  let buf = Buffer.alloc(0);
  socket.on('error', () => {});
  socket.on('data', (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    for (;;) {
      if (buf.length < 2) return;
      const opcode = buf[0] & 0x0f;
      const masked = buf[1] & 0x80;
      let len = buf[1] & 0x7f;
      let at = 2;
      if (len === 126) {
        if (buf.length < 4) return;
        len = buf.readUInt16BE(2);
        at = 4;
      } else if (len === 127) {
        if (buf.length < 10) return;
        len = Number(buf.readBigUInt64BE(2));
        at = 10;
      }
      // The browser side's frames are masked; a frame's bytes aren't read, so they stay so.
      if (masked) at += 4;
      if (buf.length < at + len) return;
      buf = buf.subarray(at + len);
      if (opcode === 0x8) return void socket.end(Buffer.from([0x88, 0]));
      if (opcode === 0x9) socket.write(Buffer.from([0x8a, 0]));
      if (opcode !== 0x2) continue;
      frames += 1;
      socket.write(textFrame(JSON.stringify({ ...FRAME, frame_seq: frames })));
      if (frames % 5 === 0) {
        socket.write(textFrame(JSON.stringify({ ...ROLL, roll_id: `e2e-${conn}-${frames}`, frame_seq: frames, ts: Date.now() })));
      }
    }
  });
});
ai.listen(aiPort, '127.0.0.1', () => console.log(`fake ai_pipeline on 127.0.0.1:${aiPort}`));

