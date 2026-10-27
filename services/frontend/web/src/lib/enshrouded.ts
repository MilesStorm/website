// What the Enshrouded panel says: the server's states, durations, players and recent commands.
// The states and commands are keyed by the Rust enums (through the schema), so one added there
// fails `astro check` until it is worded here.
import type { Schemas } from '@/api/client';
import type { Tone } from '@/components/ServerCards';

export type State = Schemas['EnshroudedState'];
export type Command = Schemas['EnshroudedCommand'];

/** How a state looks (the badge's tone), its name, an explanation, and whether it is under way. */
export const STATES: Record<
  State,
  { tone: Tone; label: string; hint: string; moving: boolean }
> = {
  Online: { tone: 'online', label: 'Online', hint: '', moving: false },
  Starting: { tone: 'pending', label: 'Starting', hint: 'Loading the world. Players can join in a minute or two.', moving: true },
  Restarting: { tone: 'pending', label: 'Restarting', hint: 'Saving the world and starting again.', moving: true },
  Updating: {
    tone: 'pending',
    label: 'Updating',
    hint: "Downloading the latest build from Steam. The server starts by itself when it's done; this can take several minutes.",
    moving: true,
  },
  Stopping: { tone: 'pending', label: 'Stopping', hint: 'Saving the world.', moving: true },
  // Stopped is a normal state, not a failure.
  Stopped: { tone: 'off', label: 'Stopped', hint: 'Restart starts it again.', moving: false },
  Crashed: { tone: 'bad', label: 'Crashed', hint: 'The server kept failing to start. Try Restart or Force update.', moving: false },
  Unreachable: {
    tone: 'bad',
    label: 'Unreachable',
    hint: "Neither the game nor its control port answers. The server's pod may be starting or moving.",
    moving: false,
  },
};

/** States in which a command is already under way: the buttons wait. */
export function busy(state: State): boolean {
  return state === 'Restarting' || state === 'Updating' || state === 'Stopping';
}

/** `secs` as "45 s", "12 min", "3 h 5 min" or "2 d 4 h"; a unit that is zero is left out ("2 d"). */
export function duration(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d > 0) return h > 0 ? `${d} d ${h} h` : `${d} d`;
  if (h > 0) return m > 0 ? `${h} h ${m} min` : `${h} h`;
  if (m > 0) return `${m} min`;
  return `${s} s`;
}

/** How long before `now` the moment `at` was (both unix seconds), as "5 min ago". */
export function ago(now: number, at: number): string {
  return now - at < 60 ? 'just now' : `${duration(now - at)} ago`;
}

/** "1 player is online." / "3 players are online." */
export function playersOnline(n: number): string {
  return n === 1 ? '1 player is online.' : `${n} players are online.`;
}

/** What a recent command did, after its sender's name. A name auth has and this site doesn't
 * know reads as a restart. */
export function actionText(action: Schemas['EnshroudedActionName']): string {
  if (action === 'update') return ' asked for an update';
  if (action === 'update_now') return ' force-updated the server, disconnecting players';
  return ' restarted the server';
}

/** The question before a command. */
export const CONFIRM: Record<Command, { question: string; yes: string }> = {
  restart: {
    question: 'Restart the server? It saves the world, stops, and starts again. Players can rejoin after a minute or two.',
    yes: 'Yes, restart',
  },
  update: {
    question:
      'Force update? The server saves and stops, downloads the latest build from Steam, and starts again. This can take several minutes.',
    yes: 'Yes, update',
  },
  update_now: {
    question:
      'Force update? The server saves and stops, downloads the latest build from Steam, and starts again. This can take several minutes.',
    yes: 'Yes, update',
  },
};

/** What a command that went through says. */
export const SENT: Record<Command, string> = {
  restart: 'Restart sent. The status above follows along.',
  update: 'Update requested. It starts now if nobody is online, otherwise once everyone has left.',
  update_now: 'Update started. The status above follows along.',
};
