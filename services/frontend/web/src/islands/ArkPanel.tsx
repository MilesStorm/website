import { useCallback, useEffect, useId, useState } from 'react';
import { CircleCheck } from 'lucide-react';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { FormAlert } from '@/components/FormAlert';
import { StatNote, StatusBadge } from '@/components/ServerCards';
import { client, type Schemas } from '@/api/client';
import { attempt, type Outcome } from '@/lib/errors';
import { ARK_BUTTONS, ArkHeader, PlayersCard } from '@/islands/ArkSkeleton';
import { PAGE_STACK } from '@/lib/layout';
import { island } from '@/lib/island';

type Command = Schemas['ArkCommand'];

/** How often the player count is asked again while the page is shown. */
export const POLL_MS = 30_000;

/** What each outcome of a command says. Keyed by the Rust enum: an outcome added there fails
 * `astro check` until it is worded here. */
export const OUTCOMES: Record<Schemas['ArkOutcome'], string> = {
  stopped: 'Stopped.',
  already_stopped: 'It was already stopped.',
  failed_to_stop: "It didn't stop.",
  started: 'Started.',
  already_running: 'It was already running.',
  failed_to_start: "It didn't start.",
  timeout: 'The game server took too long to answer.',
  restarting: 'Restarting.',
};

/** What the count says: offline when the server doesn't give one. */
export function players(count: number | null): { value: string; note: string } {
  if (count === null) return { value: 'Offline', note: 'The game server is not running.' };
  if (count === 0) return { value: '0', note: 'Server running, no players' };
  return { value: String(count), note: 'Server is active' };
}

/** The Ark server: who is online (asked again every 30 seconds while the page is in view, when
 * the tab is shown again, and after a command), and start, stop and restart. */
export const ArkPanel = island('ArkPanel', function ArkPanel() {
  const [count, setCount] = useState<Outcome<Schemas['ArkPlayers']> | null>(null);
  const [running, setRunning] = useState<Command | null>(null);
  const [said, setSaid] = useState<{ cmd: Command; outcome: Outcome<Schemas['ArkResult']> } | null>(null);

  const refresh = useCallback(async () => setCount(await attempt(client.GET('/api/ark/players'))), []);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (document.visibilityState === 'visible') void refresh();
    }, POLL_MS);
    // Back to the tab: the count may be minutes old, so ask now.
    const shown = () => {
      if (document.visibilityState === 'visible') void refresh();
    };
    document.addEventListener('visibilitychange', shown);
    return () => {
      clearInterval(timer);
      document.removeEventListener('visibilitychange', shown);
    };
  }, [refresh]);

  const run = async (cmd: Command) => {
    setRunning(cmd);
    const outcome = await attempt(client.POST('/api/ark/command', { body: { cmd } }));
    setSaid({ cmd, outcome });
    setRunning(null);
    await refresh();
  };

  // The count when it came (null: the server isn't running), and what it says.
  const n = count?.ok ? (count.data.count ?? null) : undefined;
  const shown = n === undefined ? null : players(n);
  const online = n !== undefined && n !== null;
  const stopNote = useId();
  return (
    <div className={PAGE_STACK}>
      <ArkHeader
        badge={shown && <StatusBadge tone={online ? 'online' : 'off'} label={online ? 'Online' : shown.value} />}
        buttons={ARK_BUTTONS.map(([cmd, label], i) => (
          <Button
            key={cmd}
            variant={i === 0 ? 'default' : 'outline'}
            disabled={running !== null}
            onClick={() => void run(cmd)}
            // Stop has no confirmation: its consequence is read out with the button.
            aria-describedby={cmd === 'stop' ? stopNote : undefined}
            data-faro-user-action-name={`ark ${cmd}`}
          >
            {running === cmd ? `${label}…` : label}
          </Button>
        ))}
      />
      <p id={stopNote} hidden>
        Shuts the game server down and disconnects its players.
      </p>
      {/* What came of the last command, read out as it comes (the region is there from the start). */}
      <div aria-live="polite" className="empty:hidden">
        {said &&
          (said.outcome.ok ? (
            <Alert role="status">
              <CircleCheck aria-hidden="true" />
              <AlertDescription>{OUTCOMES[said.outcome.data.outcome]}</AlertDescription>
            </Alert>
          ) : (
            <FormAlert message={said.outcome.message} />
          ))}
      </div>
      <PlayersCard
        loading={count === null}
        live
        // Offline (the badge says so) or not known: a dash.
        value={online ? shown!.value : <span aria-hidden="true">—</span>}
        note={shown ? <StatNote>{shown.note}</StatNote> : <FormAlert message={count?.ok ? null : count?.message} />}
      />
    </div>
  );
});
