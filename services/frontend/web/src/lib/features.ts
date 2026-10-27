// The parts of the site, as cards on the start page.
import type { KnownPermission } from '@/api/session';
import type { Href } from './pages';

export interface Feature {
  title: string;
  text: string;
  to: Href;
  /** Unlocks it; `null`: every logged-in user. */
  permission: KnownPermission | null;
  /** Shown (locked) to people without the permission; unadvertised ones only to those who have it. */
  advertised: boolean;
  /** SVG path, 24x24, stroked. */
  icon: string;
}

export const FEATURES: Feature[] = [
  {
    title: 'Arcane dice reader',
    text: 'Roll real dice in front of your camera and the site reads them for you, live on any device and in the browser extension.',
    to: '/arcane',
    permission: 'arcane',
    advertised: true,
    icon: 'M4 6a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2zM8.5 8.5h.01M15.5 8.5h.01M12 12h.01M8.5 15.5h.01M15.5 15.5h.01',
  },
  {
    title: 'Ark server',
    text: "Start, stop and restart the ARK game server, and see who's playing.",
    to: '/ark',
    permission: 'llama',
    advertised: false,
    icon: 'M5 12h14M5 12a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2v4a2 2 0 0 1-2 2M5 12a2 2 0 0 0-2 2v4a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2v-4a2 2 0 0 0-2-2M7 8h.01M7 16h.01',
  },
  {
    title: 'Enshrouded server',
    text: "See who's on the Chronicle server, and restart or update it when it needs it.",
    to: '/enshrouded',
    permission: 'enshrouded',
    advertised: false,
    icon: 'M12 3v4M12 17v4M3 12h4M17 12h4M6.3 6.3l2.8 2.8M14.9 14.9l2.8 2.8M6.3 17.7l2.8-2.8M14.9 9.1l2.8-2.8M12 9a3 3 0 1 1 0 6 3 3 0 0 1 0-6z',
  },
];

/** Stands in for the unadvertised features on the logged-out page. */
export const MORE: Feature = {
  title: 'More for invited accounts',
  text: 'A few other tools for the people who play with us.',
  to: '/login',
  permission: null,
  advertised: true,
  icon: 'M12 3l1.9 5.8H20l-4.9 3.6 1.9 5.8L12 14.6l-4.9 3.6 1.9-5.8L4 8.8h6.1z',
};

export type CardState = 'preview' | 'open' | 'locked';

/** A logged-in user's cards: open ones first, then locked advertised ones. */
export function homeCards(permissions: readonly KnownPermission[]): { feature: Feature; state: CardState }[] {
  const cards = FEATURES.map((feature) => ({
    feature,
    state: (feature.permission === null || permissions.includes(feature.permission) ? 'open' : 'locked') as CardState,
  })).filter(({ feature, state }) => state === 'open' || feature.advertised);
  return [...cards.filter((c) => c.state === 'open'), ...cards.filter((c) => c.state === 'locked')];
}

/** The logged-out page's previews: the advertised features with a permission, and MORE. */
export function previewCards(): Feature[] {
  return [...FEATURES.filter((f) => f.permission !== null && f.advertised), MORE];
}
