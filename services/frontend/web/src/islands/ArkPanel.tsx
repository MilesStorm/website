import { useCallback, useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { FormAlert } from '@/components/FormAlert';
import { client, type Schemas } from '@/api/client';
import { attempt, type Outcome } from '@/lib/errors';

type Command = Schemas['ArkCommand'];

/** How often the player count is asked again while the page is shown. */
export const POLL_MS = 30_000;

/** The buttons, in Dioxus's order. */
const COMMANDS: [Command, string][] = [
  ['restart', 'Restart'],
  ['stop', 'Stop'],
  ['start', 'Start'],
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

/** The Ark server: who is online (asked again every 30 seconds while the page is in view, and
 * after a command), and start, stop and restart. */
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
    return () => clearInterval(timer);
  }, [refresh]);

  const run = async (cmd: Command) => {
    setRunning(cmd);
    const outcome = await attempt(client.POST('/api/ark/command', { body: { cmd } }));
    setSaid({ cmd, outcome });
    setRunning(null);
    await refresh();
  };

  const shown = count?.ok ? players(count.data.count ?? null) : null;
  return (
    <div className="flex flex-col gap-6">
      <Card>
        <CardHeader>
          <CardTitle>
            <h2>Players online</h2>
          </CardTitle>
          <CardDescription aria-live="polite">{shown?.note ?? ' '}</CardDescription>
        </CardHeader>
        <CardContent>
          {count === null ? (
            <Skeleton className="h-10 w-24" aria-label="Loading the player count" />
          ) : shown ? (
            <p className="text-4xl font-bold tabular-nums">{shown.value}</p>
          ) : (
            <FormAlert message={count.ok ? null : count.message} />
          )}
        </CardContent>
      </Card>
      <div className="flex flex-wrap justify-center gap-3">
        {COMMANDS.map(([cmd, label]) => (
          <Button key={cmd} size="lg" disabled={running !== null} onClick={() => void run(cmd)}>
            {running === cmd ? `${label}…` : label}
          </Button>
        ))}
      </div>
      <div aria-live="polite">
        {said &&
          (said.outcome.ok ? (
            <p className="text-center text-sm">{OUTCOMES[said.outcome.data.outcome]}</p>
          ) : (
            <FormAlert message={said.outcome.message} />
          ))}
      </div>
    </div>
  );
}
