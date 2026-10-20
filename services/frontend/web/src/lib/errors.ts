// What a failed call says to the user. The server's `ApiError.message` is written for the user
// and wins; these cover an error without one, and a request that never got an answer. Both
// records are keyed by the Rust enums (through the schema), so a code added there fails
// `astro check` until it has a message here.
import type { Schemas } from '@/api/client';

export type ErrorCode = Schemas['ErrorCode'];
export type OAuthError = Schemas['OAuthError'];

const BY_CODE: Record<ErrorCode, string> = {
  logged_out: "You've been logged out. Log in again.",
  forbidden: "You don't have access to this.",
  invalid: "That isn't valid. Check it and try again.",
  conflict: 'That is already taken.',
  not_found: "That doesn't exist (any more).",
  gone: 'That link has expired.',
  rate_limited: 'Too many tries. Wait a moment and try again.',
  unavailable: "That can't be done right now. Try again later.",
  upstream: "Something we depend on isn't answering. Try again later.",
  internal: 'Something went wrong. Try again later.',
};

/** When the request itself failed (offline, the server unreachable). */
export const NO_ANSWER = "The site can't be reached right now. Check your connection and try again.";

/** The message for a failed call: the server's, else its code's, else that there was no answer
 * (no reply, or one that isn't the error envelope, such as a proxy's page). */
export function errorMessage(error: unknown): string {
  if (typeof error !== 'object' || error === null || !('error' in error)) return NO_ANSWER;
  const e = error as Schemas['ApiError'];
  return e.message || BY_CODE[e.error] || NO_ANSWER;
}

/** A finished call: its data, or the message to show (and the error's code, when it had one). */
export type Outcome<T> = { ok: true; data: T } | { ok: false; message: string; code: ErrorCode | null };

/** Waits for a call of the typed client; a call that throws (no answer) is a failure too. */
export async function attempt<T>(
  call: Promise<{ data?: T; error?: unknown; response: Response }>,
): Promise<Outcome<T>> {
  try {
    const { data, error, response } = await call;
    if (response.ok) return { ok: true, data: data as T };
    const code = typeof error === 'object' && error !== null && 'error' in error ? (error as Schemas['ApiError']).error : null;
    return { ok: false, message: errorMessage(error), code };
  } catch {
    return { ok: false, message: NO_ANSWER, code: null };
  }
}

/** Why an OAuth login came back to the login page (`?error=`). */
export const OAUTH_ERRORS: Record<OAuthError, string> = {
  unknown_provider: "That login provider isn't supported.",
  session_failed: "Your login couldn't be saved. Try again.",
  start_failed: "The login provider couldn't be reached. Try again later.",
  missing_code: "The login provider didn't finish the login. Try again.",
  missing_state: "The login provider didn't finish the login. Try again.",
  csrf_mismatch: 'That login link was stale or not from this site. Start the login again.',
  email_exists: 'An account with this email already exists. Log in with its password instead.',
  exchange_failed: "The login provider's answer couldn't be checked. Try again.",
};

export function oauthError(code: string | null): string | null {
  if (!code) return null;
  return code in OAUTH_ERRORS ? OAUTH_ERRORS[code as OAuthError] : "That login didn't work. Try again.";
}
