import { useCallback, useEffect, useState } from 'react';
import { CircleCheck, History, Info, LoaderCircle, Users, type LucideIcon } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { FormAlert } from '@/components/FormAlert';
import { Meter, StatCard, StatNote, StatusBadge } from '@/components/ServerCards';
import { client, type Schemas } from '@/api/client';
import { attempt } from '@/lib/errors';
import { CONFIRM, SENT, STATES, actionText, ago, busy, duration, playersOnline, type Command } from '@/lib/enshrouded';
import { ENSHROUDED_BUTTONS, EnshroudedHeader, EnshroudedSkeleton, Figures, UPTIME_NOTE, UPTIME_NOTE_SHORT } from '@/islands/EnshroudedSkeleton';
import { island } from '@/lib/island';
import { PAGE_GAP, PAGE_STACK } from '@/lib/layout';

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
export const EnshroudedPanel = island('EnshroudedPanel', function EnshroudedPanel() {
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
    if (!failed) return <EnshroudedSkeleton />;
    return (
      <div className={PAGE_STACK}>
        <EnshroudedHeader />
        <FormAlert message={failed} />
      </div>
    );
  }
  const s = status;
  const state = STATES[s.state];
  return (
    <div className={PAGE_STACK}>
      <FormAlert message={failed} />
      <div data-overview className={PAGE_STACK}>
        <EnshroudedHeader
          badge={<StatusBadge tone={state.tone} label={state.label} moving={state.moving} />}
          buttons={<ControlButtons status={s} now={now} command={command} ask={(cmd) => setCommand({ kind: 'confirming', cmd })} />}
        />
        <Feedback
          status={s}
          now={now}
          command={command}
          confirm={(cmd) => void send(cmd)}
          cancel={() => setCommand({ kind: 'idle' })}
        />
        <Figures>
          <PlayersFigure status={s} />
          <StatCard
            compact
            grow
            label="Uptime"
            value={s.uptime_secs !== null ? duration(s.uptime_secs) : <span aria-hidden="true">—</span>}
            footer={
              s.uptime_secs !== null ? (
                <StatNote short={UPTIME_NOTE_SHORT}>{UPTIME_NOTE}</StatNote>
              ) : (
                <StatNote>Not running.</StatNote>
              )
            }
          />
          <StatCard
            compact
            grow
            label="Build"
            value={s.version ?? <span aria-hidden="true">—</span>}
            footer={<BuildNote status={s} now={now} />}
          />
        </Figures>
      </div>
      <UpdateNotice status={s} now={now} />
      {/* Always both: a card with nothing to list says so, so the grid is never lopsided. */}
      <div className={`grid lg:grid-cols-3 3xl:grid-cols-[2fr_1fr] ${PAGE_GAP}`}>
        <Sessions status={s} />
        <Activity recent={s.recent} now={now} />
      </div>
    </div>
  );
});

/** Players of the slots, with a bar of how full the server is. */
function PlayersFigure({ status }: { status: Status }) {
  const p = status.players;
  return (
    <StatCard
      compact
      grow
      label="Players online"
      shortLabel="Players"
      heading
      value={
        p === null ? (
          <>
            <span aria-hidden="true">—</span>
            <span className="sr-only">Unknown</span>
          </>
        ) : (
          <>
            {p.online}
            <span className="text-xl text-muted-foreground @[340px]/card:text-2xl"> / {p.max}</span>
          </>
        )
      }
      footer={
        <>
          <Meter value={p?.online ?? 0} max={p?.max ?? 1} />
          {p === null ? (
            <StatNote short="Unknown">The server doesn't say.</StatNote>
          ) : (
            <StatNote short={`${p.max - p.online} free`}>{`${p.max - p.online} of ${p.max} slots free`}</StatNote>
          )}
        </>
      }
    />
  );
}

/** Whether the running build is Steam's latest, and when that came out. */
function BuildNote({ status, now }: { status: Status; now: number }) {
  if (status.update_queued && status.state === 'Online') return <StatNote>Update queued</StatNote>;
  if (status.latest_build_at === null) return <StatNote short="Unknown">Latest build unknown</StatNote>;
  if (status.update_pending) return <StatNote short="Newer build">Newer build on Steam</StatNote>;
  return <StatNote short="Up to date">{`Up to date · from ${ago(now, status.latest_build_at)}`}</StatNote>;
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
  if (status.latest_build_at === null || !status.update_pending) return null;
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

/** Who is on, and for how long (Enshrouded shares no names): a tile each, as many across as the
 * card has room for. */
function Sessions({ status }: { status: Status }) {
  const sessions = status.players?.sessions ?? [];
  return (
    <Card className="@container/card lg:col-span-2 3xl:col-span-1">
      <CardHeader>
        <CardTitle>
          <h2>Who's on</h2>
        </CardTitle>
        <CardDescription>Enshrouded doesn't share player names.</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-1 flex-col">
        {sessions.length === 0 ? (
          <Empty icon={Users}>
            {status.players === null ? "The server doesn't say who is on." : 'Nobody is on right now.'}
          </Empty>
        ) : (
          <ul className="grid gap-2 text-sm @[32rem]/card:grid-cols-2 @[48rem]/card:grid-cols-3 @[64rem]/card:grid-cols-4">
            {sessions.map((secs, i) => (
              <li key={i} className="flex items-center justify-between gap-4 rounded-xl bg-muted/50 px-3 py-2.5">
                <span className="font-medium">Player {i + 1}</span>
                <span className="text-muted-foreground tabular-nums">on for {duration(secs)}</span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}

/** What a card says when it has nothing to list: centred in the room the card has. */
function Empty({ icon: Icon, children }: { icon: LucideIcon; children: string }) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-2 rounded-xl border border-dashed px-4 py-8 text-center text-sm text-muted-foreground">
      <Icon aria-hidden="true" className="size-5" />
      <p>{children}</p>
    </div>
  );
}

/** Restart and Force update: off while they can't be used (the notes under the heading say why). */
function ControlButtons({
  status,
  now,
  command,
  ask,
}: {
  status: Status;
  now: number;
  command: CommandUi;
  ask: (cmd: Command) => void;
}) {
  const disabled = !status.can_control || busy(status.state) || waitFor(status, now) !== null || command.kind === 'sending';
  const sendingAny = (...cmds: Command[]) => command.kind === 'sending' && cmds.includes(command.cmd);

  return (
    <>
      <Button disabled={disabled} onClick={() => ask('restart')} data-faro-user-action-name="enshrouded restart">
        {sendingAny('restart') && <LoaderCircle aria-hidden="true" className="motion-safe:animate-spin" />}
        {ENSHROUDED_BUTTONS.restart}
      </Button>
      <Button variant="outline" disabled={disabled} onClick={() => ask('update')} data-faro-user-action-name="enshrouded update">
        {sendingAny('update', 'update_now') && <LoaderCircle aria-hidden="true" className="motion-safe:animate-spin" />}
        {ENSHROUDED_BUTTONS.update}
      </Button>
    </>
  );
}

/** Seconds until the next command is allowed, or null. */
function waitFor(status: Status, now: number): number | null {
  return status.next_command_at !== null && status.next_command_at > now ? status.next_command_at - now : null;
}

/** Under the heading, when there is anything to say: what the state means, why the buttons wait,
 * and the confirmation of a command or what came of it (read out as it comes). */
function Feedback({
  status,
  now,
  command,
  confirm,
  cancel,
}: {
  status: Status;
  now: number;
  command: CommandUi;
  confirm: (cmd: Command) => void;
  cancel: () => void;
}) {
  const hint = STATES[status.state].hint;
  const wait = waitFor(status, now);
  const note = !status.can_control
    ? "The website can't reach the server's controls right now."
    : wait !== null
      ? `Someone just sent a command: the buttons come back in ${wait} s.`
      : busy(status.state)
        ? 'Wait for the current restart or update to finish.'
        : null;
  return (
    <div aria-live="polite" className="flex flex-col gap-4 empty:hidden">
      {hint && <p className="text-sm text-muted-foreground">{hint}</p>}
      {note && <p className="text-sm text-muted-foreground">{note}</p>}
      <Answer command={command} online={status.players?.online ?? 0} confirm={confirm} cancel={cancel} />
    </div>
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
                <Button size="sm" onClick={() => confirm('update')} data-faro-user-action-name="enshrouded confirm update">
                  Update when empty
                </Button>
                <Button size="sm" variant="destructive" onClick={() => confirm('update_now')} data-faro-user-action-name="enshrouded confirm update_now">
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
              <Button size="sm" onClick={() => confirm(command.cmd)} data-faro-user-action-name={`enshrouded confirm ${command.cmd}`}>
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

/** Who restarted or updated the server lately: the line, then when under it. */
function Activity({ recent, now }: { recent: Schemas['EnshroudedAction'][]; now: number }) {
  return (
    <Card className="@container/card">
      <CardHeader>
        <CardTitle>
          <h2>Recent commands</h2>
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-1 flex-col">
        {recent.length === 0 ? (
          <Empty icon={History}>No commands sent lately.</Empty>
        ) : (
          <ul className="flex flex-col divide-y text-sm">
            {recent.map((a, i) => (
              <li key={i} className="flex flex-col gap-0.5 py-2.5 first:pt-0 last:pb-0">
                <span>
                  <span className="font-medium">{a.name ?? 'A deleted account'}</span>
                  {actionText(a.action)}
                </span>
                <span className="text-xs text-muted-foreground">{ago(now, a.at)}</span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
