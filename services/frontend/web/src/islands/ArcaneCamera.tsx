import { useEffect, useRef, useState } from 'react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { wsUrl } from '@/api/client';
import { ExtensionCard, RollCard, UNDER_CAMERA } from '@/islands/ArcaneSkeleton';
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

type Connection = { kind: 'connecting' } | { kind: 'connected' } | { kind: 'disconnected' } | { kind: 'error'; message: string };

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** The dice reader: the camera's frames go to the server over `/ws/arcane` (looked up on the
 * global object when it connects, so the tracing script's wrapper gives the connection its
 * `camera session` span), the answers are drawn over the video, and a settled roll is shown.
 * Leaving the page (or hiding it into the back/forward cache) stops the camera and closes the
 * connection; the server closing it stops the camera, with a Reconnect button. */
export function ArcaneCamera() {
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
  const [sized, setSized] = useState(false);
  // Bumped by Reconnect: a new session, camera and connection.
  const [round, setRound] = useState(0);

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
      if (!v || !ov || !cap || !capCtx) return set({ kind: 'error', message: 'camera unavailable' });
      if (!navigator.mediaDevices?.getUserMedia) return set({ kind: 'error', message: 'camera unavailable' });
      // The back camera on a phone (the one pointing at the dice tray); `ideal`, so a device
      // with one camera still uses it.
      let s: MediaStream;
      try {
        s = await navigator.mediaDevices.getUserMedia({ video: { facingMode: { ideal: 'environment' } } });
      } catch {
        return set({ kind: 'error', message: 'camera permission denied' });
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

  const status =
    connection.kind === 'connecting' ? (
      <Badge variant="secondary">connecting</Badge>
    ) : connection.kind === 'connected' ? (
      <Badge>{frameMs}ms</Badge>
    ) : connection.kind === 'disconnected' ? (
      <Badge variant="destructive">disconnected</Badge>
    ) : (
      <Badge variant="destructive">{connection.message}</Badge>
    );

  return (
    <div className="flex flex-col gap-6">
      <div className="relative overflow-hidden rounded-2xl bg-muted ring-1 ring-foreground/10">
        {/* 16:9, as the placeholder, until the camera says its own shape. */}
        <video
          ref={video}
          className={sized ? 'block w-full' : 'block aspect-video w-full'}
          autoPlay
          playsInline
          muted
          aria-label="Camera"
          onLoadedMetadata={() => setSized(true)}
        />
        <canvas ref={overlay} className="pointer-events-none absolute inset-0 h-full w-full" aria-hidden="true" />
        <div className="absolute top-3 left-3">{status}</div>
        <Button
          size="sm"
          variant="secondary"
          className="absolute top-3 right-3"
          aria-pressed={debug}
          onClick={() => setDebug((d) => !d)}
        >
          {debug ? '✕ debug' : 'debug'}
        </Button>
      </div>
      {/* Read out when the connection changes, not with every frame's time. */}
      <p className="sr-only" role="status" aria-label="Connection">
        {connection.kind === 'connecting'
          ? 'Connecting'
          : connection.kind === 'connected'
            ? 'Connected'
            : connection.kind === 'disconnected'
              ? 'Disconnected'
              : connection.message}
      </p>
      {connection.kind === 'disconnected' && (
        <Button className="self-center" onClick={() => setRound((n) => n + 1)}>
          Reconnect
        </Button>
      )}
      {debug && (
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
      <div className={UNDER_CAMERA}>
        <RollCard roll={lastRoll} />
        <ExtensionCard />
      </div>
      {/* Off-screen: the frame is drawn here to be encoded, never shown. */}
      <canvas ref={capture} hidden />
    </div>
  );
}
