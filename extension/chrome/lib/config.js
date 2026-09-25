// Sites the extension may talk to. Must match manifest.json (test/config.test.js
// checks this): Chrome only sends the login cookie to hosts the extension has
// permission for, and restricting the option keeps a changed setting from
// pointing the panel at some other server.
export const ALLOWED_BASES = ["https://milesstorm.com", "http://localhost:8080"];

export const DEFAULT_BASE = ALLOWED_BASES[0];

/** Host-permission pattern for a base URL. */
export const originPattern = (base) => `${base}/*`;

/** Everything but the default is an optional permission, granted from the options page. */
export const isOptional = (base) => base !== DEFAULT_BASE;

export function normalizeBase(value) {
  return ALLOWED_BASES.includes(value) ? value : DEFAULT_BASE;
}
