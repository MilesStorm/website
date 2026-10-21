// The logging-in forms' fixed text, shared with their placeholders (islands/FormSkeleton.tsx) so
// the two can't drift apart in size. Free of the API client: the placeholders are rendered when
// the pages are built.

/** Auth's own minimum password length (services/auth); the server checks it too. */
export const MIN_PASSWORD = 8;

export const REGISTER_PASSWORD_HINT = `At least ${MIN_PASSWORD} characters. We'll email you a link to confirm your address.`;
export const NEW_PASSWORD_HINT = `At least ${MIN_PASSWORD} characters.`;
export const FORGOT_LINK = 'Forgot your password?';
export const FORGOT_INTRO = "Enter your username or email, and we'll email you a link to choose a new password.";

/** A line under a form's button: text, then a link. */
export interface Note {
  text: string;
  link: string;
  end?: string;
}
export const NO_ACCOUNT: Note = { text: "Don't have an account?", link: 'Sign up' };
export const HAVE_ACCOUNT: Note = { text: 'Already have an account?', link: 'Log in' };
export const NO_PASSWORD: Note = {
  text: 'Log in with GitHub or Google? Then you have no password here: use that button on the',
  link: 'log in page',
  end: '.',
};
