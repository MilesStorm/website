// Browser half of the end-to-end trace, and what Grafana Faro can't do on its own (TRACING.md,
// "Browser"). assets/main.js imports Faro and calls `boot`; scripts/tracing.mjs bundles both
// into the one classic script the layout loads synchronously in <head>.
//
// Faro exports spans to /faro/collect, its fetch instrumentation creates a CLIENT span per
// request and injects `traceparent`, and its user actions record what a click or submit led to.
// Faro never makes a user action the parent of those spans, so this module:
//   - turns a click or form submit into a ROOT span, started lazily when its first server call
//     happens (clicks that call nothing produce no trace and leave the current span alone), and
//     starts the Faro user action it belongs to;
//   - turns the page load into a span that is a child of the server render span named by
//     <meta name="traceparent">, current until the page has loaded and gone quiet;
//   - wraps window.fetch so each request runs inside the current span's context, which is what
//     Faro's fetch instrumentation reads as the parent;
//   - starts a `camera session` span for the arcane WebSocket, whose context it passes in the URL;
//   - sits between Faro's tracer and exporter (a SpanProcessor), so a click or page load ends
//     after its last child span, and a trace is held back until its clock offset is known;
//   - ends every open span when the page is hidden for a navigation (pagehide), so the next page
//     never leaves this one's spans without their parent;
//   - reports rage, dead and error clicks, and long animation frames.
// Labels come from data-faro-user-action-name, aria-label or button text (buttons whose text is
// dynamic need data-faro-user-action-name), links use their templated path, and input values are
// never read. Faro only starts on milesstorm.com and staging.milesstorm.com, so dev builds send
// nothing.

// Requests that never join an interaction: telemetry (ours, and Cloudflare's /cdn-cgi/rum beacon,
// which Faro's XHR instrumentation would otherwise trace as a root span per page view), the
// WebSocket, the live dice feed.
export var IGNORE = [/\/faro\//, /\/cdn-cgi\//, /\/ws\//, /\/api\/arcane\/rolls/];
var QUIET_MS = 300;
// The islands' first calls can start well after the page's scripts have loaded.
var PAGE_QUIET_MS = 1500;
var MAX_MS = 10000;
var FIRST_FETCH_MS = 1000;
var PAGE_LOAD_MAX_MS = 15000;
var CLICKABLE = 'button, a[href], [role=button], input[type=submit], summary, [data-faro-user-action-name]';
var URL_ATTRS = ['http.url', 'url.full'];
var NAME_ATTR = 'data-faro-user-action-name';

export function isIgnored(url) {
  return IGNORE.some(function (re) { return re.test(url); });
}

// Numeric and id-like path segments become {id} so span names stay low-cardinality.
export function templatePath(path) {
  return path.split('/').map(function (seg) {
    return /^\d+$/.test(seg) || /^[0-9a-f-]{16,}$/i.test(seg) ? '{id}' : seg;
  }).join('/') || '/';
}

// `POST /api/session/login` for Faro's fetch spans.
export function fetchSpanName(method, url) {
  var path;
  try { path = new URL(url).pathname; } catch (e) { return method; }
  return method + ' ' + templatePath(path);
}

// Random non-zero ids with flags 00 (not sampled), made once per page.
export function unsampledTraceparent() {
  function hex(bytes) {
    var b;
    do { b = crypto.getRandomValues(new Uint8Array(bytes)); } while (b.every(function (x) { return x === 0; }));
    return Array.prototype.map.call(b, function (x) { return (x < 16 ? '0' : '') + x.toString(16); }).join('');
  }
  return '00-' + hex(16) + '-' + hex(8) + '-00';
}

function stripUrl(url) {
  return typeof url === 'string' ? url.replace(/[?#].*$/, '') : url;
}

// A value that is a URL or a path (`https://…`, `/reset?code=…`) loses its query and fragment;
// any other value is left alone.
function stripIfUrl(value) {
  return typeof value === 'string' && /^(https?:\/\/|\/)/.test(value) ? stripUrl(value) : value;
}

// URLs quoted inside free text (error and console messages).
function stripQuotedUrls(text) {
  return text.replace(/(https?:\/\/[^\s?#]*)[?#][^\s"')]*/g, '$1');
}

// W3C traceparent `00-<32 hex>-<16 hex>-<2 hex>`; all-zero ids are invalid.
export function parseTraceparent(value) {
  var m = /^00-([0-9a-f]{32})-([0-9a-f]{16})-([0-9a-f]{2})$/.exec(String(value || '').trim());
  if (!m || /^0+$/.test(m[1]) || /^0+$/.test(m[2])) return null;
  return { traceId: m[1], spanId: m[2], traceFlags: parseInt(m[3], 16), isRemote: true };
}

function text(el) {
  return (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 40);
}

// The name of what was clicked or submitted. A form with nothing naming it is labelled by the
// button that submitted it. `href` is the page's address, for links' paths.
export function labelFor(el, submitter, href) {
  var named = el.getAttribute(NAME_ATTR) || el.getAttribute('aria-label');
  if (named) return named.slice(0, 40);
  var tag = el.tagName.toLowerCase();
  if (tag === 'a') return pathOf(el.getAttribute('href'), href);
  if (tag === 'form') {
    var action = el.getAttribute('action');
    return el.getAttribute('name') || el.getAttribute('id') || (action ? pathOf(action, href) : submitter ? labelFor(submitter, null, href) : 'form');
  }
  if (tag === 'input') return (el.getAttribute('value') || el.type || tag).slice(0, 40);
  return text(el) || tag;
}

function pathOf(target, href) {
  try { return templatePath(new URL(target, href).pathname); } catch (e) { return '/'; }
}

// Faro beforeSend: drop query strings and fragments from every URL an item carries (one-time
// codes ride in reset and invite links): the page, span attributes, event attributes (resource
// timings, user actions), measurement context (web vitals attribution), error frames and
// messages, and console messages.
export function scrubItem(item) {
  var page = item && item.meta && item.meta.page;
  if (page && page.url) {
    item = Object.assign({}, item, { meta: Object.assign({}, item.meta, { page: Object.assign({}, page, { url: stripUrl(page.url) }) }) });
  }
  var payload = item && item.payload;
  if (payload && payload.resourceSpans) {
    payload.resourceSpans.forEach(function (rs) {
      (rs.scopeSpans || []).forEach(function (ss) {
        (ss.spans || []).forEach(function (span) {
          (span.attributes || []).forEach(function (attr) {
            if (URL_ATTRS.indexOf(attr.key) >= 0 && attr.value) attr.value.stringValue = stripUrl(attr.value.stringValue);
          });
        });
      });
    });
  }
  ['attributes', 'context'].forEach(function (field) {
    var values = payload && payload[field];
    if (!values || typeof values !== 'object') return;
    Object.keys(values).forEach(function (key) { values[key] = stripIfUrl(values[key]); });
  });
  // Error frames fall back to the page URL as filename; messages may quote absolute URLs.
  if (payload && payload.stacktrace && payload.stacktrace.frames) {
    payload.stacktrace.frames.forEach(function (frame) { frame.filename = stripUrl(frame.filename); });
  }
  if (payload && typeof payload.value === 'string') payload.value = stripQuotedUrls(payload.value);
  if (payload && typeof payload.message === 'string') payload.message = stripQuotedUrls(payload.message);
  return item;
}

// Browser clock correction (NTP's offset estimate; see TRACING.md, "Browser clock"). The device
// clock is often tens of ms (sometimes seconds) off the servers', which draws server spans outside
// the browser span that caused them and leaves Grafana's critical path stuck at the browser span.
// The Gateway stamps each response `Server-Timing: gateway;dur=<ms>;desc="<epoch ms received>"`;
// with the browser's requestStart/responseStart for the same request that gives
//   offset = ((t2 - t1) + (t3 - t4)) / 2, error <= delay / 2, delay = (t4 - t1) - (t3 - t2).
// Only responses no cache can replay count: the page's own navigation and /api/ server
// calls (no-store, never cached by the browser or Cloudflare; a GET the browser cache answers
// has transferSize 0 and is skipped). A replayed stamp would be hours old. HTML
// must therefore stay uncached at Cloudflare (it is today: cf-cache-status DYNAMIC).
// The lowest-delay of the last CLOCK_SAMPLES wins (NTP's clock filter). Each trace keeps the
// estimate it had when its root started (`pin`), so all its spans move together; a trace started
// before any sample takes the first one (the span processor holds its spans back until then).
// beforeSend adds the offset to every browser span and records it on the span, so the raw time
// stays readable.
var CLOCK_SAMPLES = 8;
var CLOCK_MAX_OFFSET_MS = 3600000;
var CLOCK_TRACES = 200;

export function clockSync(deps) {
  var samples = [];
  var byTrace = {};
  var traceOrder = [];
  var listeners = [];

  function usable(entry) {
    if (entry.entryType === 'navigation') return true;
    if (entry.initiatorType !== 'fetch') return false;
    try {
      var path = new URL(entry.name).pathname;
      return path.indexOf('/api/') === 0;
    } catch (e) { return false; }
  }

  // An entry's times are on the monotonic clock; they're moved onto now()'s (the clock spans
  // use) by their distance from performance.now(), not via timeOrigin, which drifts on sleep.
  function addEntry(entry) {
    if (!usable(entry) || entry.transferSize === 0) return;
    var timing = (entry.serverTiming || []).filter(function (t) { return t.name === 'gateway'; })[0];
    var t2 = timing && Number(timing.description);
    if (!t2 || !(entry.requestStart > 0) || !(entry.responseStart >= entry.requestStart)) return;
    var base = deps.now() - deps.performance.now();
    var t1 = base + entry.requestStart;
    var t4 = base + entry.responseStart;
    var t3 = t2 + (Number(timing.duration) || 0);
    var delay = (t4 - t1) - (t3 - t2);
    var offset = ((t2 - t1) + (t3 - t4)) / 2;
    // Negative delay (beyond ms rounding) or an absurd offset means the stamp isn't from this request.
    if (delay < -2 || Math.abs(offset) > CLOCK_MAX_OFFSET_MS) return;
    samples.push({ offset: offset, delay: Math.max(0, delay) });
    if (samples.length > CLOCK_SAMPLES) samples.shift();
    listeners.forEach(function (fn) { fn(); });
  }

  function best() {
    return samples.reduce(function (a, s) { return !a || s.delay < a.delay ? s : a; }, null);
  }

  function keep(traceId, b) {
    byTrace[traceId] = b;
    traceOrder.push(traceId);
    if (traceOrder.length > CLOCK_TRACES) delete byTrace[traceOrder.shift()];
  }

  // Fixes a trace's estimate to the current best. False while there is no sample; with
  // `uncorrected`, fixes it to none instead, so the trace is never half shifted.
  function pin(traceId, uncorrected) {
    if (traceId in byTrace) return true;
    var b = best();
    if (!b && !uncorrected) return false;
    keep(traceId, b || false);
    return true;
  }

  // The estimate a trace was pinned to; a trace not pinned yet is pinned now.
  function forTrace(traceId) {
    pin(traceId, true);
    return byTrace[traceId] || null;
  }

  function shift(nanos, by) {
    if (nanos === undefined || nanos === null || nanos === '') return nanos;
    var v = (BigInt(String(nanos)) + by).toString();
    return typeof nanos === 'number' ? Number(v) : v;
  }

  function correct(span) {
    var b = forTrace(span.traceId);
    if (!b) return;
    var by = BigInt(Math.round(b.offset * 1e6));
    var start = shift(span.startTimeUnixNano, by);
    var end = shift(span.endTimeUnixNano, by);
    var events = (span.events || []).map(function (e) { return shift(e.timeUnixNano, by); });
    span.startTimeUnixNano = start;
    span.endTimeUnixNano = end;
    (span.events || []).forEach(function (e, i) { e.timeUnixNano = events[i]; });
    span.attributes = (span.attributes || []).concat([
      { key: 'browser.clock_offset_ms', value: { doubleValue: Math.round(b.offset * 10) / 10 } },
      { key: 'browser.clock_offset_error_ms', value: { doubleValue: Math.round(b.delay * 5) / 10 } },
    ]);
  }

  // Faro beforeSend: shift trace payloads; other items pass through. Faro drops a whole batch if
  // a hook throws, so a span that can't be corrected is sent as it is.
  function apply(item) {
    var payload = item && item.payload;
    if (typeof BigInt !== 'function' || !payload || !payload.resourceSpans) return item;
    payload.resourceSpans.forEach(function (rs) {
      (rs.scopeSpans || []).forEach(function (ss) {
        (ss.spans || []).forEach(function (span) {
          try { correct(span); } catch (err) { /* left uncorrected */ }
        });
      });
    });
    return item;
  }

  // Requests made before this ran are in the buffer (`buffered`), the page's own document included.
  function observe(Observer) {
    ['navigation', 'resource'].forEach(function (type) {
      try {
        new Observer(function (list) {
          list.getEntries().forEach(function (e) { try { addEntry(e); } catch (err) { /* skip */ } });
        }).observe({ type: type, buffered: true });
      } catch (err) { /* unsupported entry type */ }
    });
  }

  return {
    addEntry: addEntry, apply: apply, observe: observe, best: best, pin: pin,
    listen: function (fn) { listeners.push(fn); },
  };
}

// The OTel SpanProcessor Faro's tracer reports to (TracingInstrumentation `spanProcessor`):
//   - a trace's offset is pinned when its root (its first span on this page) starts. Until a
//     sample exists its ended spans are held, then sent when the first sample arrives, or
//     uncorrected when the root ends (no sample by then means the page has none coming, and
//     holding longer would lose the trace when the tab closes) or the page is hidden;
//   - other spans' starts and ends are reported to `watch`ers, so a click or page load can end
//     after its last child (OTel's fetch span ends after the response body, 300 ms late);
//   - spans still open when the page goes away are ended by `endOpen`: OTel's fetch span for a
//     call that answered just before a navigation would otherwise end on a page that is gone.
// Spans it lets through go to `deps.next`: OTel's BatchSpanProcessor in front of Faro's exporter,
// as in Faro's default chain.

function hrMillis(t) {
  return Array.isArray(t) ? t[0] * 1e3 + t[1] / 1e6 : t;
}

export function spanProcessor(deps) {
  var traces = {};
  var traceOrder = [];
  var watchers = [];
  var open = new Map();

  function queue(spans) {
    spans.forEach(function (span) {
      try { deps.next.onEnd(span); } catch (err) { /* this span is dropped */ }
    });
  }

  function release(t, uncorrected) {
    if (t.fixed || !deps.clock.pin(t.id, uncorrected)) return;
    t.fixed = true;
    queue(t.held);
    t.held = [];
  }

  function notify(kind, span, t) {
    watchers.forEach(function (w) { try { w[kind](t.id, hrMillis(span.endTime)); } catch (err) { /* skip */ } });
  }

  deps.clock.listen(function () {
    Object.keys(traces).forEach(function (id) { release(traces[id]); });
  });

  function flushHeld() {
    Object.keys(traces).forEach(function (id) { release(traces[id], true); });
  }

  return {
    onStart: function (span) {
      var sc = span.spanContext();
      open.set(span, true);
      var t = traces[sc.traceId];
      if (t) return notify('start', span, t);
      t = traces[sc.traceId] = { id: sc.traceId, root: sc.spanId, held: [], fixed: false };
      traceOrder.push(sc.traceId);
      if (traceOrder.length > CLOCK_TRACES) {
        var oldest = traces[traceOrder.shift()];
        if (oldest) release(oldest, true);
        if (oldest) delete traces[oldest.id];
      }
      release(t);
    },
    onEnd: function (span) {
      open.delete(span);
      var sc = span.spanContext();
      var t = traces[sc.traceId];
      if (!t) return queue([span]);
      if (sc.spanId !== t.root) notify('end', span, t);
      if (t.fixed) return queue([span]);
      t.held.push(span);
      if (sc.spanId === t.root) release(t, true);
    },
    // Ends every span still open, at `at`, marked as cut short by the page going away.
    endOpen: function (at) {
      Array.from(open.keys()).forEach(function (span) {
        try {
          if (span.setAttribute) span.setAttribute('browser.ended_by', 'pagehide');
          span.end(at);
        } catch (err) { /* already ended */ }
      });
      open.clear();
    },
    // Everything goes now; held traces uncorrected.
    forceFlush: function () {
      flushHeld();
      return deps.next.forceFlush();
    },
    shutdown: function () {
      flushHeld();
      return deps.next.shutdown();
    },
    watch: function (w) { watchers.push(w); },
  };
}

// Navigation timing marks recorded on the page load span (those after its start).
var NAV_MARKS = ['responseEnd', 'domInteractive', 'domContentLoadedEventStart', 'domContentLoadedEventEnd',
  'domComplete', 'loadEventStart', 'loadEventEnd'];
var CAMERA_PATH = '/ws/arcane';
var LOAF = 'long-animation-frame';

export function create(deps) {
  var otel = deps.otel;
  var win = deps.window;
  var doc = deps.document;
  var perf = deps.performance;
  var now = deps.now;
  var setTimer = deps.setTimeout;
  var clearTimer = deps.clearTimeout;
  var actions = deps.userActions;
  var tracer = otel.trace.getTracer('milesstorm-web');
  // `current` is the span requests join; `pending` is an interaction waiting for its first request.
  var current = null;
  var pending = null;
  var loadedAt = doc.readyState === 'complete' ? now() : 0;

  function label(el, submitter) {
    return labelFor(el, submitter, win.location.href);
  }

  // A performance timestamp (monotonic) on now()'s clock; see pageLoad.
  function wallTime(t) {
    return now() - Math.max(0, perf.now() - t);
  }

  function clear(it) {
    clearTimer(it.quietTimer);
    clearTimer(it.maxTimer);
    clearTimer(it.firstTimer);
  }

  // With nothing in flight, ends at the last settle or child span end (a page load: no earlier
  // than window load); otherwise now.
  function endTime(it) {
    if (it.inflight > 0 || it.children > 0) return now();
    if (it.waitLoad) return loadedAt ? Math.max(loadedAt, it.lastSettle || loadedAt) : now();
    return it.lastSettle || now();
  }

  function navigationMarks(it, end) {
    var nav = perf && perf.getEntriesByType && perf.getEntriesByType('navigation')[0];
    if (!nav) return;
    NAV_MARKS.forEach(function (mark) {
      var at = nav[mark] > 0 && wallTime(nav[mark]);
      if (at && at >= it.startTime && at <= end) it.span.addEvent(mark, at);
    });
  }

  function finish(it) {
    clear(it);
    if (current === it) current = null;
    if (pending === it) pending = null;
    if (!it.span) return;
    var end = endTime(it);
    if (it.waitLoad) {
      try { navigationMarks(it, end); } catch (err) { /* marks are optional */ }
    }
    it.span.end(end);
  }

  // The quiet window runs from the last settle or child span end, not from now: a fetch span
  // reaches the processor 300 ms after it ended (OTel's fetch instrumentation waits for its
  // resource timing entry), and the root must still end after it.
  function maybeQuiet(it) {
    clearTimer(it.quietTimer);
    if (current !== it || it.inflight > 0 || it.children > 0 || (it.waitLoad && !loadedAt)) return;
    var from = Math.max(it.lastSettle || 0, it.waitLoad ? loadedAt : 0) || now();
    var quiet = it.waitLoad ? PAGE_QUIET_MS : QUIET_MS;
    it.quietTimer = setTimer(function () { finish(it); }, Math.max(0, from + quiet - now()));
  }

  // A new interaction replaces a pending one, but only takes over `current` at its first request.
  // It also starts the Faro user action named like it, unless one is running already (Faro runs
  // one at a time, and starts its own on pointerdown for elements with data-faro-user-action-name).
  function interaction(type, name) {
    if (pending) finish(pending);
    var it = pending = { name: type + ' ' + name, type: type, startTime: now(), span: null, inflight: 0, children: 0 };
    it.maxTimer = setTimer(function () { finish(it); }, MAX_MS);
    it.firstTimer = setTimer(function () { if (!it.span) finish(it); }, FIRST_FETCH_MS);
    try { if (actions) actions.start(name, type); } catch (err) { /* never break the page */ }
  }

  function traceIdOf(span) {
    return span && span.spanContext ? span.spanContext().traceId : null;
  }

  // The root span carries the running Faro user action's name and id, as Faro puts them on its
  // fetch spans, and the action's `faro.user.action` event carries the trace id: each finds the
  // other.
  function startPending() {
    var it = pending;
    pending = null;
    clearTimer(it.firstTimer);
    var attributes = { 'ui.event': it.type };
    var action = null;
    try { action = actions ? actions.active() : null; } catch (err) { action = null; }
    if (action) {
      attributes['faro.action.user.name'] = action.name;
      attributes['faro.action.user.parentId'] = action.parentId;
    }
    it.span = tracer.startSpan(it.name, { root: true, startTime: it.startTime, attributes: attributes },
      otel.context.active());
    it.traceId = traceIdOf(it.span);
    if (action && action.attributes) action.attributes.trace_id = it.traceId;
    if (current) finish(current);
    current = it;
  }

  // Child spans of the current interaction, from the span processor.
  var watcher = {
    start: function (traceId) {
      var it = current;
      if (!it || it.traceId !== traceId) return;
      it.children++;
      clearTimer(it.quietTimer);
    },
    end: function (traceId, endMs) {
      var it = current;
      if (!it || it.traceId !== traceId || it.children <= 0) return;
      it.children--;
      if (endMs > 0) it.lastSettle = Math.max(it.lastSettle || 0, Math.min(endMs, now()));
      maybeQuiet(it);
    },
  };

  function onClick(e) {
    try {
      var el = e.target && e.target.closest ? e.target.closest(CLICKABLE) : null;
      if (el) interaction('click', label(el));
    } catch (err) { /* never break the page */ }
  }

  function onSubmit(e) {
    try {
      if (e.target && e.target.tagName) interaction('submit', label(e.target, e.submitter));
    } catch (err) { /* never break the page */ }
  }

  function pageLoad() {
    var meta = doc.querySelector('meta[name="traceparent"]');
    var parent = meta && parseTraceparent(meta.getAttribute('content'));
    if (!parent) return;
    var nav = perf && perf.getEntriesByType && perf.getEntriesByType('navigation')[0];
    // Elapsed time from the monotonic clock, anchored to now(): timeOrigin can lag the wall clock
    // by days after the machine sleeps.
    var startTime = nav && nav.responseStart && perf.now ? wallTime(nav.responseStart) : now();
    var it = current = { name: 'page load ' + templatePath(win.location.pathname), waitLoad: true, startTime: startTime, inflight: 0, children: 0 };
    // The page load outlives the server render it's a child of (OTel's document-load pattern),
    // so it is marked as following it rather than contained in it (TRACING.md, "Page loads").
    it.span = tracer.startSpan(it.name, { startTime: startTime, attributes: { 'ui.event': 'load', 'trace.relation': 'follows' } },
      otel.trace.setSpanContext(otel.context.active(), parent));
    it.traceId = traceIdOf(it.span);
    it.maxTimer = setTimer(function () { finish(it); }, PAGE_LOAD_MAX_MS);
    maybeQuiet(it);
  }

  // Long animation frames (Chromium; long tasks where they are missing) become events on the
  // open click, navigation or page load span: they label its own time, which no child span
  // covers. Each long animation frame also goes to `deps.onLongFrame`.
  function onLongTasks(list) {
    list.getEntries().forEach(function (e) {
      var it = current;
      var at = wallTime(e.startTime);
      if (e.entryType === LOAF && deps.onLongFrame) {
        try { deps.onLongFrame(e); } catch (err) { /* never break the page */ }
      }
      if (!it || !it.span || at < it.startTime) return;
      if (e.entryType === LOAF) {
        it.span.addEvent(LOAF, { 'loaf.duration_ms': Math.round(e.duration), 'loaf.blocking_ms': Math.round(e.blockingDuration || 0) }, at);
      } else {
        it.span.addEvent('longtask', { 'longtask.duration_ms': Math.round(e.duration) }, at);
      }
    });
  }

  function urlOf(input) {
    var raw = typeof input === 'string' ? input : input && (input.href || input.url);
    return new URL(String(raw), win.location.href).href;
  }

  function wrapFetch(inner) {
    return function fetch(input, init) {
      var self = this === undefined ? win : this;
      var it, ctx;
      try {
        if ((pending || current) && !isIgnored(urlOf(input))) {
          if (pending) startPending();
          it = current;
          ctx = otel.trace.setSpan(otel.context.active(), it.span);
        }
      } catch (err) { ctx = null; }
      if (!ctx) return inner.call(self, input, init);

      var settle = function () {
        try {
          it.inflight--;
          it.lastSettle = now();
          maybeQuiet(it);
        } catch (err) { /* never break the page */ }
      };
      it.inflight++;
      clearTimer(it.quietTimer);
      var result;
      try {
        result = otel.context.with(ctx, function () { return inner.call(self, input, init); });
      } catch (err) {
        settle();
        throw err;
      }
      try {
        Promise.resolve(result).then(settle, settle);
      } catch (err) { settle(); }
      return result;
    };
  }

  // The arcane camera WebSocket gets a `camera session` span: a child of the click that opened
  // it, else (no click, or only the page load) a root. It ends at `open` (the handshake); the session's work is in its own traces.
  // Browsers can't set WebSocket headers, so its context goes in the URL (`traceparent` query
  // parameter), which the frontend links its session span to.
  function cameraSpan(url) {
    if (new URL(String(url), win.location.href).pathname !== CAMERA_PATH) return null;
    if (pending) startPending();
    var it = current && !current.waitLoad ? current : null;
    var parent = it ? otel.trace.setSpan(otel.context.active(), it.span) : otel.context.active();
    var span = tracer.startSpan('camera session', { root: !it, startTime: now(), attributes: { 'url.path': CAMERA_PATH } }, parent);
    var sc = span.spanContext();
    var flags = (sc.traceFlags & 0xff).toString(16);
    var tp = '00-' + sc.traceId + '-' + sc.spanId + '-' + (flags.length < 2 ? '0' : '') + flags;
    return { span: span, url: String(url) + (String(url).indexOf('?') >= 0 ? '&' : '?') + 'traceparent=' + tp };
  }

  function wrapWebSocket(Inner) {
    function WebSocket(url, protocols) {
      var camera = null;
      try { camera = cameraSpan(url); } catch (err) { camera = null; }
      var target = camera ? camera.url : url;
      var ws = arguments.length > 1 ? new Inner(target, protocols) : new Inner(target);
      if (!camera) return ws;
      var done = function (e) {
        try {
          ws.removeEventListener('open', done);
          ws.removeEventListener('error', done);
          ws.removeEventListener('close', done);
          if (e.type !== 'open') camera.span.setStatus({ code: 2, message: 'WebSocket ' + e.type + ' before open' });
          camera.span.end(now());
        } catch (err) { /* never break the page */ }
      };
      ws.addEventListener('open', done);
      ws.addEventListener('error', done);
      ws.addEventListener('close', done);
      return ws;
    }
    WebSocket.prototype = Inner.prototype;
    ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED'].forEach(function (k) { WebSocket[k] = Inner[k]; });
    return WebSocket;
  }

  // The page is going away (a navigation, a closed tab) or into the back/forward cache: whatever
  // is open ends now, so the spans are exported while the page can still send them. The login
  // form's submit, its two calls and the navigation that follows are one trace this way.
  function pageHide() {
    if (pending) finish(pending);
    if (current) finish(current);
    if (deps.spans && deps.spans.endOpen) deps.spans.endOpen(now());
  }

  function install() {
    doc.addEventListener('click', onClick, true);
    doc.addEventListener('submit', onSubmit, true);
    win.addEventListener('load', function () {
      loadedAt = now();
      if (current) maybeQuiet(current);
    });
    win.fetch = wrapFetch(win.fetch);
    if (typeof win.WebSocket === 'function') win.WebSocket = wrapWebSocket(win.WebSocket);
    if (deps.spans) deps.spans.watch(watcher);
    var Observer = deps.PerformanceObserver;
    if (Observer) {
      var type = (Observer.supportedEntryTypes || []).indexOf(LOAF) >= 0 ? LOAF : 'longtask';
      try { new Observer(onLongTasks).observe({ type: type, buffered: true }); } catch (err) { /* unsupported */ }
    }
    pageLoad();
  }

  return {
    install: install,
    pageHide: pageHide,
    labelFor: label,
    wrapFetch: wrapFetch,
    current: function () { return current; },
    pending: function () { return pending; },
  };
}

// Frustration signals (Faro 2.12 has none; these use the event names and attributes of Faro's
// own, unreleased, frustration instrumentation, so the dashboards keep working when it ships):
// `faro.frustration.rage_click`, `faro.frustration.dead_click` and `faro.frustration.error_click`,
// with `target` (the label a trace would give the element) and, for rage, `clickCount`:
//   - rage: RAGE_CLICKS clicks within RAGE_MS and RAGE_PX of each other, once per burst;
//   - dead: a click on a button (not a link: a slow page load isn't the button's fault) after
//     which the page doesn't change at all for DEAD_MS;
//   - error: an uncaught error or rejection within ERROR_MS of a click.
var RAGE_CLICKS = 3;
var RAGE_MS = 1000;
var RAGE_PX = 30;
var DEAD_MS = 3000;
var ERROR_MS = 1000;

export function frustration(deps) {
  var now = deps.now;
  var clicks = [];
  var changedAt = 0;
  var hidden = false;
  var lastClick = null;

  function onClick(e) {
    try {
      var el = e.target && e.target.closest ? e.target.closest(CLICKABLE) : null;
      if (!el) return;
      var target = labelFor(el, null, deps.window.location.href);
      var at = now();
      lastClick = { at: at, target: target };
      clicks = clicks.filter(function (c) {
        return at - c.at <= RAGE_MS && Math.abs(c.x - e.clientX) <= RAGE_PX && Math.abs(c.y - e.clientY) <= RAGE_PX;
      });
      clicks.push({ at: at, x: e.clientX, y: e.clientY });
      if (clicks.length === RAGE_CLICKS) deps.push('rage', target, RAGE_CLICKS);
      var disabled = el.disabled || el.getAttribute('aria-disabled') === 'true';
      if (el.tagName.toLowerCase() !== 'a' && !disabled) {
        deps.setTimeout(function () {
          if (!hidden && changedAt < at) deps.push('dead', target);
        }, DEAD_MS);
      }
    } catch (err) { /* never break the page */ }
  }

  function onError() {
    try {
      if (lastClick && now() - lastClick.at <= ERROR_MS) {
        deps.push('error', lastClick.target);
        lastClick = null;
      }
    } catch (err) { /* never break the page */ }
  }

  return {
    install: function () {
      deps.document.addEventListener('click', onClick, true);
      deps.window.addEventListener('error', onError);
      deps.window.addEventListener('unhandledrejection', onError);
      deps.window.addEventListener('pagehide', function () { hidden = true; });
      deps.window.addEventListener('pageshow', function () { hidden = false; });
      if (deps.MutationObserver) {
        new deps.MutationObserver(function () { changedAt = now(); })
          .observe(deps.document, { subtree: true, childList: true, attributes: true, characterData: true });
      }
    },
  };
}

// At most this many long animation frames per page go to Faro as measurements.
var LONG_FRAMES_PER_PAGE = 20;

// A long animation frame as a Faro measurement: how long, how long it blocked input, and the
// script that ran longest in it (its URL without query string).
export function longFrameMeasurement(entry) {
  var script = (entry.scripts || []).reduce(function (a, s) { return !a || s.duration > a.duration ? s : a; }, null);
  var context = {};
  if (script) {
    context.invoker = String(script.invoker || '').slice(0, 100);
    context.source = stripUrl(script.sourceURL || '');
    context.function = String(script.sourceFunctionName || '').slice(0, 100);
  }
  return {
    measurement: {
      type: LOAF,
      values: { duration: Math.round(entry.duration), blocking_duration: Math.round(entry.blockingDuration || 0) },
    },
    options: { context: context },
  };
}

// Hosts that send telemetry, and the deployment environment their spans are tagged with.
var ENVIRONMENTS = { 'milesstorm.com': 'production', 'staging.milesstorm.com': 'staging' };

// Starts Faro and everything above, once per page. `deps.sdk` and `deps.tracing` are the Faro
// exports (assets/main.js); `deps.sdk.faro` is the live Faro instance once initializeFaro has
// run. `deps.global` is the page's global object.
export function boot(deps) {
  var g = deps.global;
  if (g.__msTrace) return g.__msTrace;
  var state = g.__msTrace = { tracer: null };
  try {
    var sdk = deps.sdk;
    var tracing = deps.tracing;
    var document = g.document;
    var location = g.location;
    var environment = location ? ENVIRONMENTS[location.hostname] : undefined;
    if (!document || !environment) return state;
    var Observer = typeof g.PerformanceObserver === 'function' && g.performance ? g.PerformanceObserver : null;
    var clock = clockSync({ now: Date.now, performance: g.performance });
    if (Observer) clock.observe(Observer);
    // Faro's own chain (session and user attributes, batch, export) with ours before the batch.
    var spans = spanProcessor({
      next: new deps.BatchSpanProcessor(new tracing.FaroTraceExporter({ get api() { return sdk.faro.api; } }), {
        scheduledDelayMillis: 1000,
        maxExportBatchSize: 30,
      }),
      clock: clock,
    });
    // Registered before Faro's own listeners (its transport's pagehide, its batch's
    // visibilitychange), so open, held and batched spans reach Faro's transport before it
    // flushes for a page that is going away. The user action ends too: Faro holds the errors and
    // logs of a running action until it ends.
    g.addEventListener('pagehide', function () {
      try {
        if (state.tracer) state.tracer.pageHide();
        var action = sdk.faro.api.getActiveUserAction();
        if (action) action.end();
      } catch (err) { /* never break the page */ }
      spans.forceFlush();
    });
    document.addEventListener('visibilitychange', function () {
      if (document.visibilityState === 'hidden') spans.forceFlush();
    });
    // The build the page was served by, which is also this bundle's: `service.version` (the
    // commit), which also names the source maps' release.
    var versionMeta = document.querySelector('meta[name="service-version"]');
    var version = (versionMeta && versionMeta.getAttribute('content')) || undefined;
    var LogLevel = sdk.LogLevel;
    var faro = sdk.initializeFaro({
      // Beacons carry an unsampled traceparent so the Gateway does not trace each one.
      transports: [new sdk.FetchTransport({
        url: location.origin + '/faro/collect',
        requestOptions: { headers: { traceparent: unsampledTraceparent() } },
      })],
      app: { name: 'milesstorm-web', namespace: 'milesstorm', environment: environment, version: version },
      // Every page's scripts, fonts and styles (/_astro/), not only fetches; beforeSend strips
      // their query strings like every other URL.
      trackResources: true,
      webVitalsInstrumentation: { trackAttributionSources: true },
      // console.warn and console.error; uncaught errors and rejections come from the errors
      // instrumentation.
      consoleInstrumentation: { disabledLevels: [LogLevel.TRACE, LogLevel.DEBUG, LogLevel.LOG, LogLevel.INFO] },
      // No CSP or navigation instrumentation: every navigation is a page load here.
      instrumentations: [
        new sdk.ErrorsInstrumentation(),
        new sdk.WebVitalsInstrumentation(),
        new sdk.SessionInstrumentation(),
        new sdk.ViewInstrumentation(),
        new sdk.ConsoleInstrumentation(),
        new sdk.PerformanceInstrumentation(),
        new sdk.UserActionInstrumentation(),
        new tracing.TracingInstrumentation({
          spanProcessor: new tracing.FaroMetaAttributesSpanProcessor(spans, { get value() { return sdk.faro.metas.value; } }),
          instrumentationOptions: {
            fetchInstrumentationOptions: {
              // Faro turns these off. OTel's fetch span events (fetchStart, DNS, connect, TLS,
              // requestStart, responseStart, responseEnd) are timestamps only: no URL or query.
              ignoreNetworkEvents: false,
              // The request is a Request or a fetch init without a url, so read the span's attributes.
              requestHook: function (span) {
                var attrs = span.attributes || {};
                span.updateName(fetchSpanName(attrs['http.request.method'] || span.name, attrs['url.full'] || attrs['http.url']));
              },
            },
          },
        }),
      ],
      ignoreUrls: IGNORE,
      beforeSend: function (item) {
        item = scrubItem(item);
        try { return clock.apply(item); } catch (err) { return item; }
      },
    });
    var otel = faro && faro.api.getOTEL();
    if (!otel) return state;
    var api = faro.api;
    var longFrames = 0;
    state.tracer = create({
      otel: otel,
      window: g,
      document: document,
      performance: g.performance,
      PerformanceObserver: Observer,
      spans: spans,
      now: Date.now,
      setTimeout: g.setTimeout.bind(g),
      clearTimeout: g.clearTimeout.bind(g),
      userActions: {
        start: function (name, trigger) {
          if (!api.getActiveUserAction()) api.startUserAction(name, {}, { triggerName: trigger });
        },
        active: function () {
          var action = api.getActiveUserAction();
          return action && action.getState() === sdk.UserActionState.Started ? action : null;
        },
      },
      onLongFrame: function (entry) {
        if (++longFrames > LONG_FRAMES_PER_PAGE) return;
        var m = longFrameMeasurement(entry);
        api.pushMeasurement(m.measurement, m.options);
      },
    });
    state.tracer.install();
    frustration({
      window: g,
      document: document,
      MutationObserver: g.MutationObserver,
      now: Date.now,
      setTimeout: g.setTimeout.bind(g),
      push: function (kind, target, count) {
        var attributes = { target: target };
        if (count) attributes.clickCount = String(count);
        api.pushEvent('faro.frustration.' + kind + '_click', attributes);
      },
    }).install();
  } catch (err) { /* tracing must never break the page */ }
  return state;
}
