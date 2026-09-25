// The one site the extension talks to. HTTPS only: the login cookie is Secure and
// nothing should travel in plain text.
export const BASE = "https://milesstorm.com";

/**
 * Host-permission pattern for a site. No port: Firefox ignores patterns that
 * include one, so the permission would silently never apply.
 */
export function originPattern(base) {
  const u = new URL(base);
  return `${u.protocol}//${u.hostname}/*`;
}
