// The logging-in forms' and the profile's fixed text, shared with their placeholders
// (islands/FormSkeleton.tsx, islands/ProfileSkeleton.tsx) so the two can't drift apart in size.
// Free of the API client: the placeholders are rendered when the pages are built.

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

// The profile (islands/Profile.tsx).
export const ACCOUNT_INTRO = 'How you appear on this site.';
export const PICTURE_HINT = 'JPEG, PNG, WebP or GIF, up to 5 MB.';
/** Around the username, in bold. */
export const NAME_HINT = {
  before: 'Shown on the site instead of your username. Leave it empty to use your username,',
  after: ", which you log in with and which can't be changed.",
};
export const SHARING_TITLE = 'Help improve dice recognition';
export const SHARING_INTRO = 'Share pictures of your rolls so the dice reader gets better at reading them.';
export const SHARING_FACTS: [string, string][] = [
  ["What's saved", 'The camera picture and what the model read.'],
  ['Which rolls', 'Rolls the model was unsure about, and about 1 in 20 others.'],
  ['Who sees them', 'Only the site owner, to check the numbers. Used only to train the dice reader.'],
];
export const SHARING_NOTE =
  "Turning this off stops new saves. Rolls you flag as wrong in the browser extension are saved either way; your latest roll's picture is kept for 10 minutes so it can be flagged.";
export const SHARING_DELETE = 'Delete my shared pictures';
export const DELETE_INTRO =
  "Removes your account and profile picture. Any data you chose to share will also be removed. This can't be undone.";
export const DELETE_OPEN = 'Delete my account…';

/** Opening a deletion link (islands/DeleteAccount.tsx): around the account's username, in bold. */
export const DELETE_LINK = {
  before: 'This permanently deletes the account',
  after: ": your profile and profile picture. Any data you chose to share will also be removed. It can't be undone.",
};
