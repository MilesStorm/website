import { useCallback, useEffect, useState } from 'react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { FormAlert } from '@/components/FormAlert';
import { ServerControls, StatCard } from '@/components/ServerCards';
import { client, type Schemas } from '@/api/client';
import { attempt, type Outcome } from '@/lib/errors';
import { ARK_BUTTONS, PlayersPlace } from '@/islands/ArkSkeleton';

type Command = Schemas['ArkCommand'];

/** How often the player count is asked again while the page is shown. */
export const POLL_MS = 30_000;

/** The buttons, in Dioxus's order (ARK_BUTTONS, which the placeholder shows). */
const COMMANDS: [Command, string][] = [
  ['restart', ARK_BUTTONS[0]],
  ['stop', ARK_BUTTONS[1]],
  ['start', ARK_BUTTONS[2]],
];

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
export function ArkPanel() {
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

  const shown = count?.ok ? players(count.data.count ?? null) : null;
  const online = count?.ok === true && count.data.count !== null;
  return (
    <div className="flex flex-col gap-6">
      {count === null ? (
        <PlayersPlace />
      ) : (
        <StatCard
          label="Players online"
          heading
          // Offline (the badge says so) or not known: a dash.
          value={online ? shown!.value : <span aria-hidden="true">—</span>}
          action={shown && <Badge variant={online ? 'default' : 'outline'}>{online ? 'Online' : shown.value}</Badge>}
          footer={
            shown ? (
              <p className="text-muted-foreground" aria-live="polite">
                {shown.note}
              </p>
            ) : (
              <FormAlert message={count.ok ? null : count.message} />
            )
          }
        />
      )}
      <ServerControls
        buttons={COMMANDS.map(([cmd, label], i) => (
          <Button key={cmd} variant={i === 0 ? 'default' : 'outline'} disabled={running !== null} onClick={() => void run(cmd)}>
            {running === cmd ? `${label}…` : label}
          </Button>
        ))}
      >
        <div aria-live="polite">
          {said &&
            (said.outcome.ok ? (
              <p className="text-sm">{OUTCOMES[said.outcome.data.outcome]}</p>
            ) : (
              <FormAlert message={said.outcome.message} />
            ))}
        </div>
      </ServerControls>
    </div>
  );
}
