// Every island is wrapped in a Faro error boundary: an island that throws while rendering shows
// a short message in its place instead of an empty box, and the error goes to Faro with the
// island's name (`context.island`). The page's Faro is started by the tracing script in <head>
// (TRACING.md, "Browser"), not by this bundle, so the boundary's React integration is added to
// that instance (`window.faro`). Without one (`astro dev`, the component tests) the boundary
// reports to this bundle's own Faro, which isn't started and drops it.
import type { ComponentType } from 'react';
import { FaroErrorBoundary, ReactIntegration, faro } from '@grafana/faro-react';
import { FormAlert } from '@/components/FormAlert';

export const BROKEN = "This part of the page couldn't be shown. Reload the page to try again.";

let connected = false;

function connect() {
  if (connected) return;
  connected = true;
  const integration = new ReactIntegration();
  const page = (globalThis as { faro?: typeof faro }).faro;
  if (page?.instrumentations) {
    page.instrumentations.add(integration);
  } else {
    integration.api = faro.api;
    integration.initialize();
  }
}

/** `Component` as an island named `name` (minified builds rename functions). */
export function island<P extends object>(name: string, Component: ComponentType<P>): ComponentType<P> {
  function Island(props: P) {
    connect();
    return (
      <FaroErrorBoundary fallback={<FormAlert message={BROKEN} />} pushErrorOptions={{ context: { island: name } }}>
        <Component {...props} />
      </FaroErrorBoundary>
    );
  }
  Island.displayName = name;
  return Island;
}
