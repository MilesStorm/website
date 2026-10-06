// Browser half of the end-to-end trace (see TRACING.md, "Browser").
//
// Loaded synchronously in <head> after the vendored Grafana Faro bundles. Faro exports spans to
// /faro/collect and its fetch instrumentation creates a CLIENT span per request and injects
// `traceparent`. Faro alone never makes a user action the parent of those spans, so this script:
//   - turns a click or form submit into a ROOT span, started lazily when its first server call
//     happens (clicks that call nothing produce no trace and leave the current span alone);
//   - turns the page load into a span that is a child of the server render span named by
//     <meta name="traceparent">, current until the page has loaded and gone quiet;
//   - wraps window.fetch so each request runs inside the current span's context, which is what
//     Faro's fetch instrumentation reads as the parent.
// Labels come from data-trace-name, aria-label or button text (buttons whose text is dynamic need
// data-trace-name), links use their templated path, and input values are never read.
// Faro only starts on milesstorm.com, so dev builds send nothing.
(function () {
  'use strict';
  if (globalThis.__msTrace) return;

  // Requests that never join an interaction: telemetry, the WebSocket, the live dice feed.
  var IGNORE = [/\/faro\//, /\/ws\//, /\/api\/arcane\/rolls/];
  var QUIET_MS = 300;
  // Hydration-time server calls can start well after the WASM fetch settles.
  var PAGE_QUIET_MS = 1500;
  var MAX_MS = 10000;
  var FIRST_FETCH_MS = 1000;
  var PAGE_LOAD_MAX_MS = 15000;
  var CLICKABLE = 'button, a[href], [role=button], input[type=submit], summary, [data-trace-name]';
  var URL_ATTRS = ['http.url', 'url.full'];
  var URL_FIELDS = ['url', 'href', 'page_url'];

  function isIgnored(url) {
    return IGNORE.some(function (re) { return re.test(url); });
  }

  // Numeric and id-like path segments become {id} so span names stay low-cardinality.
  function templatePath(path) {
    return path.split('/').map(function (seg) {
      return /^\d+$/.test(seg) || /^[0-9a-f-]{16,}$/i.test(seg) ? '{id}' : seg;
    }).join('/') || '/';
  }

  // `POST /bff/login_password` for Faro's fetch spans: Dioxus appends a numeric hash to server
  // function paths (/bff/login_password1003...), which is dropped.
  function fetchSpanName(method, url) {
    var path;
    try { path = new URL(url).pathname; } catch (e) { return method; }
    if (path.indexOf('/bff/') === 0) path = path.replace(/([^\/\d])\d{6,}$/, '$1');
    return method + ' ' + templatePath(path);
  }

  // Random non-zero ids with flags 00 (not sampled), made once per page.
  function unsampledTraceparent() {
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

  // W3C traceparent `00-<32 hex>-<16 hex>-<2 hex>`; all-zero ids are invalid.
  function parseTraceparent(value) {
    var m = /^00-([0-9a-f]{32})-([0-9a-f]{16})-([0-9a-f]{2})$/.exec(String(value || '').trim());
    if (!m || /^0+$/.test(m[1]) || /^0+$/.test(m[2])) return null;
    return { traceId: m[1], spanId: m[2], traceFlags: parseInt(m[3], 16), isRemote: true };
  }

  function text(el) {
    return (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 40);
  }

  // Faro beforeSend: drop query strings and fragments from page and request URLs.
  function scrubItem(item) {
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
    if (payload && payload.attributes) {
      URL_ATTRS.forEach(function (key) {
        if (key in payload.attributes) payload.attributes[key] = stripUrl(payload.attributes[key]);
      });
    }
    // Error frames fall back to the page URL as filename; messages may quote absolute URLs.
    if (payload && payload.stacktrace && payload.stacktrace.frames) {
      payload.stacktrace.frames.forEach(function (frame) { frame.filename = stripUrl(frame.filename); });
    }
    if (payload && typeof payload.value === 'string') {
      payload.value = payload.value.replace(/(https?:\/\/[^\s?#]*)[?#][^\s"')]*/g, '$1');
    }
    // Errors and web vitals carry page URLs in their context.
    if (payload && payload.context) {
      URL_FIELDS.forEach(function (key) {
        if (key in payload.context) payload.context[key] = stripUrl(payload.context[key]);
      });
    }
    return item;
  }

  function create(deps) {
    var otel = deps.otel;
    var win = deps.window;
    var doc = deps.document;
    var now = deps.now;
    var setTimer = deps.setTimeout;
    var clearTimer = deps.clearTimeout;
    var tracer = otel.trace.getTracer('milesstorm-web');
    // `current` is the span requests join; `pending` is an interaction waiting for its first request.
    var current = null;
    var pending = null;
    var loadedAt = doc.readyState === 'complete' ? now() : 0;

    function pathOf(href) {
      try { return templatePath(new URL(href, win.location.href).pathname); } catch (e) { return '/'; }
    }

    // A form with nothing naming it is labelled by the button that submitted it.
    function labelFor(el, submitter) {
      var named = el.getAttribute('data-trace-name') || el.getAttribute('aria-label');
      if (named) return named.slice(0, 40);
      var tag = el.tagName.toLowerCase();
      if (tag === 'a') return pathOf(el.getAttribute('href'));
      if (tag === 'form') {
        var action = el.getAttribute('action');
        return el.getAttribute('name') || el.getAttribute('id') || (action ? pathOf(action) : submitter ? labelFor(submitter) : 'form');
      }
      if (tag === 'input') return (el.getAttribute('value') || el.type || tag).slice(0, 40);
      return text(el) || tag;
    }

    function clear(it) {
      clearTimer(it.quietTimer);
      clearTimer(it.maxTimer);
      clearTimer(it.firstTimer);
    }

    // With nothing in flight, ends at the last settle (a page load: no earlier than window load);
    // otherwise now.
    function endTime(it) {
      if (it.inflight > 0) return now();
      if (it.waitLoad) return loadedAt ? Math.max(loadedAt, it.lastSettle || loadedAt) : now();
      return it.lastSettle || now();
    }

    function finish(it) {
      clear(it);
      if (current === it) current = null;
      if (pending === it) pending = null;
      if (it.span) it.span.end(endTime(it));
    }

    function maybeQuiet(it) {
      clearTimer(it.quietTimer);
      if (current !== it || it.inflight > 0 || (it.waitLoad && !loadedAt)) return;
      it.quietTimer = setTimer(function () { finish(it); }, it.waitLoad ? PAGE_QUIET_MS : QUIET_MS);
    }

    // A new interaction replaces a pending one, but only takes over `current` at its first request.
    function interaction(type, el, submitter) {
      if (pending) finish(pending);
      var it = pending = { name: type + ' ' + labelFor(el, submitter), type: type, startTime: now(), span: null, inflight: 0 };
      it.maxTimer = setTimer(function () { finish(it); }, MAX_MS);
      it.firstTimer = setTimer(function () { if (!it.span) finish(it); }, FIRST_FETCH_MS);
    }

    function startPending() {
      var it = pending;
      pending = null;
      clearTimer(it.firstTimer);
      it.span = tracer.startSpan(it.name, { root: true, startTime: it.startTime, attributes: { 'ui.event': it.type } },
        otel.context.active());
      if (current) finish(current);
      current = it;
    }

    function onClick(e) {
      try {
        var el = e.target && e.target.closest ? e.target.closest(CLICKABLE) : null;
        if (el) interaction('click', el);
      } catch (err) { /* never break the page */ }
    }

    function onSubmit(e) {
      try {
        if (e.target && e.target.tagName) interaction('submit', e.target, e.submitter);
      } catch (err) { /* never break the page */ }
    }

    function pageLoad() {
      var meta = doc.querySelector('meta[name="traceparent"]');
      var parent = meta && parseTraceparent(meta.getAttribute('content'));
      if (!parent) return;
      var perf = deps.performance;
      var nav = perf && perf.getEntriesByType && perf.getEntriesByType('navigation')[0];
      var startTime = nav && perf.timeOrigin && nav.responseStart ? perf.timeOrigin + nav.responseStart : now();
      var it = current = { name: 'page load ' + templatePath(win.location.pathname), waitLoad: true, inflight: 0 };
      it.span = tracer.startSpan(it.name, { startTime: startTime, attributes: { 'ui.event': 'load' } },
        otel.trace.setSpanContext(otel.context.active(), parent));
      it.maxTimer = setTimer(function () { finish(it); }, PAGE_LOAD_MAX_MS);
      maybeQuiet(it);
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

    function install() {
      doc.addEventListener('click', onClick, true);
      doc.addEventListener('submit', onSubmit, true);
      win.addEventListener('load', function () {
        loadedAt = now();
        if (current) maybeQuiet(current);
      });
      win.fetch = wrapFetch(win.fetch);
      pageLoad();
    }

    return {
      install: install,
      labelFor: labelFor,
      wrapFetch: wrapFetch,
      current: function () { return current; },
      pending: function () { return pending; },
    };
  }

  globalThis.__msTrace = {
    create: create,
    templatePath: templatePath,
    fetchSpanName: fetchSpanName,
    unsampledTraceparent: unsampledTraceparent,
    parseTraceparent: parseTraceparent,
    scrubItem: scrubItem,
    isIgnored: isIgnored,
  };

  try {
    var sdk = globalThis.GrafanaFaroWebSdk;
    var tracing = globalThis.GrafanaFaroWebTracing;
    if (!sdk || !tracing || typeof document === 'undefined' || location.hostname !== 'milesstorm.com') return;
    var faro = sdk.initializeFaro({
      // Beacons carry an unsampled traceparent so the Gateway does not trace each one.
      transports: [new sdk.FetchTransport({
        url: location.origin + '/faro/collect',
        requestOptions: { headers: { traceparent: unsampledTraceparent() } },
      })],
      app: { name: 'milesstorm-web', namespace: 'milesstorm', environment: 'production' },
      // No performance, user action, navigation, CSP or console instrumentation: volume, query
      // strings (reset and verify codes) in resource URLs, and extra fetch/history patching.
      instrumentations: [
        new sdk.ErrorsInstrumentation(),
        new sdk.WebVitalsInstrumentation(),
        new sdk.SessionInstrumentation(),
        new sdk.ViewInstrumentation(),
        new tracing.TracingInstrumentation({
          instrumentationOptions: {
            fetchInstrumentationOptions: {
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
      beforeSend: scrubItem,
    });
    var otel = faro && faro.api.getOTEL();
    if (!otel) return;
    create({
      otel: otel,
      window: window,
      document: document,
      performance: globalThis.performance,
      now: Date.now,
      setTimeout: setTimeout.bind(globalThis),
      clearTimeout: clearTimeout.bind(globalThis),
    }).install();
  } catch (err) { /* tracing must never break the page */ }
})();
