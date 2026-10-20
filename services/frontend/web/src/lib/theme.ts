// Light, dark, or the system's (the default). The inline script in the layout applies it before
// the page paints; the toggle changes it.
export const THEME_KEY = 'ms.theme';
export const CONSENT_KEY = 'ms.cookie-consent';
/** The Dioxus app's keys, removed on the first visit (a one-time reset of theme and consent). */
export const OLD_KEYS = ['theme', 'accepted_cookies', 'showing_cookies'];

export type Theme = 'light' | 'dark' | 'system';

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

/** `dark` on <html> unless the theme resolves to light. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement): void {
  const dark = theme === 'dark' || (theme === 'system' && prefersDark());
  root.classList.toggle('dark', dark);
}

export function getConsent(): string | null {
  return storage()?.getItem(CONSENT_KEY) ?? null;
}

export function setConsent(value: 'accepted' | 'dismissed'): void {
  storage()?.setItem(CONSENT_KEY, value);
}
