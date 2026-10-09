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

// Records spans with their parent; contexts are plain objects carrying `span`. Like OTel's SDK,
// spans report their start and end to `processor` when one is given.
function fakeOtel(clk, processor) {
  const spans = [];
  const stack = [{}];
  let ids = 0;
  const tracer = {
    startSpan(name, opts, ctx) {
      const parent = opts.root ? null : ctx.span || null;
      const traceId = parent ? (parent.remote || parent.ctx).traceId : 't' + ++ids;
      const span = {
        name, opts, parent, endTime: undefined, events: [], status: null,
        ctx: { traceId, spanId: 's' + ++ids, traceFlags: 1 },
        spanContext() { return this.ctx; },
        addEvent(n, attrs, time) {
          if (typeof attrs === 'number') [attrs, time] = [undefined, attrs];
          this.events.push({ name: n, attrs, time });
        },
        setStatus(st) { this.status = st; },
        end(t) { this.endTime = t ?? clk.now(); processor?.onEnd(this); },
      };
      spans.push(span);
      processor?.onStart(span);
      return span;
    },
  };
  const otel = {
    trace: {
      getTracer: () => tracer,
      setSpan: (ctx, span) => ({ ...ctx, span }),
      setSpanContext: (ctx, sc) => ({ ...ctx, span: { remote: sc } }),
    },
    context: {
      active: () => stack[stack.length - 1],
      with(ctx, fn) { stack.push(ctx); try { return fn(); } finally { stack.pop(); } },
    },
  };
  return { otel, spans, tracer };
}

function el(tagName, attrs = {}, textContent = '') {
  const node = {
    tagName: tagName.toUpperCase(), textContent, type: attrs.type,
    getAttribute: (k) => (k in attrs ? attrs[k] : null),
    closest: () => node,
  };
  return node;
}

// The page's navigation entry; performance.now() is 30 at clock 1_000_000.
const NAV = { responseStart: 20, responseEnd: 25, domInteractive: 40, domContentLoadedEventStart: 41,
  domContentLoadedEventEnd: 42, domComplete: 0, loadEventStart: 0, loadEventEnd: 0 };

// `fetchSpans`: each fetch gets a CLIENT child span, as Faro's fetch instrumentation does; the test
// ends it (`endFetchSpan`) the way OTel does: at the body's end, reported 300 ms later.
function setup({ meta = null, readyState = 'loading', fetchSpans = false, path = '/login' } = {}) {
  const clk = clock();
  const sandbox = { URL, Date, crypto };
  vm.runInNewContext(SRC, sandbox);
  const api = sandbox.__msTrace;
  const exported = [];
  const perfNow = () => 30 + (clk.now() - 1_000_000);
  const clockSync = api.clockSync({ now: clk.now, performance: { now: perfNow } });
  const processor = api.spanProcessor({
    exporter: { export: (batch, done) => { exported.push(...batch); done({ code: 0 }); } },
    clock: clockSync, setTimeout: clk.setTimeout, clearTimeout: clk.clearTimeout,
  });
  const { otel, spans, tracer: otelTracer } = fakeOtel(clk, processor);
  const listeners = {};
  const calls = [];
  const fetchSpanList = [];
  const sockets = [];
  const observers = [];
  let respond = () => Promise.resolve({ ok: true });
  class FakeSocket {
    constructor(url, protocols) { this.url = url; this.protocols = protocols; this.listeners = {}; sockets.push(this); }
    addEventListener(type, fn) { (this.listeners[type] ||= []).push(fn); }
    removeEventListener(type, fn) { this.listeners[type] = (this.listeners[type] || []).filter((f) => f !== fn); }
    fire(type) { (this.listeners[type] || []).slice().forEach((fn) => fn({ type })); }
  }
  FakeSocket.OPEN = 1;
  const win = {
    location: { href: 'https://milesstorm.com' + path, pathname: path, origin: 'https://milesstorm.com' },
    addEventListener: (type, fn) => { listeners['win:' + type] = fn; },
    history: { pushState(state, title, url) { win.location.pathname = new URL(url, win.location.href).pathname; } },
    WebSocket: FakeSocket,
    fetch(input, init) {
      const parent = otel.context.active().span;
      calls.push({ input, init, self: this, parent });
      if (fetchSpans) fetchSpanList.push(otelTracer.startSpan('POST', parent ? {} : { root: true }, otel.context.active()));
      return respond(input);
    },
  };
  const doc = {
    readyState,
    addEventListener: (type, fn) => { listeners[type] = fn; },
    querySelector: () => (meta === null ? null : { getAttribute: () => meta }),
  };
  const tracer = api.create({
    otel, window: win, document: doc, now: clk.now, setTimeout: clk.setTimeout, clearTimeout: clk.clearTimeout,
    // timeOrigin lags the wall clock, as after the machine sleeps; only now() - responseStart counts.
    performance: { timeOrigin: 500_000, now: perfNow, getEntriesByType: () => [NAV] },
    PerformanceObserver: class { constructor(fn) { this.fn = fn; } observe(opts) { observers.push({ fn: this.fn, opts }); } },
    spans: processor,
  });
  tracer.install();
  return {
    api, tracer, clk, spans, calls, win, exported, processor, clockSync, sockets, observers,
    fetchSpans: fetchSpanList,
    endFetchSpan: (i, at) => fetchSpanList[i].end(at),
    click: (target) => listeners.click({ target }),
    submit: (target, submitter) => listeners.submit({ target, submitter }),
    load: () => listeners['win:load'](),
    popstate: () => listeners['win:popstate'](),
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
    document: { readyState: 'loading', addEventListener() {}, querySelector: () => null, visibilityState: 'visible' },
    GrafanaFaroWebSdk: {
      FetchTransport: class { constructor(opts) { this.opts = opts; } },
      ErrorsInstrumentation: class {},
      WebVitalsInstrumentation: class {},
      SessionInstrumentation: class {},
      ViewInstrumentation: class {},
      faro: { api: { pushTraces: (t) => sandbox.pushed.push(t) }, metas: { value: {} } },
      initializeFaro: (cfg) => { sandbox.inits++; sandbox.cfg = cfg; return { api: { getOTEL: () => fakeOtel(clock()).otel } }; },
    },
    GrafanaFaroWebTracing: {
      TracingInstrumentation: class { constructor(opts) { sandbox.tracingOpts = opts; } },
      FaroTraceExporter: class { constructor(cfg) { this.cfg = cfg; } export(spans) { this.cfg.api.pushTraces(spans); } },
      FaroMetaAttributesSpanProcessor: class { constructor(inner, metas) { this.inner = inner; this.metas = metas; } },
    },
    pushed: [],
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

test('fetch spans keep their network timing events', () => {
  const sandbox = bootSandbox('milesstorm.com');
  vm.runInContext(SRC, sandbox);
  assert.equal(sandbox.tracingOpts.instrumentationOptions.fetchInstrumentationOptions.ignoreNetworkEvents, false);
});

test('Faro is not started off milesstorm.com and its staging host', () => {
  for (const host of ['localhost', 'dev.milesstorm.com', 'milesstorm.com.evil.example']) {
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

// Browser clock 60 ms behind the servers: the Gateway took the request at browser time 1_000_010
// (server 1_000_070) and answered 5 ms later; the browser saw requestStart/responseStart 20 ms apart.
function clockAt(nowMs = 1_000_100, perfNow = 100) {
  const { clockSync } = setup().api;
  return clockSync({ now: () => nowMs, performance: { now: () => perfNow } });
}
const gatewayEntry = (requestStart, responseStart, received, dur, extra = {}) => ({
  entryType: 'resource', initiatorType: 'fetch', name: 'https://milesstorm.com/bff/get_account123',
  transferSize: 300, requestStart, responseStart,
  serverTiming: [{ name: 'gateway', duration: dur, description: String(received) }], ...extra,
});
const payloadOf = (...spans) => ({ payload: { resourceSpans: [{ scopeSpans: [{ spans }] }] } });

test('clock offset is NTP\'s estimate from the Gateway Server-Timing stamp', () => {
  const c = clockAt();
  // base = now - perf.now = 1_000_000: t1 = 1_000_002, t4 = 1_000_022, t2 = 1_000_070, t3 = 1_000_075.
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5));
  assert.deepEqual({ ...c.best() }, { offset: 60.5, delay: 15 });
});

test('clock sync keeps the lowest-delay sample and ignores entries without a gateway stamp', () => {
  const c = clockAt();
  c.addEntry(gatewayEntry(2, 102, 1_000_080, 5));
  c.addEntry(gatewayEntry(2, 12, 1_000_065, 4));
  c.addEntry(gatewayEntry(2, 5, 0, 1, { serverTiming: [{ name: 'cfOrigin', duration: 1, description: '' }] }));
  c.addEntry(gatewayEntry(0, 5, 1_000_060, 1));
  assert.deepEqual({ ...c.best() }, { offset: 60, delay: 6 });
});

test('clock sync only trusts responses no cache can replay', () => {
  const c = clockAt();
  // A day-old stamp replayed by a cache: negative delay, huge offset. Static assets, cache hits
  // (transferSize 0) and non-/bff/ fetches are never sampled.
  c.addEntry(gatewayEntry(2, 3, 1_000_070 - 86_400_000, 4));
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { transferSize: 0 }));
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { name: 'https://milesstorm.com/assets/web.wasm' }));
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { initiatorType: 'img' }));
  c.addEntry(gatewayEntry(2, 4, 1_000_070, 5)); // delay -3 ms: not this request's stamp
  assert.equal(c.best(), null);
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { entryType: 'navigation', initiatorType: 'navigation', name: 'https://milesstorm.com/login' }));
  assert.equal(c.best().offset, 60.5);
});

test('beforeSend shifts browser spans by the offset and records it, leaving other items alone', () => {
  const c = clockAt();
  const untouched = payloadOf({ traceId: 't0', startTimeUnixNano: '5' });
  assert.equal(c.apply(untouched).payload.resourceSpans[0].scopeSpans[0].spans[0].startTimeUnixNano, '5');
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5));
  const span = { traceId: 't1', startTimeUnixNano: '1791537494592000001', endTimeUnixNano: 1000, events: [{ timeUnixNano: '0' }], attributes: [{ key: 'a', value: {} }] };
  c.apply(payloadOf(span));
  assert.equal(span.startTimeUnixNano, '1791537494652500001');
  assert.equal(span.endTimeUnixNano, 60_501_000);
  assert.equal(span.events[0].timeUnixNano, '60500000');
  assert.deepEqual(span.attributes.map((a) => [a.key, a.value.doubleValue]),
    [['a', undefined], ['browser.clock_offset_ms', 60.5], ['browser.clock_offset_error_ms', 7.5]]);
  const log = { payload: { message: 'x' } };
  assert.equal(c.apply(log), log);
});

test('every span of a trace gets the offset its trace was first sent with', () => {
  const c = clockAt();
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5)); // offset 60.5
  const first = { traceId: 't1', startTimeUnixNano: '0', endTimeUnixNano: '0' };
  c.apply(payloadOf(first));
  c.addEntry(gatewayEntry(2, 12, 1_000_065, 4)); // better sample: offset 60
  const later = { traceId: 't1', startTimeUnixNano: '0', endTimeUnixNano: '0' };
  const other = { traceId: 't2', startTimeUnixNano: '0', endTimeUnixNano: '0' };
  c.apply(payloadOf(later, other));
  assert.equal(first.startTimeUnixNano, '60500000');
  assert.equal(later.startTimeUnixNano, '60500000');
  assert.equal(other.startTimeUnixNano, '60000000');
});

test('beforeSend pins a trace it has not seen: with no sample yet, uncorrected for good', () => {
  const c = clockAt();
  const early = { traceId: 't1', startTimeUnixNano: '0', endTimeUnixNano: '0' };
  c.apply(payloadOf(early));
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5));
  const late = { traceId: 't1', startTimeUnixNano: '0', endTimeUnixNano: '0' };
  const next = { traceId: 't2', startTimeUnixNano: '0', endTimeUnixNano: '0' };
  c.apply(payloadOf(late, next));
  assert.deepEqual([early.startTimeUnixNano, late.startTimeUnixNano, late.attributes], ['0', '0', undefined]);
  assert.equal(next.startTimeUnixNano, '60500000');
});

test('a span that cannot be corrected is sent unchanged and never throws', () => {
  const c = clockAt();
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5));
  const bad = { traceId: 't1', startTimeUnixNano: '1e+21', endTimeUnixNano: '7' };
  const good = { traceId: 't2', startTimeUnixNano: '0', endTimeUnixNano: '0' };
  assert.doesNotThrow(() => c.apply(payloadOf(bad, good)));
  assert.deepEqual([bad.startTimeUnixNano, bad.endTimeUnixNano, bad.attributes], ['1e+21', '7', undefined]);
  assert.equal(good.startTimeUnixNano, '60500000');
});

test('Faro beforeSend scrubs URLs and then corrects the clock', () => {
  const sandbox = bootSandbox('milesstorm.com');
  vm.runInContext(SRC, sandbox);
  const item = sandbox.cfg.beforeSend({ meta: { page: { url: 'https://milesstorm.com/reset?code=abc' } }, payload: {} });
  assert.equal(item.meta.page.url, 'https://milesstorm.com/reset');
});

// ---- Root lifecycle, held-back traces, navigation, camera WebSocket, long tasks ----

const skewEntry = () => gatewayEntry(2, 22, 1_000_070, 5); // any usable sample
const ids = (spans) => spans.map((sp) => sp.name);

test('a click ends after its last child span, not at the fetch promise', async () => {
  const h = setup({ fetchSpans: true });
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/bff/a');
  await tick();
  const settled = h.clk.now();
  h.clk.advance(300);
  assert.equal(h.spans[0].endTime, undefined, 'its fetch span is still open');
  // OTel ends the fetch span at the body's end and reports it 300 ms later.
  h.endFetchSpan(0, settled + 1);
  h.clk.advance(0);
  assert.equal(h.spans[0].endTime, undefined, 'the quiet window runs from the child end');
  h.clk.advance(1);
  const [root, child] = h.spans;
  assert.equal(child.endTime, settled + 1);
  assert.equal(root.endTime, settled + 1, 'ends with its child');
  assert.ok(root.endTime >= child.endTime);
});

test('the quiet window runs from the child span end, and a later fetch keeps the root open', async () => {
  const h = setup({ fetchSpans: true });
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/bff/a');
  await tick();
  const t0 = h.clk.now();
  h.clk.advance(100);
  h.endFetchSpan(0, t0 + 50); // reported early: 250 ms of the window remain
  h.clk.advance(249);
  assert.equal(h.spans[0].endTime, undefined);
  await h.win.fetch('/bff/b');
  assert.equal(h.calls[1].parent, h.spans[0], 'still current');
  await tick();
  h.clk.advance(1000);
  assert.equal(h.spans[0].endTime, undefined, 'waits for the second child');
  h.endFetchSpan(1, h.clk.now() - 300);
  h.clk.advance(0);
  assert.equal(h.spans[0].endTime, h.clk.now() - 300);
});

test('a page load span follows the server render it is a child of', () => {
  const h = setup({ meta: REMOTE });
  assert.equal(h.spans[0].opts.attributes['trace.relation'], 'follows');
  assert.equal(h.spans[0].opts.attributes['ui.event'], 'load');
});

test('a page load span carries the navigation timing marks after its start', async () => {
  const h = setup({ meta: REMOTE });
  h.load();
  h.clk.advance(1500);
  const [load] = h.spans;
  assert.equal(load.endTime, 1_000_000);
  // performance.now() 30 is clock 1_000_000; marks before responseStart, and unset ones, are left out.
  assert.deepEqual(load.events.map((e) => [e.name, e.time]), [
    ['responseEnd', 999_995], ['domInteractive', 1_000_010], ['domContentLoadedEventStart', 1_000_011],
    ['domContentLoadedEventEnd', 1_000_012],
  ].filter(([, t]) => t <= load.endTime));
});

test('long tasks become events on the open span', async () => {
  const h = setup({ meta: REMOTE });
  const [obs] = h.observers;
  assert.deepEqual({ ...obs.opts }, { type: 'longtask', buffered: true });
  // A task before the page load span started is left out.
  obs.fn({ getEntries: () => [{ startTime: 5, duration: 80 }, { startTime: 25, duration: 120.4 }] });
  assert.deepEqual(h.spans[0].events.map((e) => [e.name, e.time, e.attrs['longtask.duration_ms']]),
    [['longtask', 999_995, 120]]);
  h.clk.advance(15000);
  obs.fn({ getEntries: () => [{ startTime: 15_100, duration: 60 }] });
  assert.equal(h.spans[0].events.filter((e) => e.name === 'longtask').length, 1, 'no span open: dropped');
});

test('a trace started before any clock sample is held back, then sent when a sample arrives', async () => {
  const h = setup({ fetchSpans: true });
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/bff/a');
  await tick();
  h.endFetchSpan(0, h.clk.now());
  h.clk.advance(100);
  assert.equal(h.exported.length, 0, 'held');
  assert.equal(h.spans[0].endTime, undefined);
  h.clockSync.addEntry(gatewayEntry(2, 22, 1_000_070, 5));
  h.clk.advance(1000);
  assert.deepEqual(ids(h.exported), ['POST', 'click Go'], 'released, then the root as usual');
  // The held spans and later spans of the trace get the sample that released them.
  const traceId = h.spans[0].ctx.traceId;
  const otlp = (start) => ({ traceId, startTimeUnixNano: start, endTimeUnixNano: start });
  h.clockSync.addEntry(gatewayEntry(2, 12, 1_000_065, 4)); // a better sample (offset 90) comes too late
  const sent = [otlp('0'), otlp('0')];
  h.clockSync.apply(payloadOf(...sent));
  assert.deepEqual(sent.map((sp) => sp.startTimeUnixNano), ['90500000', '90500000'], 'offset 90.5');
});

test('a held trace whose root ends without a sample is sent uncorrected, and stays so', async () => {
  const h = setup({ fetchSpans: true });
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/bff/a');
  await tick();
  h.endFetchSpan(0, h.clk.now());
  h.clk.advance(300);
  assert.notEqual(h.spans[0].endTime, undefined, 'root ended');
  h.clk.advance(1000);
  assert.deepEqual(ids(h.exported).sort(), ['POST', 'click Go']);
  h.clockSync.addEntry(skewEntry());
  const late = { traceId: h.spans[0].ctx.traceId, startTimeUnixNano: '0', endTimeUnixNano: '0' };
  h.clockSync.apply(payloadOf(late));
  assert.deepEqual([late.startTimeUnixNano, late.attributes], ['0', undefined]);
});

test('a trace started after a sample is sent without holding, in batches', async () => {
  const h = setup({ fetchSpans: true });
  h.clockSync.addEntry(skewEntry());
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/bff/a');
  await tick();
  h.endFetchSpan(0, h.clk.now());
  assert.equal(h.exported.length, 0, 'batched');
  h.clk.advance(999);
  assert.equal(h.exported.length, 0);
  h.clk.advance(1);
  assert.deepEqual(ids(h.exported), ['POST', 'click Go'], 'the batch went 1 s after its first span');
  // 30 ended spans go at once.
  for (let i = 0; i < 30; i++) h.processor.onEnd({ spanContext: () => ({ traceId: 'x' + i, spanId: 's' }), endTime: [1, 0] });
  assert.equal(h.exported.length, 32);
});

test('forceFlush sends held traces uncorrected', () => {
  const h = setup({ meta: REMOTE, fetchSpans: true });
  h.win.fetch('/assets/web.wasm');
  h.endFetchSpan(0, h.clk.now());
  assert.equal(h.exported.length, 0);
  h.processor.forceFlush();
  assert.deepEqual(ids(h.exported), ['POST']);
});

test('client-side route changes start a lazy navigate root named by templated path', async () => {
  const h = setup({ path: '/' });
  h.win.history.pushState(null, '', '/blog/42?code=secret#x');
  assert.equal(h.spans.length, 0, 'no span before a server call');
  await h.win.fetch('/bff/get_post');
  assert.equal(h.spans[0].name, 'navigate /blog/{id}');
  assert.equal(h.spans[0].opts.root, true);
  assert.equal(h.spans[0].opts.attributes['ui.event'], 'navigate');
  assert.equal(h.calls[0].parent, h.spans[0]);
  // Back button.
  h.win.location.pathname = '/';
  h.popstate();
  await h.win.fetch('/bff/list');
  assert.equal(h.spans[1].name, 'navigate /');
  assert.equal(h.spans[0].endTime !== undefined, true, 'the new root ended the old one');
});

test('a route change caused by a click keeps the click as the root; same-route pushes are ignored', async () => {
  const h = setup({ path: '/' });
  h.win.history.pushState(null, '', '/?tab=2');
  h.click(el('a', { href: '/login' }, 'Log in'));
  h.win.history.pushState(null, '', '/login');
  await h.win.fetch('/bff/a');
  assert.deepEqual(ids(h.spans), ['click /login']);
});

test('route changes while the page load is current stay in the page load', async () => {
  const h = setup({ meta: REMOTE, path: '/' });
  h.win.history.pushState(null, '', '/login');
  await h.win.fetch('/bff/check_login_status');
  assert.deepEqual(ids(h.spans), ['page load /']);
  assert.equal(h.calls[0].parent, h.spans[0]);
});

test('the arcane WebSocket gets a camera session span whose context rides in the URL', () => {
  const h = setup();
  const ws = new h.win.WebSocket('wss://milesstorm.com/ws/arcane');
  const [span] = h.spans;
  assert.equal(span.name, 'camera session');
  assert.equal(span.opts.root, true, 'no click in progress: a root');
  assert.equal(ws.url, `wss://milesstorm.com/ws/arcane?traceparent=00-${span.ctx.traceId}-${span.ctx.spanId}-01`);
  assert.equal(ws.protocols, undefined);
  assert.ok(ws instanceof h.win.WebSocket);
  assert.equal(h.win.WebSocket.OPEN, 1);
  h.clk.advance(40);
  ws.fire('open');
  assert.equal(span.endTime, 1_000_040, 'ends at the handshake');
  assert.equal(span.status, null);
  ws.fire('close');
  assert.equal(span.endTime, 1_000_040);
});

test('a camera session opened by a click is its child and keeps it open; a failed handshake is an error', () => {
  const h = setup();
  h.click(el('button', {}, 'Start camera'));
  const ws = new h.win.WebSocket('wss://milesstorm.com/ws/arcane?x=1', ['p']);
  const [click, camera] = h.spans;
  assert.equal(click.name, 'click Start camera');
  assert.equal(camera.parent, click);
  assert.equal(ws.protocols[0], 'p');
  assert.match(ws.url, /\/ws\/arcane\?x=1&traceparent=00-/);
  h.clk.advance(500);
  assert.equal(click.endTime, undefined, 'waits for the handshake');
  ws.fire('error');
  assert.deepEqual({ ...camera.status }, { code: 2, message: 'WebSocket error before open' });
  h.clk.advance(300);
  assert.equal(click.endTime, 1_000_500);
});

test('a camera session opened during the page load is a root of its own', () => {
  const h = setup({ meta: REMOTE });
  new h.win.WebSocket('wss://milesstorm.com/ws/arcane');
  assert.deepEqual(ids(h.spans), ['page load /login', 'camera session']);
  assert.equal(h.spans[1].opts.root, true);
});

test('other WebSockets are left alone', () => {
  const h = setup();
  const ws = new h.win.WebSocket('wss://milesstorm.com/ws/other');
  assert.equal(ws.url, 'wss://milesstorm.com/ws/other');
  assert.equal(h.spans.length, 0);
});

test('staging starts Faro tagged staging, with the span processor in Faro\'s chain', () => {
  const sandbox = bootSandbox('staging.milesstorm.com');
  vm.runInContext(SRC, sandbox);
  assert.equal(sandbox.inits, 1);
  assert.equal(sandbox.cfg.app.environment, 'staging');
  assert.equal(sandbox.cfg.transports[0].opts.url, 'https://staging.milesstorm.com/faro/collect');
  const chain = sandbox.tracingOpts.spanProcessor;
  assert.equal(typeof chain.inner.onEnd, 'function');
  assert.deepEqual({ ...chain.metas.value }, {});
});
