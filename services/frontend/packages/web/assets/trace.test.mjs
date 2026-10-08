// Tests for trace.js: run with `npm run test:trace` from services/frontend.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';

const SRC = readFileSync(new URL('./trace.js', import.meta.url), 'utf8');
const REMOTE = '00-' + 'a'.repeat(32) + '-' + 'b'.repeat(16) + '-01';

// Deterministic clock: timers fire only when advance() passes their due time.
function clock() {
  let t = 1_000_000;
  let id = 0;
  const timers = new Map();
  return {
    now: () => t,
    setTimeout: (fn, ms) => { timers.set(++id, { fn, at: t + ms }); return id; },
    clearTimeout: (h) => { timers.delete(h); },
    advance(ms) {
      const end = t + ms;
      for (;;) {
        const due = [...timers.entries()].filter(([, v]) => v.at <= end).sort((a, b) => a[1].at - b[1].at)[0];
        if (!due) break;
        timers.delete(due[0]);
        t = due[1].at;
        due[1].fn();
      }
      t = end;
    },
  };
}

// Records spans with their parent; contexts are plain objects carrying `span`.
function fakeOtel(clk) {
  const spans = [];
  const stack = [{}];
  const otel = {
    trace: {
      getTracer: () => ({
        startSpan(name, opts, ctx) {
          const span = { name, opts, parent: opts.root ? null : ctx.span, endTime: undefined,
            end(t) { this.endTime = t ?? clk.now(); } };
          spans.push(span);
          return span;
        },
      }),
      setSpan: (ctx, span) => ({ ...ctx, span }),
      setSpanContext: (ctx, sc) => ({ ...ctx, span: { remote: sc } }),
    },
    context: {
      active: () => stack[stack.length - 1],
      with(ctx, fn) { stack.push(ctx); try { return fn(); } finally { stack.pop(); } },
    },
  };
  return { otel, spans };
}

function el(tagName, attrs = {}, textContent = '') {
  const node = {
    tagName: tagName.toUpperCase(), textContent, type: attrs.type,
    getAttribute: (k) => (k in attrs ? attrs[k] : null),
    closest: () => node,
  };
  return node;
}

function setup({ meta = null, readyState = 'loading' } = {}) {
  const clk = clock();
  const { otel, spans } = fakeOtel(clk);
  const listeners = {};
  const calls = [];
  let respond = () => Promise.resolve({ ok: true });
  const win = {
    location: { href: 'https://milesstorm.com/login', pathname: '/login', origin: 'https://milesstorm.com' },
    addEventListener: (type, fn) => { listeners['win:' + type] = fn; },
    fetch(input, init) { calls.push({ input, init, self: this, parent: otel.context.active().span }); return respond(input); },
  };
  const doc = {
    readyState,
    addEventListener: (type, fn) => { listeners[type] = fn; },
    querySelector: () => (meta === null ? null : { getAttribute: () => meta }),
  };
  const sandbox = { URL, Date, crypto };
  vm.runInNewContext(SRC, sandbox);
  const api = sandbox.__msTrace;
  const tracer = api.create({
    otel, window: win, document: doc, now: clk.now, setTimeout: clk.setTimeout, clearTimeout: clk.clearTimeout,
    // timeOrigin lags the wall clock, as after the machine sleeps; only now() - responseStart counts.
    performance: { timeOrigin: 500_000, now: () => 30, getEntriesByType: () => [{ responseStart: 20 }] },
  });
  tracer.install();
  return {
    api, tracer, clk, spans, calls, win,
    click: (target) => listeners.click({ target }),
    submit: (target, submitter) => listeners.submit({ target, submitter }),
    load: () => listeners['win:load'](),
    respondWith: (fn) => { respond = fn; },
  };
}

const tick = () => new Promise((r) => setImmediate(r));

test('click then fetch creates a lazy root span that parents the fetch', async () => {
  const h = setup();
  h.clk.advance(10);
  h.click(el('button', {}, '  Sign\n  in  '));
  assert.equal(h.spans.length, 0, 'span must not exist before a fetch');
  h.clk.advance(50);
  await h.win.fetch('/bff/login_password', { method: 'POST' });
  assert.equal(h.spans.length, 1);
  const [span] = h.spans;
  assert.equal(span.name, 'click Sign in');
  assert.equal(span.parent, null);
  assert.equal(span.opts.root, true);
  assert.equal(span.opts.startTime, 1_000_010);
  assert.equal(span.opts.attributes['ui.event'], 'click');
  assert.equal(h.calls[0].parent, span);
  assert.equal(h.calls[0].self, h.win);
});

test('click without a fetch produces no span and is dropped', async () => {
  const h = setup();
  h.click(el('button', {}, 'Toggle'));
  h.clk.advance(1001);
  assert.equal(h.tracer.current(), null);
  await h.win.fetch('/bff/x');
  assert.equal(h.spans.length, 0);
  assert.equal(h.calls[0].parent, undefined);
});

test('a fetch started from the first fetch .then stays in the interaction', async () => {
  const h = setup();
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/bff/a').then(() => h.win.fetch('/bff/b'));
  assert.equal(h.spans.length, 1);
  assert.equal(h.calls[1].parent, h.spans[0]);
});

test('interaction ends QUIET_MS after its last fetch settles, at the settle time', async () => {
  const h = setup();
  h.click(el('button', {}, 'Go'));
  h.clk.advance(20);
  await h.win.fetch('/bff/a');
  await tick();
  const settled = h.clk.now();
  h.clk.advance(299);
  assert.equal(h.spans[0].endTime, undefined);
  h.clk.advance(1);
  assert.equal(h.spans[0].endTime, settled);
  assert.equal(h.tracer.current(), null);
  await h.win.fetch('/bff/later');
  assert.equal(h.calls[1].parent, undefined);
});

test('interaction ends at MAX_MS while a fetch is still in flight', async () => {
  const h = setup();
  h.respondWith(() => new Promise(() => {}));
  h.click(el('button', {}, 'Go'));
  h.win.fetch('/bff/slow');
  h.clk.advance(9999);
  assert.equal(h.spans[0].endTime, undefined);
  h.clk.advance(1);
  assert.equal(h.spans[0].endTime, 1_010_000);
});

test('a new interaction ends the previous one only at its first fetch', async () => {
  const h = setup();
  h.respondWith(() => new Promise(() => {}));
  h.click(el('button', {}, 'One'));
  h.win.fetch('/bff/a');
  h.clk.advance(5);
  h.click(el('button', {}, 'Two'));
  assert.equal(h.spans[0].endTime, undefined, 'a pending click does not end the current span');
  h.clk.advance(5);
  h.win.fetch('/bff/b');
  assert.equal(h.spans[0].endTime, 1_000_010);
  assert.equal(h.spans[1].name, 'click Two');
  assert.equal(h.spans[1].opts.startTime, 1_000_005);
  assert.equal(h.calls[1].parent, h.spans[1]);
});

test('submit replaces the pending click on the same button', async () => {
  const h = setup();
  const button = el('button', { type: 'submit' }, 'Log In');
  h.click(button);
  h.submit(el('form'), button);
  await h.win.fetch('/bff/login_password');
  assert.equal(h.spans.length, 1);
  assert.equal(h.spans[0].name, 'submit Log In');
  assert.equal(h.tracer.pending(), null);
});

test('a click that never fetches leaves the page load span alone', async () => {
  const h = setup({ meta: REMOTE });
  const [load] = h.spans;
  h.clk.advance(100);
  h.click(el('button', {}, 'Menu'));
  assert.equal(h.tracer.current().span, load);
  h.clk.advance(1000);
  assert.equal(h.tracer.pending(), null, 'the pending click expired');
  assert.equal(load.endTime, undefined);
  await h.win.fetch('/bff/get_account');
  assert.equal(h.calls[0].parent, load);
  assert.equal(h.spans.length, 1);
});

test('window load firing while an interaction is current', async () => {
  const h = setup({ meta: REMOTE });
  const [load] = h.spans;
  h.respondWith(() => new Promise(() => {}));
  h.clk.advance(100);
  h.click(el('button', {}, 'Go'));
  h.win.fetch('/bff/a');
  assert.equal(load.endTime, 1_000_100, 'ends at the click fetch, before window load');
  const [, click] = h.spans;
  h.clk.advance(50);
  h.load();
  h.clk.advance(2000);
  assert.equal(click.endTime, undefined, 'still waiting on its fetch');
  assert.equal(h.tracer.current().span, click);
  assert.equal(load.endTime, 1_000_100);
});

test('a form with nothing naming it takes its submitter label', async () => {
  const h = setup();
  h.submit(el('form'), el('button', { type: 'submit' }, ' Log In '));
  await h.win.fetch('/bff/login_password');
  assert.equal(h.spans[0].name, 'submit Log In');
  assert.equal(h.tracer.labelFor(el('form')), 'form');
  assert.equal(h.tracer.labelFor(el('form', { id: 'login' }), el('button', {}, 'Log In')), 'login');
});

test('fetch span names are method plus templated path without the server-fn hash', () => {
  const { fetchSpanName } = setup().api;
  assert.equal(fetchSpanName('POST', 'https://milesstorm.com/bff/login_password10033624767130250299'), 'POST /bff/login_password');
  assert.equal(fetchSpanName('POST', 'https://milesstorm.com/bff/login_password10033624767130250299?x=1#y'), 'POST /bff/login_password');
  assert.equal(fetchSpanName('GET', 'https://milesstorm.com/bff/items/42'), 'GET /bff/items/{id}');
  assert.equal(fetchSpanName('GET', 'https://milesstorm.com/bff/login_v2'), 'GET /bff/login_v2');
  assert.equal(fetchSpanName('GET', 'https://milesstorm.com/assets/web_bg-dxhd673222225b8b59d.wasm'), 'GET /assets/web_bg-dxhd673222225b8b59d.wasm');
  assert.equal(fetchSpanName('GET', 'https://milesstorm.com/blog/post12345678'), 'GET /blog/post12345678');
  assert.equal(fetchSpanName('GET', undefined), 'GET');
});

test('ignored URLs are neither parented nor counted', async () => {
  const h = setup();
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('https://milesstorm.com/faro/collect', { method: 'POST' });
  await h.win.fetch(new URL('https://milesstorm.com/api/arcane/rolls'));
  await h.win.fetch('https://milesstorm.com/cdn-cgi/rum', { method: 'POST' });
  assert.equal(h.spans.length, 0);
  assert.equal(h.calls[0].parent, undefined);
  assert.equal(h.calls[1].parent, undefined);
  assert.equal(h.calls[2].parent, undefined);
  assert.equal(h.tracer.pending().inflight, 0);
  assert.equal(h.tracer.current(), null);
});

test('Request objects are recognised by their url', async () => {
  const h = setup();
  h.submit(el('form', { name: 'login', action: '/bff/login' }));
  await h.win.fetch({ url: 'https://milesstorm.com/bff/login_password' });
  assert.equal(h.spans[0].name, 'submit login');
  assert.equal(h.calls[0].parent, h.spans[0]);
});

test('page load span is a child of the meta traceparent and parents fetches until loaded and quiet', async () => {
  const h = setup({ meta: REMOTE });
  assert.equal(h.spans.length, 1);
  const [span] = h.spans;
  assert.equal(span.name, 'page load /login');
  assert.deepEqual({ ...span.parent.remote }, { traceId: 'a'.repeat(32), spanId: 'b'.repeat(16), traceFlags: 1, isRemote: true });
  assert.equal(span.opts.startTime, 999_990);
  await h.win.fetch('/assets/web.wasm');
  await tick();
  assert.equal(h.calls[0].parent, span);
  h.clk.advance(1000);
  assert.equal(span.endTime, undefined, 'stays open until window load');
  h.load();
  h.clk.advance(1000);
  await h.win.fetch('/bff/get_account123456789');
  await tick();
  assert.equal(h.calls[1].parent, span, 'a hydration-time call 1s after load still joins');
  h.clk.advance(1499);
  assert.equal(span.endTime, undefined);
  h.clk.advance(1);
  assert.equal(span.endTime, 1_002_000, 'ends at the last settle');
});

test('page load span never ends before window load', async () => {
  const h = setup({ meta: REMOTE });
  await h.win.fetch('/assets/web.wasm');
  await tick();
  h.clk.advance(800);
  h.load();
  h.clk.advance(1500);
  assert.equal(h.spans[0].endTime, 1_000_800);
});

test('page load is capped at 15s and ended early by a user interaction', () => {
  const capped = setup({ meta: REMOTE });
  capped.clk.advance(15000);
  assert.equal(capped.spans[0].endTime, 1_015_000);
  const h = setup({ meta: REMOTE });
  h.clk.advance(100);
  h.click(el('a', { href: '/blog/42' }, 'secret text'));
  h.win.fetch('/bff/post');
  assert.equal(h.spans[0].endTime, 1_000_100);
});

test('invalid traceparent metas produce no page load span', () => {
  for (const meta of ['', 'garbage', '00-' + '0'.repeat(32) + '-' + 'b'.repeat(16) + '-01',
    '00-' + 'a'.repeat(32) + '-' + '0'.repeat(16) + '-01', '01-' + 'a'.repeat(32) + '-' + 'b'.repeat(16) + '-01',
    '00-' + 'A'.repeat(32) + '-' + 'b'.repeat(16) + '-01']) {
    assert.equal(setup({ meta }).spans.length, 0, meta);
  }
});

test('anchors are named by templated path, never by their text', async () => {
  const h = setup();
  h.click(el('a', { href: '/blog/42/comments/1234?x=secret' }, 'Private title'));
  await h.win.fetch('/bff/post');
  assert.equal(h.spans[0].name, 'click /blog/{id}/comments/{id}');
  assert.equal(h.tracer.labelFor(el('button', { 'aria-label': 'Close' }, 'x')), 'Close');
  assert.equal(h.tracer.labelFor(el('div', { 'data-trace-name': 'roll dice' }, 'ignored')), 'roll dice');
  assert.equal(h.tracer.labelFor(el('button', {}, 'x'.repeat(60))), 'x'.repeat(40));
});

test('wrapper returns the same response and rethrows the same rejection', async () => {
  const h = setup();
  const response = { ok: true };
  const failure = new TypeError('network down');
  h.click(el('button', {}, 'Go'));
  h.respondWith(() => Promise.resolve(response));
  assert.equal(await h.win.fetch('/bff/a'), response);
  h.respondWith(() => Promise.reject(failure));
  await assert.rejects(h.win.fetch('/bff/b'), (e) => e === failure);
  h.respondWith(() => { throw failure; });
  assert.throws(() => h.win.fetch('/bff/c'), (e) => e === failure);
  await tick();
  assert.equal(h.tracer.current().inflight, 0);
});

function bootSandbox(hostname) {
  const sandbox = {
    inits: 0,
    URL, Date, setTimeout, clearTimeout, crypto,
    location: { origin: 'https://' + hostname, hostname },
    document: { readyState: 'loading', addEventListener() {}, querySelector: () => null },
    GrafanaFaroWebSdk: {
      FetchTransport: class { constructor(opts) { this.opts = opts; } },
      ErrorsInstrumentation: class {},
      WebVitalsInstrumentation: class {},
      SessionInstrumentation: class {},
      ViewInstrumentation: class {},
      initializeFaro: (cfg) => { sandbox.inits++; sandbox.cfg = cfg; return { api: { getOTEL: () => fakeOtel(clock()).otel } }; },
    },
    GrafanaFaroWebTracing: { TracingInstrumentation: class { constructor(opts) { sandbox.tracingOpts = opts; } } },
  };
  sandbox.window = { location: { href: `https://${hostname}/` }, addEventListener() {}, fetch() {} };
  vm.createContext(sandbox);
  return sandbox;
}

test('loading the script twice keeps the first instance and boots Faro once', () => {
  const sandbox = bootSandbox('milesstorm.com');
  vm.runInContext(SRC, sandbox);
  const first = sandbox.__msTrace;
  const wrapped = sandbox.window.fetch;
  vm.runInContext(SRC, sandbox);
  assert.equal(sandbox.inits, 1);
  assert.equal(sandbox.__msTrace, first);
  assert.equal(sandbox.window.fetch, wrapped);
  assert.equal(sandbox.cfg.url, undefined);
  const [transport] = sandbox.cfg.transports;
  assert.equal(transport.opts.url, 'https://milesstorm.com/faro/collect');
  assert.match(transport.opts.requestOptions.headers.traceparent, /^00-[0-9a-f]{32}-[0-9a-f]{16}-00$/);
  assert.equal(sandbox.cfg.instrumentations.length, 5);
  assert.equal(sandbox.cfg.app.name, 'milesstorm-web');
  assert.equal(sandbox.cfg.app.environment, 'production');
});

test('the fetch request hook renames Faro fetch spans', () => {
  const sandbox = bootSandbox('milesstorm.com');
  vm.runInContext(SRC, sandbox);
  const hook = sandbox.tracingOpts.instrumentationOptions.fetchInstrumentationOptions.requestHook;
  const span = { name: 'POST', attributes: { 'http.request.method': 'POST', 'url.full': 'https://milesstorm.com/bff/login_password10033624767130250299?x=1' },
    updateName(n) { this.name = n; } };
  hook(span, {});
  assert.equal(span.name, 'POST /bff/login_password');
});

test('Faro is not started off milesstorm.com', () => {
  for (const host of ['localhost', 'staging.milesstorm.com']) {
    const sandbox = bootSandbox(host);
    vm.runInContext(SRC, sandbox);
    assert.equal(sandbox.inits, 0, host);
    assert.equal(sandbox.tracingOpts, undefined, host);
    assert.equal(typeof sandbox.__msTrace.create, 'function', host);
  }
});

test('beforeSend strips query strings and fragments from page and span URLs', () => {
  const h = setup();
  const item = h.api.scrubItem({
    meta: { page: { url: 'https://milesstorm.com/login?next=/x#top' } },
    payload: { resourceSpans: [{ scopeSpans: [{ spans: [{ attributes: [
      { key: 'http.url', value: { stringValue: 'https://milesstorm.com/bff/a?token=1' } },
      { key: 'url.full', value: { stringValue: 'https://milesstorm.com/bff/b#frag' } },
    ] }] }] }] },
  });
  assert.equal(item.meta.page.url, 'https://milesstorm.com/login');
  const attrs = item.payload.resourceSpans[0].scopeSpans[0].spans[0].attributes;
  assert.equal(attrs[0].value.stringValue, 'https://milesstorm.com/bff/a');
  assert.equal(attrs[1].value.stringValue, 'https://milesstorm.com/bff/b');
  const ctx = h.api.scrubItem({ payload: { context: { url: 'https://milesstorm.com/reset?code=abc', href: '/x#t', page_url: '/p?q', other: '/keep?q' } } });
  assert.deepEqual({ ...ctx.payload.context }, { url: 'https://milesstorm.com/reset', href: '/x', page_url: '/p', other: '/keep?q' });
  const event = h.api.scrubItem({ payload: { attributes: { 'http.url': 'https://milesstorm.com/bff/a?q=1' } } });
  assert.equal(event.payload.attributes['http.url'], 'https://milesstorm.com/bff/a');
});

test('beacon traceparent is unsampled with random non-zero ids', () => {
  const { unsampledTraceparent } = setup().api;
  const a = unsampledTraceparent();
  assert.match(a, /^00-[0-9a-f]{32}-[0-9a-f]{16}-00$/);
  assert.notEqual(a, unsampledTraceparent());
});

test('beforeSend strips URLs from exception frames and messages', () => {
  const { scrubItem } = setup().api;
  const item = scrubItem({ payload: {
    type: 'Error',
    value: 'failed to load https://milesstorm.com/reset-password?code=s3cret (see "https://x.dev/a#frag")',
    stacktrace: { frames: [
      { filename: 'https://milesstorm.com/reset-password?code=s3cret', function: '?' },
      { filename: 'https://milesstorm.com/assets/web.js', lineno: 1 },
    ] },
  } });
  assert.equal(item.payload.value, 'failed to load https://milesstorm.com/reset-password (see "https://x.dev/a")');
  assert.equal(item.payload.stacktrace.frames[0].filename, 'https://milesstorm.com/reset-password');
  assert.equal(item.payload.stacktrace.frames[1].filename, 'https://milesstorm.com/assets/web.js');
  assert.ok(!JSON.stringify(item).includes('s3cret'));
});
