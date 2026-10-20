// The CSS that shows or hides parts of a page by who is looking, from the attributes the server
// writes on <html> (`data-session`, `data-perms`). It needs no script, so the first paint is
// right. Without the attributes (a page the server didn't splice) the logged-out parts show.
import type { KnownPermission } from '@/api/session';

// Every permission once: a permission added in Rust fails to compile here until it has a rule.
const PERMISSIONS: Record<KnownPermission, true> = {
  llama: true,
  enshrouded: true,
  arcane: true,
  manage_permissions: true,
};

/** Whether auth's permission `name` is one this site knows (and checks somewhere). */
export function isKnownPermission(name: string): name is KnownPermission {
  return Object.hasOwn(PERMISSIONS, name);
}

export const GATE_CSS = [
  'html:not([data-session="in"]) [data-gate="in"]{display:none!important}',
  'html[data-session="in"] [data-gate="out"]{display:none!important}',
  ...(Object.keys(PERMISSIONS) as KnownPermission[]).map(
    (p) => `html:not([data-perms~="${p}"]) [data-requires="${p}"]{display:none!important}`,
  ),
].join('');
