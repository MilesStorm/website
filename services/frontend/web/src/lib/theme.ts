// Light, dark, or the system's (the default); motion likewise. The inline script in the layout
// applies both before the page paints; the toggles change them.
export const THEME_KEY = 'ms.theme';
export const MOTION_KEY = 'ms.motion';
export const CONSENT_KEY = 'ms.cookie-consent';
/** The Dioxus app's keys, removed on the first visit (a one-time reset of theme and consent). */
export const OLD_KEYS = ['theme', 'accepted_cookies', 'showing_cookies'];

export type Theme = 'light' | 'dark' | 'system';
export type Motion = 'reduced' | 'full' | 'system';

function storage(): Storage | undefined {
  try {
    return localStorage;
  } catch {
    return undefined;
  }
}

export function getTheme(): Theme {
  const saved = storage()?.getItem(THEME_KEY);
  return saved === 'light' || saved === 'dark' ? saved : 'system';
}

export function prefersDark(): boolean {
  return window.matchMedia?.('(prefers-color-scheme: dark)').matches ?? true;
}

export function setTheme(theme: Theme): void {
  storage()?.setItem(THEME_KEY, theme);
  applyTheme(theme);
}

/** `setTheme`, with the new theme spreading in a circle from `from` (the toggle) where the browser
 * has view transitions and motion is on. */
export function switchTheme(theme: Theme, from?: Element | null): void {
  const dark = theme === 'dark' || (theme === 'system' && prefersDark());
  const unchanged = dark === document.documentElement.classList.contains('dark');
  if (unchanged || !document.startViewTransition || !from || !motionOn()) {
    setTheme(theme);
    return;
  }
  const transition = document.startViewTransition(() => setTheme(theme));
  const { left, top, width, height } = from.getBoundingClientRect();
  const x = left + width / 2;
  const y = top + height / 2;
  const r = Math.hypot(Math.max(x, innerWidth - x), Math.max(y, innerHeight - y));
  void transition.ready.then(() =>
    document.documentElement.animate(
      { clipPath: [`circle(0 at ${x}px ${y}px)`, `circle(${r}px at ${x}px ${y}px)`] },
      { duration: 500, easing: 'cubic-bezier(0.4, 0, 0.2, 1)', pseudoElement: '::view-transition-new(root)' },
    ),
  );
}

/** `dark` on <html> unless the theme resolves to light. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement): void {
  const dark = theme === 'dark' || (theme === 'system' && prefersDark());
  root.classList.toggle('dark', dark);
}

export function getMotion(): Motion {
  const saved = storage()?.getItem(MOTION_KEY);
  return saved === 'reduced' || saved === 'full' ? saved : 'system';
}

export function setMotion(motion: Motion): void {
  storage()?.setItem(MOTION_KEY, motion);
  applyMotion(motion);
}

/** `data-motion` on <html> for a choice other than the system's. */
export function applyMotion(motion: Motion, root: HTMLElement = document.documentElement): void {
  if (motion === 'system') root.removeAttribute('data-motion');
  else root.setAttribute('data-motion', motion);
}

/** Whether animation is welcome: the choice on <html>, else the system's. */
export function motionOn(root: HTMLElement = document.documentElement): boolean {
  const chosen = root.getAttribute('data-motion');
  if (chosen) return chosen === 'full';
  return !(window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ?? false);
}

export function getConsent(): string | null {
  return storage()?.getItem(CONSENT_KEY) ?? null;
}

export function setConsent(value: 'accepted' | 'dismissed'): void {
  storage()?.setItem(CONSENT_KEY, value);
}
