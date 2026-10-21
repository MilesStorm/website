import { useCallback, useEffect, useState } from 'react';
import { CircleCheck, Info, LoaderCircle } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { FormAlert } from '@/components/FormAlert';
import { client, type Schemas } from '@/api/client';
import { attempt } from '@/lib/errors';
import { CONFIRM, SENT, STATES, actionText, ago, busy, duration, playersOnline, type Command } from '@/lib/enshrouded';

type Status = Schemas['EnshroudedStatus'];

/** How often the status is asked again while the page is shown. */
export const POLL_MS = 10_000;

/** A command waiting for its confirmation, being sent, or the answer to the last one. */
type CommandUi =
  | { kind: 'idle' }
  | { kind: 'confirming'; cmd: Command }
  | { kind: 'sending'; cmd: Command }
  | { kind: 'sent'; cmd: Command }
  | { kind: 'failed'; message: string };

const unixNow = () => Math.floor(Date.now() / 1000);

/** The Enshrouded server: its state, an update on Steam, who is on, Restart and Force update,
 * and the latest commands. Asked again every 10 seconds while the tab is visible, when it is
 * shown again, and after a command; the countdowns tick every second. */
export function EnshroudedPanel() {
  // The last status the server gave, and why the latest ask failed (shown above it): a failed
  // poll keeps the panel, and an open confirmation, where they were.
  const [status, setStatus] = useState<Status | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [now, setNow] = useState(unixNow);
  const [command, setCommand] = useState<CommandUi>({ kind: 'idle' });

  const refresh = useCallback(async () => {
    const asked = await attempt(client.GET('/api/enshrouded'));
    if (asked.ok) setStatus(asked.data);
    setFailed(asked.ok ? null : asked.message);
  }, []);
  useEffect(() => {
    void refresh();
    const tick = setInterval(() => setNow(unixNow()), 1000);
    const poll = setInterval(() => {
      if (document.visibilityState === 'visible') void refresh();
    }, POLL_MS);
    const shown = () => {
      if (document.visibilityState === 'visible') void refresh();
    };
    document.addEventListener('visibilitychange', shown);
    return () => {
      clearInterval(tick);
      clearInterval(poll);
      document.removeEventListener('visibilitychange', shown);
    };
  }, [refresh]);

  const send = async (cmd: Command) => {
    setCommand({ kind: 'sending', cmd });
    const done = await attempt(client.POST('/api/enshrouded/command', { body: { cmd } }));
    setCommand(done.ok ? { kind: 'sent', cmd } : { kind: 'failed', message: done.message });
    await refresh();
  };

  if (!status) {
    if (failed) return <FormAlert message={failed} />;
    return <Skeleton className="h-96 w-full rounded-xl" role="status" aria-label="Loading the server's status" />;
  }
  const s = status;
  return (
    <div className="flex flex-col gap-6">
      <FormAlert message={failed} />
      <Header status={s} />
      <UpdateNotice status={s} now={now} />
      <Players status={s} />
      <Controls
        status={s}
        now={now}
        command={command}
        ask={(cmd) => setCommand({ kind: 'confirming', cmd })}
        confirm={(cmd) => void send(cmd)}
        cancel={() => setCommand({ kind: 'idle' })}
      />
      <Activity recent={s.recent} now={now} />
    </div>
  );
}

function Header({ status }: { status: Status }) {
  const state = STATES[status.state];
  return (
    <Card>
      <CardHeader>
        <CardDescription className="font-semibold tracking-widest uppercase">Enshrouded</CardDescription>
        <CardTitle>
          <h1 className="text-3xl font-extrabold">Chronicle</h1>
        </CardTitle>
        <CardAction>
          <Badge variant={state.badge}>
            {state.moving && <LoaderCircle aria-hidden="true" className="motion-safe:animate-spin" />}
            {state.label}
          </Badge>
        </CardAction>
      </CardHeader>
      {(state.hint || status.uptime_secs !== null || status.version !== null) && (
        <CardContent className="flex flex-col gap-3 text-sm">
          {state.hint && <p className="text-muted-foreground">{state.hint}</p>}
          <p className="flex flex-wrap gap-x-6 gap-y-1 text-muted-foreground">
            {status.uptime_secs !== null && <span>Up for {duration(status.uptime_secs)}</span>}
            {status.version !== null && <span>Build {status.version}</span>}
          </p>
        </CardContent>
      )}
    </Card>
  );
}

/** A newer build on Steam than the running server, or one queued for when it is empty. */
function UpdateNotice({ status, now }: { status: Status; now: number }) {
  if (status.update_queued && status.state === 'Online') {
    return (
      <Alert role="status">
        <Info aria-hidden="true" />
        <AlertTitle>An update is queued.</AlertTitle>
        <AlertDescription>
          It installs once everyone has left the server (checked every 30 minutes). Force update → "Update now anyway"
          installs it right away.
        </AlertDescription>
      </Alert>
    );
  }
  if (status.latest_build_at === null) return null;
  if (!status.update_pending) {
    return (
      <p className="px-1 text-sm text-muted-foreground">
        Up to date: Steam's latest build is from {ago(now, status.latest_build_at)}.
      </p>
    );
  }
  return (
    <Alert>
      <Info aria-hidden="true" />
      <AlertTitle>A new build came out {ago(now, status.latest_build_at)}.</AlertTitle>
      <AlertDescription>
        The server installs it by itself when nobody is online (it checks every 30 minutes). Force update installs it now.
      </AlertDescription>
    </Alert>
  );
}

function Players({ status }: { status: Status }) {
  const p = status.players;
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Players online</h2>
        </CardTitle>
      </CardHeader>
      <CardContent>
        {p === null ? (
          <p className="text-2xl font-bold text-muted-foreground">
            <span aria-hidden="true">—</span>
            <span className="sr-only">Unknown</span>
          </p>
        ) : (
          <>
            <p className="text-4xl font-extrabold tabular-nums">
              {p.online}
              <span className="text-xl text-muted-foreground"> / {p.max}</span>
            </p>
            {p.sessions.length > 0 && (
              <>
                <ul className="mt-3 flex flex-col gap-1 text-sm">
                  {p.sessions.map((secs, i) => (
                    <li key={i} className="flex justify-between gap-4">
                      <span>Player {i + 1}</span>
                      <span className="tabular-nums">on for {duration(secs)}</span>
                    </li>
                  ))}
                </ul>
                <p className="mt-2 text-xs text-muted-foreground">Enshrouded doesn't share player names.</p>
              </>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}

function Controls({
  status,
  now,
  command,
  ask,
  confirm,
  cancel,
}: {
  status: Status;
  now: number;
  command: CommandUi;
  ask: (cmd: Command) => void;
  confirm: (cmd: Command) => void;
  cancel: () => void;
}) {
  const working = busy(status.state);
  const wait = status.next_command_at !== null && status.next_command_at > now ? status.next_command_at - now : null;
  const sending = command.kind === 'sending';
  const disabled = !status.can_control || working || wait !== null || sending;
  const online = status.players?.online ?? 0;
  const sendingAny = (...cmds: Command[]) => command.kind === 'sending' && cmds.includes(command.cmd);

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Server controls</h2>
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <div className="flex flex-wrap gap-3">
          <Button disabled={disabled} onClick={() => ask('restart')}>
            {sendingAny('restart') && <LoaderCircle aria-hidden="true" className="motion-safe:animate-spin" />}
            Restart
          </Button>
          <Button variant="outline" disabled={disabled} onClick={() => ask('update')}>
            {sendingAny('update', 'update_now') && <LoaderCircle aria-hidden="true" className="motion-safe:animate-spin" />}
            Force update
          </Button>
        </div>
        {!status.can_control ? (
          <p className="text-sm text-muted-foreground">The website can't reach the server's controls right now.</p>
        ) : wait !== null ? (
          <p className="text-sm text-muted-foreground">Someone just sent a command: the buttons come back in {wait} s.</p>
        ) : working ? (
          <p className="text-sm text-muted-foreground">Wait for the current restart or update to finish.</p>
        ) : null}
        <div aria-live="polite">
          <Answer command={command} online={online} confirm={confirm} cancel={cancel} />
        </div>
      </CardContent>
    </Card>
  );
}

/** The confirmation of a command, or what came of it. */
function Answer({
  command,
  online,
  confirm,
  cancel,
}: {
  command: CommandUi;
  online: number;
  confirm: (cmd: Command) => void;
  cancel: () => void;
}) {
  switch (command.kind) {
    case 'idle':
    case 'sending':
      return null;
    case 'failed':
      return <FormAlert message={command.message} />;
    case 'sent':
      return (
        <Alert role="status">
          <CircleCheck aria-hidden="true" />
          <AlertDescription>{SENT[command.cmd]}</AlertDescription>
        </Alert>
      );
    case 'confirming':
      // With players online, Force update waits for them unless asked not to.
      if (command.cmd === 'update' && online > 0) {
        return (
          <Alert>
            <AlertDescription className="flex flex-col gap-3 text-foreground">
              <p>
                {playersOnline(online)} Force update waits until everyone has left, then installs the latest build (the
                server checks every 30 minutes).
              </p>
              <p>"Update now anyway" saves and stops the server right away, disconnecting them. Updating takes several minutes.</p>
              <div className="flex flex-wrap gap-2">
                <Button size="sm" onClick={() => confirm('update')}>
                  Update when empty
                </Button>
                <Button size="sm" variant="destructive" onClick={() => confirm('update_now')}>
                  Update now anyway
                </Button>
                <Button size="sm" variant="ghost" onClick={cancel}>
                  Cancel
                </Button>
              </div>
            </AlertDescription>
          </Alert>
        );
      }
      return (
        <Alert>
          <AlertDescription className="flex flex-col gap-3 text-foreground">
            <p>{CONFIRM[command.cmd].question}</p>
            {online > 0 && <p className="font-semibold">{playersOnline(online)} They will be disconnected.</p>}
            <div className="flex gap-2">
              <Button size="sm" onClick={() => confirm(command.cmd)}>
                {CONFIRM[command.cmd].yes}
              </Button>
              <Button size="sm" variant="ghost" onClick={cancel}>
                Cancel
              </Button>
            </div>
          </AlertDescription>
        </Alert>
      );
  }
}

/** Who restarted or updated the server lately. */
function Activity({ recent, now }: { recent: Schemas['EnshroudedAction'][]; now: number }) {
  if (recent.length === 0) return null;
  return (
    <Card>
      <CardHeader>
        <CardTitle>
          <h2>Recent commands</h2>
        </CardTitle>
      </CardHeader>
      <CardContent>
        <ul className="flex flex-col gap-1 text-sm">
          {recent.map((a, i) => (
            <li key={i} className="flex justify-between gap-4">
              <span>
                <span className="font-semibold">{a.name ?? 'A deleted account'}</span>
                {actionText(a.action)}
              </span>
              <span className="whitespace-nowrap text-muted-foreground">{ago(now, a.at)}</span>
            </li>
          ))}
        </ul>
      </CardContent>
    </Card>
  );
}
