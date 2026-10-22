// Tests for trace.js: run with `npm run test:trace` in services/frontend/web.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import * as trace from './trace.js';

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
        end(t) { if (this.endTime !== undefined) return; this.endTime = t ?? clk.now(); processor?.onEnd(this); },
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
function setup({ meta = null, readyState = 'loading', fetchSpans = false, path = '/login', userActions, Observer, onLongFrame } = {}) {
  const clk = clock();
  const api = trace;
  const exported = [];
  const perfNow = () => 30 + (clk.now() - 1_000_000);
  const clockSync = api.clockSync({ now: clk.now, performance: { now: perfNow } });
  const processor = api.spanProcessor({
    next: { onEnd: (span) => exported.push(span), forceFlush: () => Promise.resolve(), shutdown: () => Promise.resolve() },
    clock: clockSync,
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
    PerformanceObserver: Observer || class { constructor(fn) { this.fn = fn; } observe(opts) { observers.push({ fn: this.fn, opts }); } },
    spans: processor,
    userActions,
    onLongFrame,
  });
  tracer.install();
  return {
    api, tracer, clk, spans, calls, win, exported, processor, clockSync, sockets, observers,
    fetchSpans: fetchSpanList,
    endFetchSpan: (i, at) => fetchSpanList[i].end(at),
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
  await h.win.fetch('/api/session/login', { method: 'POST' });
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
  await h.win.fetch('/api/x');
  assert.equal(h.spans.length, 0);
  assert.equal(h.calls[0].parent, undefined);
});

test('a fetch started from the first fetch .then stays in the interaction', async () => {
  const h = setup();
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/api/a').then(() => h.win.fetch('/api/b'));
  assert.equal(h.spans.length, 1);
  assert.equal(h.calls[1].parent, h.spans[0]);
});

test('interaction ends QUIET_MS after its last fetch settles, at the settle time', async () => {
  const h = setup();
  h.click(el('button', {}, 'Go'));
  h.clk.advance(20);
  await h.win.fetch('/api/a');
  await tick();
  const settled = h.clk.now();
  h.clk.advance(299);
  assert.equal(h.spans[0].endTime, undefined);
  h.clk.advance(1);
  assert.equal(h.spans[0].endTime, settled);
  assert.equal(h.tracer.current(), null);
  await h.win.fetch('/api/later');
  assert.equal(h.calls[1].parent, undefined);
});

test('interaction ends at MAX_MS while a fetch is still in flight', async () => {
  const h = setup();
  h.respondWith(() => new Promise(() => {}));
  h.click(el('button', {}, 'Go'));
  h.win.fetch('/api/slow');
  h.clk.advance(9999);
  assert.equal(h.spans[0].endTime, undefined);
  h.clk.advance(1);
  assert.equal(h.spans[0].endTime, 1_010_000);
});

test('a new interaction ends the previous one only at its first fetch', async () => {
  const h = setup();
  h.respondWith(() => new Promise(() => {}));
  h.click(el('button', {}, 'One'));
  h.win.fetch('/api/a');
  h.clk.advance(5);
  h.click(el('button', {}, 'Two'));
  assert.equal(h.spans[0].endTime, undefined, 'a pending click does not end the current span');
  h.clk.advance(5);
  h.win.fetch('/api/b');
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
  await h.win.fetch('/api/session/login');
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
  await h.win.fetch('/api/account');
  assert.equal(h.calls[0].parent, load);
  assert.equal(h.spans.length, 1);
});

test('window load firing while an interaction is current', async () => {
  const h = setup({ meta: REMOTE });
  const [load] = h.spans;
  h.respondWith(() => new Promise(() => {}));
  h.clk.advance(100);
  h.click(el('button', {}, 'Go'));
  h.win.fetch('/api/a');
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
  await h.win.fetch('/api/session/login');
  assert.equal(h.spans[0].name, 'submit Log In');
  assert.equal(h.tracer.labelFor(el('form')), 'form');
  assert.equal(h.tracer.labelFor(el('form', { id: 'login' }), el('button', {}, 'Log In')), 'login');
});

test('fetch span names are method plus templated path', () => {
  const { fetchSpanName } = setup().api;
  assert.equal(fetchSpanName('POST', 'https://milesstorm.com/api/session/login'), 'POST /api/session/login');
  assert.equal(fetchSpanName('POST', 'https://milesstorm.com/api/session/login?x=1#y'), 'POST /api/session/login');
  assert.equal(fetchSpanName('DELETE', 'https://milesstorm.com/api/admin/invites/42'), 'DELETE /api/admin/invites/{id}');
  assert.equal(fetchSpanName('GET', 'https://milesstorm.com/api/login_v2'), 'GET /api/login_v2');
  assert.equal(fetchSpanName('GET', 'https://milesstorm.com/_astro/client.Ab12Cd.js'), 'GET /_astro/client.Ab12Cd.js');
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
  h.submit(el('form', { name: 'login', action: '/api/session/login' }));
  await h.win.fetch({ url: 'https://milesstorm.com/api/session/login' });
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
  await h.win.fetch('/_astro/client.Ab12Cd.js');
  await tick();
  assert.equal(h.calls[0].parent, span);
  h.clk.advance(1000);
  assert.equal(span.endTime, undefined, 'stays open until window load');
  h.load();
  h.clk.advance(1000);
  await h.win.fetch('/api/account');
  await tick();
  assert.equal(h.calls[1].parent, span, 'a hydration-time call 1s after load still joins');
  h.clk.advance(1499);
  assert.equal(span.endTime, undefined);
  h.clk.advance(1);
  assert.equal(span.endTime, 1_002_000, 'ends at the last settle');
});

test('page load span never ends before window load', async () => {
  const h = setup({ meta: REMOTE });
  await h.win.fetch('/_astro/client.Ab12Cd.js');
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
  h.win.fetch('/api/post');
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
  await h.win.fetch('/api/post');
  assert.equal(h.spans[0].name, 'click /blog/{id}/comments/{id}');
  assert.equal(h.tracer.labelFor(el('button', { 'aria-label': 'Close' }, 'x')), 'Close');
  assert.equal(h.tracer.labelFor(el('div', { 'data-faro-user-action-name': 'roll dice' }, 'ignored')), 'roll dice');
  assert.equal(h.tracer.labelFor(el('button', {}, 'x'.repeat(60))), 'x'.repeat(40));
});

test('wrapper returns the same response and rethrows the same rejection', async () => {
  const h = setup();
  const response = { ok: true };
  const failure = new TypeError('network down');
  h.click(el('button', {}, 'Go'));
  h.respondWith(() => Promise.resolve(response));
  assert.equal(await h.win.fetch('/api/a'), response);
  h.respondWith(() => Promise.reject(failure));
  await assert.rejects(h.win.fetch('/api/b'), (e) => e === failure);
  h.respondWith(() => { throw failure; });
  assert.throws(() => h.win.fetch('/api/c'), (e) => e === failure);
  await tick();
  assert.equal(h.tracer.current().inflight, 0);
});

// A page for boot(): fake Faro exports that record what boot asks of them.
function bootSandbox(hostname, { version = null } = {}) {
  const order = [];
  const listeners = {};
  let action = null;
  const api = {
    pushTraces: (t) => sandbox.pushed.push(t),
    getOTEL: () => fakeOtel(clock()).otel,
    getActiveUserAction: () => action,
    startUserAction(name, attributes, options) {
      action = { name, attributes, options, parentId: 'ua1', getState: () => 0, end() { sandbox.ended++; action = null; } };
      sandbox.actions.push(action);
      return action;
    },
    pushEvent: (name, attributes) => sandbox.events.push([name, attributes]),
    pushMeasurement: (measurement, options) => sandbox.measurements.push([measurement, options]),
  };
  const named = (name) => class { constructor() { this.name = name; } };
  const sandbox = {
    inits: 0, pushed: [], actions: [], events: [], measurements: [], ended: 0, flushed: 0, order, listeners,
    location: { origin: 'https://' + hostname, hostname, href: `https://${hostname}/`, pathname: '/' },
    document: {
      readyState: 'loading', visibilityState: 'visible',
      addEventListener(type, fn) { listeners['doc:' + type] = fn; },
      querySelector: (selector) => (selector === 'meta[name="service-version"]' && version ? { getAttribute: () => version } : null),
    },
    addEventListener(type, fn) { order.push('page:' + type); (listeners[type] ||= []).push(fn); },
    setTimeout, clearTimeout,
    fetch() {},
  };
  sandbox.sdk = {
    FetchTransport: class { constructor(opts) { this.opts = opts; order.push('transport'); } },
    ErrorsInstrumentation: named('errors'),
    WebVitalsInstrumentation: named('web-vitals'),
    SessionInstrumentation: named('session'),
    ViewInstrumentation: named('view'),
    ConsoleInstrumentation: named('console'),
    PerformanceInstrumentation: named('performance'),
    UserActionInstrumentation: named('user-actions'),
    LogLevel: { TRACE: 'trace', DEBUG: 'debug', LOG: 'log', INFO: 'info', WARN: 'warn', ERROR: 'error' },
    UserActionState: { Started: 0 },
    faro: { api, metas: { value: {} } },
    initializeFaro: (cfg) => { sandbox.inits++; sandbox.cfg = cfg; return sandbox.sdk.faro; },
  };
  sandbox.tracing = {
    TracingInstrumentation: class { constructor(opts) { this.name = 'tracing'; sandbox.tracingOpts = opts; } },
    FaroTraceExporter: class { constructor(cfg) { this.cfg = cfg; } export(spans) { this.cfg.api.pushTraces(spans); } },
    FaroMetaAttributesSpanProcessor: class { constructor(inner, metas) { this.inner = inner; this.metas = metas; } },
  };
  sandbox.BatchSpanProcessor = class {
    constructor(exporter, opts) { this.exporter = exporter; sandbox.batchOpts = opts; }
    onEnd(span) { this.exporter.export([span], () => {}); }
    forceFlush() { sandbox.flushed++; return Promise.resolve(); }
    shutdown() { return Promise.resolve(); }
  };
  return sandbox;
}

function boot(sandbox) {
  return trace.boot({ global: sandbox, sdk: sandbox.sdk, tracing: sandbox.tracing, BatchSpanProcessor: sandbox.BatchSpanProcessor });
}

test('booting twice keeps the first instance and starts Faro once', () => {
  const sandbox = bootSandbox('milesstorm.com');
  const first = boot(sandbox);
  const wrapped = sandbox.fetch;
  assert.equal(boot(sandbox), first);
  assert.equal(sandbox.inits, 1);
  assert.equal(sandbox.fetch, wrapped);
  assert.notEqual(first.tracer, null);
});

test('Faro sends to /faro/collect with an unsampled traceparent and every instrumentation on', () => {
  const sandbox = bootSandbox('milesstorm.com');
  boot(sandbox);
  assert.equal(sandbox.cfg.url, undefined);
  const [transport] = sandbox.cfg.transports;
  assert.equal(transport.opts.url, 'https://milesstorm.com/faro/collect');
  assert.match(transport.opts.requestOptions.headers.traceparent, /^00-[0-9a-f]{32}-[0-9a-f]{16}-00$/);
  assert.deepEqual(sandbox.cfg.instrumentations.map((i) => i.name),
    ['errors', 'web-vitals', 'session', 'view', 'console', 'performance', 'user-actions', 'tracing']);
  assert.equal(sandbox.cfg.app.name, 'milesstorm-web');
  assert.equal(sandbox.cfg.app.environment, 'production');
  assert.equal(sandbox.cfg.trackResources, true);
  assert.equal(sandbox.cfg.webVitalsInstrumentation.trackAttributionSources, true);
  assert.deepEqual(sandbox.cfg.consoleInstrumentation.disabledLevels, ['trace', 'debug', 'log', 'info']);
});

test('the fetch request hook renames Faro fetch spans', () => {
  const sandbox = bootSandbox('milesstorm.com');
  boot(sandbox);
  const hook = sandbox.tracingOpts.instrumentationOptions.fetchInstrumentationOptions.requestHook;
  const span = { name: 'POST', attributes: { 'http.request.method': 'POST', 'url.full': 'https://milesstorm.com/api/session/login?x=1' },
    updateName(n) { this.name = n; } };
  hook(span, {});
  assert.equal(span.name, 'POST /api/session/login');
});

test('fetch spans keep their network timing events', () => {
  const sandbox = bootSandbox('milesstorm.com');
  boot(sandbox);
  assert.equal(sandbox.tracingOpts.instrumentationOptions.fetchInstrumentationOptions.ignoreNetworkEvents, false);
});

test('Faro is not started off milesstorm.com and its staging host', () => {
  for (const host of ['localhost', 'dev.milesstorm.com', 'milesstorm.com.evil.example']) {
    const sandbox = bootSandbox(host);
    const state = boot(sandbox);
    assert.equal(sandbox.inits, 0, host);
    assert.equal(sandbox.tracingOpts, undefined, host);
    assert.equal(state.tracer, null, host);
  }
});

test('pagehide is handled before Faro flushes: open spans end, the user action ends, all is flushed', () => {
  const sandbox = bootSandbox('milesstorm.com');
  const state = boot(sandbox);
  assert.ok(sandbox.order.indexOf('page:pagehide') < sandbox.order.indexOf('transport'), sandbox.order.join());
  let hidden = 0;
  state.tracer.pageHide = () => { hidden++; };
  sandbox.sdk.faro.api.startUserAction('Log In', {}, { triggerName: 'submit' });
  const pagehide = () => sandbox.listeners.pagehide.forEach((fn) => fn({ type: 'pagehide' }));
  pagehide();
  assert.deepEqual([hidden, sandbox.ended, sandbox.flushed], [1, 1, 1]);
  pagehide();
  assert.deepEqual([hidden, sandbox.ended, sandbox.flushed], [2, 1, 2], 'no action left to end');
});

test('frustration goes to Faro under the names of Faro\'s own frustration events', () => {
  const sandbox = bootSandbox('milesstorm.com');
  sandbox.document.addEventListener = (type, fn) => { (sandbox.listeners['doc:' + type] ||= []).push(fn); };
  boot(sandbox);
  const target = el('button', {}, 'Save');
  for (let i = 0; i < 3; i++) sandbox.listeners['doc:click'].forEach((fn) => fn({ target, clientX: 1, clientY: 1 }));
  assert.deepEqual(sandbox.events.filter(([n]) => n.startsWith('faro.frustration')),
    [['faro.frustration.rage_click', { target: 'Save', clickCount: '3' }]]);
});

test('a long animation frame goes to Faro as a measurement, at most 20 per page', () => {
  const m = trace.longFrameMeasurement({
    duration: 180.4, blockingDuration: 120.6,
    scripts: [
      { duration: 20, invoker: 'a', sourceURL: 'https://milesstorm.com/_astro/a.js' },
      { duration: 150, invoker: 'BUTTON#go.onclick', sourceURL: 'https://milesstorm.com/_astro/b.js?v=1', sourceFunctionName: 'go' },
    ],
  });
  assert.deepEqual(JSON.parse(JSON.stringify(m)), {
    measurement: { type: 'long-animation-frame', values: { duration: 180, blocking_duration: 121 } },
    options: { context: { invoker: 'BUTTON#go.onclick', source: 'https://milesstorm.com/_astro/b.js', function: 'go' } },
  });
  assert.deepEqual(trace.longFrameMeasurement({ duration: 60 }).options.context, {});
});

test('beforeSend strips query strings and fragments from page and span URLs', () => {
  const h = setup();
  const item = h.api.scrubItem({
    meta: { page: { url: 'https://milesstorm.com/login?next=/x#top' } },
    payload: { resourceSpans: [{ scopeSpans: [{ spans: [{ attributes: [
      { key: 'http.url', value: { stringValue: 'https://milesstorm.com/api/a?token=1' } },
      { key: 'url.full', value: { stringValue: 'https://milesstorm.com/api/b#frag' } },
    ] }] }] }] },
  });
  assert.equal(item.meta.page.url, 'https://milesstorm.com/login');
  const attrs = item.payload.resourceSpans[0].scopeSpans[0].spans[0].attributes;
  assert.equal(attrs[0].value.stringValue, 'https://milesstorm.com/api/a');
  assert.equal(attrs[1].value.stringValue, 'https://milesstorm.com/api/b');
  // Web vitals attribution and error context: every value that is a URL or a path.
  const ctx = h.api.scrubItem({ payload: { context: {
    url: 'https://milesstorm.com/reset?code=abc', href: '/x#t', resource_url: '/_astro/a.js?v=1', target_element: 'button#go?', count: 3,
  } } });
  assert.deepEqual({ ...ctx.payload.context }, { url: 'https://milesstorm.com/reset', href: '/x', resource_url: '/_astro/a.js', target_element: 'button#go?', count: 3 });
  // Events: resource timings, user actions.
  const event = h.api.scrubItem({ payload: { name: 'faro.performance.resource', attributes: {
    'http.url': 'https://milesstorm.com/api/a?q=1', name: 'https://milesstorm.com/invite#c0de', initiatorType: 'fetch',
  } } });
  assert.deepEqual({ ...event.payload.attributes }, { 'http.url': 'https://milesstorm.com/api/a', name: 'https://milesstorm.com/invite', initiatorType: 'fetch' });
  // console.warn and console.error messages.
  const log = h.api.scrubItem({ payload: { level: 'warn', message: 'GET https://milesstorm.com/verify-email#c0de failed' } });
  assert.equal(log.payload.message, 'GET https://milesstorm.com/verify-email failed');
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
      { filename: 'https://milesstorm.com/_astro/Layout.astro_astro_type_script_index_0_lang.Ab12Cd.js', lineno: 1 },
    ] },
  } });
  assert.equal(item.payload.value, 'failed to load https://milesstorm.com/reset-password (see "https://x.dev/a")');
  assert.equal(item.payload.stacktrace.frames[0].filename, 'https://milesstorm.com/reset-password');
  assert.equal(item.payload.stacktrace.frames[1].filename, 'https://milesstorm.com/_astro/Layout.astro_astro_type_script_index_0_lang.Ab12Cd.js');
  assert.ok(!JSON.stringify(item).includes('s3cret'));
});

// Browser clock 60 ms behind the servers: the Gateway took the request at browser time 1_000_010
// (server 1_000_070) and answered 5 ms later; the browser saw requestStart/responseStart 20 ms apart.
function clockAt(nowMs = 1_000_100, perfNow = 100) {
  const { clockSync } = setup().api;
  return clockSync({ now: () => nowMs, performance: { now: () => perfNow } });
}
const gatewayEntry = (requestStart, responseStart, received, dur, extra = {}) => ({
  entryType: 'resource', initiatorType: 'fetch', name: 'https://milesstorm.com/api/account',
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
  // (transferSize 0) and fetches outside /api/ are never sampled.
  c.addEntry(gatewayEntry(2, 3, 1_000_070 - 86_400_000, 4));
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { transferSize: 0 }));
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { name: 'https://milesstorm.com/_astro/client.Ab12Cd.js' }));
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { initiatorType: 'img' }));
  c.addEntry(gatewayEntry(2, 4, 1_000_070, 5)); // delay -3 ms: not this request's stamp
  assert.equal(c.best(), null);
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { entryType: 'navigation', initiatorType: 'navigation', name: 'https://milesstorm.com/login' }));
  assert.equal(c.best().offset, 60.5);
});

test('clock sync samples the JSON API only', () => {
  const c = clockAt();
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { name: 'https://milesstorm.com/apix/session' }));
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { name: 'https://milesstorm.com/bff/get_account123' }));
  assert.equal(c.best(), null);
  c.addEntry(gatewayEntry(2, 22, 1_000_070, 5, { name: 'https://milesstorm.com/api/session' }));
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
  boot(sandbox);
  const item = sandbox.cfg.beforeSend({ meta: { page: { url: 'https://milesstorm.com/reset?code=abc' } }, payload: {} });
  assert.equal(item.meta.page.url, 'https://milesstorm.com/reset');
});

// ---- Root lifecycle, held-back traces, navigation, camera WebSocket, long tasks ----

const skewEntry = () => gatewayEntry(2, 22, 1_000_070, 5); // any usable sample
const ids = (spans) => spans.map((sp) => sp.name);

test('a click ends after its last child span, not at the fetch promise', async () => {
  const h = setup({ fetchSpans: true });
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/api/a');
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
  await h.win.fetch('/api/a');
  await tick();
  const t0 = h.clk.now();
  h.clk.advance(100);
  h.endFetchSpan(0, t0 + 50); // reported early: 250 ms of the window remain
  h.clk.advance(249);
  assert.equal(h.spans[0].endTime, undefined);
  await h.win.fetch('/api/b');
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
  await h.win.fetch('/api/a');
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
  await h.win.fetch('/api/a');
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

test('a trace started after a sample goes straight to the batch processor', async () => {
  const h = setup({ fetchSpans: true });
  h.clockSync.addEntry(skewEntry());
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/api/a');
  await tick();
  h.endFetchSpan(0, h.clk.now());
  assert.deepEqual(ids(h.exported), ['POST'], 'not held');
});

test('a held trace pushed out by newer ones is sent, not dropped', () => {
  const h = setup();
  const span = (trace, id) => ({ name: trace, spanContext: () => ({ traceId: trace, spanId: id }), endTime: [1, 0] });
  h.processor.onStart(span('old', 'root'));
  h.processor.onEnd(span('old', 'child'));
  assert.equal(h.exported.length, 0, 'held: no clock sample yet');
  for (let i = 0; i < 200; i++) h.processor.onStart(span('t' + i, 'root'));
  assert.deepEqual(h.exported.map((s) => s.name), ['old']);
});

// The real BatchSpanProcessor (the one the bundle has) behind our processor: a span reaches the
// exporter.
test('OTel\'s BatchSpanProcessor exports what the span processor lets through', async () => {
  const { BatchSpanProcessor } = await import('@opentelemetry/sdk-trace-web');
  const sent = [];
  const batch = new BatchSpanProcessor(
    { export: (spans, done) => { sent.push(...spans); done({ code: 0 }); }, shutdown: () => Promise.resolve() },
    { scheduledDelayMillis: 1000, maxExportBatchSize: 30 },
  );
  const clockSync = trace.clockSync({ now: Date.now, performance: { now: () => 0 } });
  const processor = trace.spanProcessor({ next: batch, clock: clockSync });
  const span = { name: 'click', spanContext: () => ({ traceId: 'a'.repeat(32), spanId: 'b'.repeat(16), traceFlags: 1 }), endTime: [1, 0], resource: { asyncAttributesPending: false } };
  processor.onStart(span);
  processor.onEnd(span);
  await processor.forceFlush();
  assert.deepEqual(sent.map((s) => s.name), ['click']);
  await processor.shutdown();
});

test('forceFlush sends held traces uncorrected', () => {
  const h = setup({ meta: REMOTE, fetchSpans: true });
  h.win.fetch('/_astro/client.Ab12Cd.js');
  h.endFetchSpan(0, h.clk.now());
  assert.equal(h.exported.length, 0);
  h.processor.forceFlush();
  assert.deepEqual(ids(h.exported), ['POST']);
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
  boot(sandbox);
  assert.equal(sandbox.inits, 1);
  assert.equal(sandbox.cfg.app.environment, 'staging');
  assert.equal(sandbox.cfg.transports[0].opts.url, 'https://staging.milesstorm.com/faro/collect');
  const chain = sandbox.tracingOpts.spanProcessor;
  assert.equal(typeof chain.inner.onEnd, 'function');
  assert.deepEqual({ ...chain.metas.value }, {});
});

test('the version comes from the page the server rendered', () => {
  const sandbox = bootSandbox('milesstorm.com', { version: '0123abc' });
  boot(sandbox);
  assert.equal(sandbox.cfg.app.version, '0123abc');

  const without = bootSandbox('milesstorm.com');
  boot(without);
  assert.equal(without.cfg.app.version, undefined);
});

test('the environment comes from the host, so one bundle serves staging and production', () => {
  const envs = {};
  for (const host of ['staging.milesstorm.com', 'milesstorm.com']) {
    const sandbox = bootSandbox(host);
    boot(sandbox);
    envs[host] = sandbox.cfg.app.environment;
  }
  assert.deepEqual(envs, { 'staging.milesstorm.com': 'staging', 'milesstorm.com': 'production' });
  // 'production' appears only in the host table, never as a fixed value.
  assert.equal(SRC.match(/'production'/g).length, 1);
});

// ---- Leaving the page, Faro user actions, long animation frames, frustration ----

test('leaving the page ends the click, its open fetch span and the camera handshake, at once', async () => {
  const h = setup({ fetchSpans: true });
  h.click(el('button', { type: 'submit' }, 'Log In'));
  await h.win.fetch('/api/session/login');
  await h.win.fetch('/api/session/after-login');
  h.endFetchSpan(0, h.clk.now());
  const ws = new h.win.WebSocket('wss://milesstorm.com/ws/arcane');
  h.clk.advance(20);
  h.tracer.pageHide();
  const [root, login, afterLogin, camera] = h.spans;
  assert.deepEqual(ids(h.spans), ['click Log In', 'POST', 'POST', 'camera session']);
  assert.equal(root.endTime, 1_000_020);
  assert.equal(afterLogin.endTime, 1_000_020, 'the call answered before the navigation, OTel had not ended it');
  assert.equal(camera.endTime, 1_000_020);
  assert.equal(login.endTime, 1_000_000, 'already ended: left alone');
  assert.equal(afterLogin.parent, root);
  assert.equal(h.tracer.current(), null);
  ws.fire('open');
  h.processor.forceFlush();
  assert.deepEqual(ids(h.exported).sort(), ['POST', 'POST', 'camera session', 'click Log In']);
});

test('leaving the page during the page load ends it with the marks it has', () => {
  const h = setup({ meta: REMOTE });
  h.clk.advance(50);
  h.tracer.pageHide();
  assert.equal(h.spans[0].endTime, 1_000_050);
  assert.ok(h.spans[0].events.some((e) => e.name === 'domInteractive'));
});

test('a click starts the Faro user action named by its label, and the root span and action name each other', async () => {
  const started = [];
  const action = { name: 'Log In', parentId: 'ua1', attributes: {} };
  const userActions = { start: (name, trigger) => started.push([name, trigger]), active: () => action };
  const h = setup({ userActions });
  const button = el('button', { type: 'submit' }, 'Log In');
  h.click(button);
  h.submit(el('form'), button);
  await h.win.fetch('/api/session/login');
  assert.deepEqual(started, [['Log In', 'click'], ['Log In', 'submit']], 'Faro keeps the first: one action at a time');
  const [root] = h.spans;
  assert.equal(root.opts.attributes['faro.action.user.name'], 'Log In');
  assert.equal(root.opts.attributes['faro.action.user.parentId'], 'ua1');
  assert.equal(action.attributes.trace_id, root.ctx.traceId);
});

test('without a running user action the root span carries none, and a failing hook never breaks the click', async () => {
  const userActions = { start: () => { throw new Error('faro'); }, active: () => null };
  const h = setup({ userActions });
  h.click(el('button', {}, 'Go'));
  await h.win.fetch('/api/a');
  assert.deepEqual(Object.keys(h.spans[0].opts.attributes), ['ui.event']);
});

test('long animation frames, where the browser has them, become events and go to Faro', () => {
  const frames = [];
  const observers = [];
  class Observer {
    static supportedEntryTypes = ['longtask', 'long-animation-frame'];
    constructor(fn) { this.fn = fn; }
    observe(opts) { observers.push({ fn: this.fn, opts }); }
  }
  const h = setup({ meta: REMOTE, Observer, onLongFrame: (e) => frames.push(e) });
  const [obs] = observers;
  assert.deepEqual({ ...obs.opts }, { type: 'long-animation-frame', buffered: true });
  const entry = { entryType: 'long-animation-frame', startTime: 25, duration: 90.4, blockingDuration: 40.2 };
  obs.fn({ getEntries: () => [entry] });
  assert.deepEqual(h.spans[0].events.map((e) => [e.name, e.time, e.attrs]),
    [['long-animation-frame', 999_995, { 'loaf.duration_ms': 90, 'loaf.blocking_ms': 40 }]]);
  assert.deepEqual(frames, [entry]);
});

function frustrationPage() {
  const clk = clock();
  const listeners = {};
  const pushed = [];
  let mutated = null;
  const on = (prefix) => (type, fn) => { (listeners[prefix + type] ||= []).push(fn); };
  const fire = (key, e = {}) => (listeners[key] || []).forEach((fn) => fn(e));
  trace.frustration({
    window: { location: { href: 'https://milesstorm.com/profile' }, addEventListener: on('win:') },
    document: { addEventListener: on('') },
    MutationObserver: class { constructor(fn) { mutated = fn; } observe() {} },
    now: clk.now,
    setTimeout: clk.setTimeout,
    push: (kind, target, count) => pushed.push(count ? [kind, target, count] : [kind, target]),
  }).install();
  return {
    clk, pushed,
    click: (target, x = 10, y = 10) => fire('click', { target, clientX: x, clientY: y }),
    mutate: () => mutated(),
    error: () => fire('win:error'),
    rejection: () => fire('win:unhandledrejection'),
    hide: () => fire('win:pagehide'),
  };
}

test('three quick clicks on one spot are a rage click, reported once per burst', () => {
  const p = frustrationPage();
  const save = el('button', {}, 'Save');
  for (let i = 0; i < 5; i++) { p.click(save, 10 + i, 10); p.mutate(); p.clk.advance(200); }
  assert.deepEqual(p.pushed, [['rage', 'Save', 3]]);
  p.clk.advance(2000);
  p.click(save, 10, 10); p.mutate(); p.clk.advance(400);
  p.click(save, 200, 10); p.mutate(); p.clk.advance(400);
  p.click(save, 10, 10); p.mutate();
  assert.deepEqual(p.pushed, [['rage', 'Save', 3]], 'spread out, or too slow: not rage');
});

test('a button click after which nothing changes for 3 s is a dead click; links never are', () => {
  const p = frustrationPage();
  p.click(el('button', {}, 'Refresh'));
  p.clk.advance(2999);
  assert.deepEqual(p.pushed, []);
  p.clk.advance(1);
  assert.deepEqual(p.pushed, [['dead', 'Refresh']]);
  p.click(el('button', {}, 'Open'));
  p.clk.advance(500);
  p.mutate();
  p.click(el('a', { href: '/arcane' }, 'Arcane'), 100, 100);
  p.click(el('button', { 'aria-disabled': 'true' }, 'Wait'), 200, 200);
  p.clk.advance(3000);
  assert.deepEqual(p.pushed, [['dead', 'Refresh']]);
  p.click(el('button', {}, 'Leave'));
  p.hide();
  p.clk.advance(3000);
  assert.deepEqual(p.pushed, [['dead', 'Refresh']], 'the page went away');
});

test('an uncaught error or rejection within 1 s of a click is an error click', () => {
  const p = frustrationPage();
  p.click(el('button', {}, 'Roll'));
  p.mutate();
  p.clk.advance(300);
  p.error();
  p.rejection();
  p.click(el('button', {}, 'Later'));
  p.mutate();
  p.clk.advance(1001);
  p.rejection();
  assert.deepEqual(p.pushed, [['error', 'Roll']]);
});
