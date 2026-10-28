import { useEffect, useRef, useState, type CSSProperties, type ReactNode } from 'react';
import { Bug, CameraOff, LoaderCircle, Unplug } from 'lucide-react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { wsUrl } from '@/api/client';
import {
  ARCANE_GRID,
  CAMERA_COLUMN,
  CAMERA_FRAME,
  ExtensionCard,
  RollCard,
  SIDE_CARDS,
} from '@/islands/ArcaneSkeleton';
import {
  CAMERA_FRAME_MS,
  PageTimings,
  REPLY_WAIT_MS,
  detectionLines,
  drawOverlay,
  readMessage,
  rollText,
  type Detection,
} from '@/lib/arcane';
import { island } from '@/lib/island';

/** Why the camera didn't start: the browser (or the person) refused it, there is none, something
 * else has it, the browser can't open one here, or anything else. */
type Problem = 'denied' | 'missing' | 'busy' | 'unsupported' | 'failed';

type Connection = { kind: 'connecting' } | { kind: 'connected' } | { kind: 'disconnected' } | { kind: 'error'; problem: Problem };

const PROBLEMS: Record<Problem, { title: string; text: string; retry: string | null }> = {
  denied: {
    title: 'Camera access is off',
    text: 'The reader needs it to see your dice. Allow it when asked, or if it was blocked, set Camera to Allow in the site settings from the address bar.',
    retry: 'Allow camera',
  },
  missing: {
    title: 'No camera found',
    text: 'Plug one in, or open this page on a device that has one.',
    retry: 'Try again',
  },
  busy: {
    title: 'The camera is busy',
    text: 'Another app or tab may be using it. Close that, then try again.',
    retry: 'Try again',
  },
  unsupported: {
    title: "This browser can't use a camera here",
    text: 'Open the page in a current browser, over a secure connection.',
    retry: null,
  },
  failed: {
    title: "The camera didn't start",
    text: 'Something went wrong while opening it.',
    retry: 'Try again',
  },
};

/** What a refused `getUserMedia` means. */
const problemOf = (e: unknown): Problem => {
  switch (e instanceof DOMException ? e.name : '') {
    case 'NotAllowedError':
    case 'SecurityError':
      return 'denied';
    case 'NotFoundError':
    case 'OverconstrainedError':
      return 'missing';
    case 'NotReadableError':
    case 'AbortError':
      return 'busy';
    default:
      return 'failed';
  }
};

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The dice reader: the camera's frames go to the server over `/ws/arcane` (looked up on the
 * global object when it connects, so the tracing script's wrapper gives the connection its
 * `camera session` span), the answers are drawn over the video, and a settled roll is shown.
 * Leaving the page (or hiding it into the back/forward cache) stops the camera and closes the
 * connection; the server closing it stops the camera, with a Reconnect button. */
export const ArcaneCamera = island('ArcaneCamera', function ArcaneCamera() {
  const video = useRef<HTMLVideoElement>(null);
  const overlay = useRef<HTMLCanvasElement>(null);
  const capture = useRef<HTMLCanvasElement>(null);
  const [connection, setConnection] = useState<Connection>({ kind: 'connecting' });
  const [frameMs, setFrameMs] = useState(0);
  const [debug, setDebug] = useState(false);
  // The frame loop reads it between renders.
  const debugNow = useRef(false);
  debugNow.current = debug;
  const [detections, setDetections] = useState(0);
  const [serverError, setServerError] = useState<string | null>(null);
  const [debugText, setDebugText] = useState('');
  const [lastRoll, setLastRoll] = useState<string | null>(null);
  const [timings, setTimings] = useState('');
  // Bumped by Reconnect: a new session, camera and connection.
  const [round, setRound] = useState(0);
  // The camera's shape, once it has one (`w / h`): the frame takes it below lg (CAMERA_FRAME).
  const [ratio, setRatio] = useState<string | null>(null);

  useEffect(() => {
    let left = false;
    let stream: MediaStream | null = null;
    let socket: WebSocket | null = null;
    const set = (c: Connection) => !left && setConnection(c);
    const stopCamera = () => {
      stream?.getTracks().forEach((t) => t.stop());
      if (video.current) video.current.srcObject = null;
    };
    /** Stops the camera and closes the connection: the page is left (unmounted, or hidden into
     * the back/forward cache). */
    const leave = () => {
      left = true;
      stopCamera();
      if (socket) {
        socket.onmessage = null;
        socket.close();
      }
    };
    // Back to a page whose camera was stopped: start over, as a new page.
    const shown = (e: PageTransitionEvent) => {
      if (e.persisted) location.reload();
    };
    window.addEventListener('pagehide', leave);
    window.addEventListener('pageshow', shown);
    setConnection({ kind: 'connecting' });

    void (async () => {
      const v = video.current;
      const ov = overlay.current?.getContext('2d');
      const cap = capture.current;
      const capCtx = cap?.getContext('2d');
      if (!v || !ov || !cap || !capCtx) return set({ kind: 'error', problem: 'failed' });
      if (!navigator.mediaDevices?.getUserMedia) return set({ kind: 'error', problem: 'unsupported' });
      // The back camera on a phone (the one pointing at the dice tray); `ideal`, so a device
      // with one camera still uses it.
      let s: MediaStream;
      try {
        s = await navigator.mediaDevices.getUserMedia({ video: { facingMode: { ideal: 'environment' } } });
      } catch (e) {
        return set({ kind: 'error', problem: problemOf(e) });
      }
      // Left while the browser was still asking: this stream is nobody's.
      if (left) return s.getTracks().forEach((t) => t.stop());
      stream = s;
      v.srcObject = s;
      if (v.readyState < HTMLMediaElement.HAVE_METADATA) await new Promise((r) => v.addEventListener('loadedmetadata', r, { once: true }));
      const w = Math.max(1, v.videoWidth);
      const h = Math.max(1, v.videoHeight);
      cap.width = overlay.current!.width = w;
      cap.height = overlay.current!.height = h;
      setRatio(`${w} / ${h}`);

      const ws = new globalThis.WebSocket(wsUrl('/ws/arcane'));
      socket = ws;
      ws.binaryType = 'arraybuffer';
      await new Promise((r) => {
        ws.addEventListener('open', r, { once: true });
        ws.addEventListener('close', r, { once: true });
      });
      if (ws.readyState !== WebSocket.OPEN) {
        stopCamera();
        return set({ kind: 'disconnected' });
      }
      set({ kind: 'connected' });
      // The server closed it: the camera stops too, until Reconnect.
      ws.addEventListener('close', () => {
        if (left) return;
        stopCamera();
        set({ kind: 'disconnected' });
      });

      // The latest answer, for the draw; whether a frame awaits its answer, and since when.
      let latest: Detection[] = [];
      let awaiting = false;
      let sentAt = 0;
      let replyMs = 0;
      ws.onmessage = (e: MessageEvent) => {
        if (typeof e.data !== 'string') return;
        const msg = readMessage(e.data);
        if (!msg) return;
        if (msg.type === 'frame') {
          if (awaiting) replyMs = performance.now() - sentAt;
          awaiting = false;
          latest = msg.detections;
          setFrameMs(msg.frame_ms);
          setDetections(msg.detections.length);
          setServerError(null);
          setDebugText(detectionLines(msg.detections));
        } else if (msg.type === 'roll') {
          setLastRoll(rollText(msg));
        } else {
          awaiting = false;
          setServerError(msg.error);
          setDetections(0);
          setDebugText('');
        }
      };

      // The dice server sets the pace: the next frame goes when the last one is answered (it
      // keeps only the newest frame), never faster than the camera, and after REPLY_WAIT_MS
      // anyway (a frame it dropped gets no answer).
      const pageTimings = new PageTimings();
      let lastSent = 0;
      while (!left) {
        const waitFrom = performance.now();
        while (awaiting && performance.now() - waitFrom < REPLY_WAIT_MS) await sleep(4);
        const since = performance.now() - lastSent;
        if (since < CAMERA_FRAME_MS) await sleep(Math.ceil(CAMERA_FRAME_MS - since));
        if (left) break;
        if (ws.readyState !== WebSocket.OPEN) break;
        capCtx.drawImage(v, 0, 0);
        const encodeStarted = performance.now();
        // The JPEG's bytes as they are (a data URL would be base64, decoded again here).
        const blob = await new Promise<Blob | null>((r) => cap.toBlob(r, 'image/jpeg'));
        if (blob && ws.readyState === WebSocket.OPEN) {
          ws.send(blob);
          awaiting = true;
          lastSent = sentAt = performance.now();
          pageTimings.frame(lastSent, lastSent - encodeStarted, replyMs, blob.size);
          const report = pageTimings.report(lastSent, w, h);
          if (report && !left) setTimings(report);
        }
        drawOverlay(ov, latest, w, h, debugNow.current);
      }
    })();

    return () => {
      window.removeEventListener('pagehide', leave);
      window.removeEventListener('pageshow', shown);
      leave();
    };
  }, [round]);

  const live = connection.kind === 'connected';
  const status =
    connection.kind === 'connected' ? (
      <Badge>{frameMs}ms</Badge>
    ) : connection.kind === 'disconnected' ? (
      <Badge variant="destructive">disconnected</Badge>
    ) : null;

  return (
    <div className={ARCANE_GRID}>
      <div className={CAMERA_COLUMN}>
        <div className={CAMERA_FRAME} style={ratio ? ({ '--camera-ratio': ratio } as CSSProperties) : undefined}>
          {/* Whatever shape the camera has, it is fitted inside the frame (the overlay with it,
              so the boxes stay on the dice). Below lg the frame takes the camera's shape (a phone
              held upright films 9:16); from lg it stays 16:9 in its column. */}
          <video
            ref={video}
            className="absolute inset-0 size-full object-contain"
            autoPlay
            playsInline
            muted
            aria-label="Camera"
          />
          <canvas
            ref={overlay}
            className="pointer-events-none absolute inset-0 size-full object-contain"
            aria-hidden="true"
          />
          {connection.kind === 'connecting' && (
            <div className="absolute inset-0 flex items-center justify-center gap-2 text-sm text-muted-foreground" aria-hidden="true">
              <LoaderCircle className="size-4 motion-safe:animate-spin" />
              Starting the camera…
            </div>
          )}
          {connection.kind === 'error' && (
            <FramePanel
              icon={<CameraOff className="size-5 text-muted-foreground" aria-hidden="true" />}
              title={PROBLEMS[connection.problem].title}
              text={PROBLEMS[connection.problem].text}
              action={PROBLEMS[connection.problem].retry}
              onAction={() => setRound((n) => n + 1)}
              faro="arcane allow camera"
            />
          )}
          {connection.kind === 'disconnected' && (
            <FramePanel
              icon={<Unplug className="size-5 text-muted-foreground" aria-hidden="true" />}
              title="The connection closed"
              text="The camera is off until you connect again."
              action="Reconnect"
              onAction={() => setRound((n) => n + 1)}
              faro="arcane reconnect"
            />
          )}
          {status && <div className="absolute top-3 left-3">{status}</div>}
          {live && (
            <Button
              size="icon-sm"
              variant={debug ? 'default' : 'secondary'}
              className="absolute top-3 right-3"
              aria-label="Debug"
              aria-pressed={debug}
              onClick={() => setDebug((d) => !d)}
            >
              <Bug aria-hidden="true" />
            </Button>
          )}
        </div>
        {/* Read out when the connection changes, not with every frame's time. */}
        <p className="sr-only" role="status" aria-label="Connection">
          {connection.kind === 'connecting'
            ? 'Connecting'
            : connection.kind === 'connected'
              ? 'Connected'
              : connection.kind === 'disconnected'
                ? 'Disconnected'
                : PROBLEMS[connection.problem].title}
        </p>
        {debug && live && (
          <Card size="sm">
            <CardContent className="flex flex-col gap-2 font-mono text-xs">
              <div className="flex flex-wrap items-center gap-4">
                <span className="text-muted-foreground">frame_ms</span>
                <span className="font-bold tabular-nums">{frameMs}ms</span>
                <span className="text-muted-foreground">detections</span>
                <span className="font-bold tabular-nums">{detections}</span>
                <span className="font-bold tabular-nums">{timings}</span>
                <span className="text-muted-foreground">ws</span>
                <span className="font-bold">
                  {{ connecting: 'connecting', connected: 'open', disconnected: 'closed', error: 'error' }[connection.kind]}
                </span>
              </div>
              {serverError && <div className="break-all text-destructive">server: {serverError}</div>}
              {debugText ? (
                <pre className="overflow-x-auto leading-5 whitespace-pre-wrap">{debugText}</pre>
              ) : connection.kind === 'connected' ? (
                <div className="text-muted-foreground italic">no detections yet</div>
              ) : null}
            </CardContent>
          </Card>
        )}
      </div>
      <div className={SIDE_CARDS}>
        <RollCard roll={lastRoll} />
        <ExtensionCard />
      </div>
      {/* Off-screen: the frame is drawn here to be encoded, never shown. */}
      <canvas ref={capture} hidden />
    </div>
  );
});

/** What the frame says when the camera isn't running: what happened, what to do, and the button
 * that does it (a click is a gesture, so the browser may ask the person again). */
function FramePanel({
  icon,
  title,
  text,
  action,
  onAction,
  faro,
}: {
  icon: ReactNode;
  title: string;
  text: string;
  action: string | null;
  onAction: () => void;
  faro: string;
}) {
  return (
    <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 p-4 text-center sm:gap-4 sm:p-6">
      <div className="flex size-10 shrink-0 items-center justify-center rounded-full bg-background ring-1 ring-foreground/10 sm:size-12">
        {icon}
      </div>
      <div className="flex max-w-md flex-col gap-1">
        <h2 className="text-base font-medium">{title}</h2>
        <p className="text-sm text-muted-foreground">{text}</p>
      </div>
      {action && (
        <Button onClick={onAction} data-faro-user-action-name={faro}>
          {action}
        </Button>
      )}
    </div>
  );
}
