// The browser telemetry's entry point: Grafana Faro's modules and trace.js, which
// scripts/tracing.mjs bundles (tree-shaken, minified, with a source map) into the one classic
// script the layout loads synchronously in <head> (TRACING.md, "Browser").
import {
  ConsoleInstrumentation,
  ErrorsInstrumentation,
  FetchTransport,
  LogLevel,
  PerformanceInstrumentation,
  SessionInstrumentation,
  UserActionInstrumentation,
  UserActionState,
  ViewInstrumentation,
  WebVitalsInstrumentation,
  faro,
  initializeFaro,
} from '@grafana/faro-web-sdk';
import { FaroMetaAttributesSpanProcessor, FaroTraceExporter, TracingInstrumentation } from '@grafana/faro-web-tracing';
// Faro's default chain uses OTel's BatchSpanProcessor, and so does ours: the same package Faro
// depends on (keep its version on Faro's).
import { BatchSpanProcessor } from '@opentelemetry/sdk-trace-web';
import { boot } from './trace.js';

boot({
  global: globalThis,
  sdk: {
    ConsoleInstrumentation,
    ErrorsInstrumentation,
    FetchTransport,
    LogLevel,
    PerformanceInstrumentation,
    SessionInstrumentation,
    UserActionInstrumentation,
    UserActionState,
    ViewInstrumentation,
    WebVitalsInstrumentation,
    initializeFaro,
    // A live binding: the instance initializeFaro registers.
    get faro() {
      return faro;
    },
  },
  tracing: { FaroMetaAttributesSpanProcessor, FaroTraceExporter, TracingInstrumentation },
  BatchSpanProcessor,
});
