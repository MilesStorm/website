import { useEffect, useRef, useState } from 'react';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { wsUrl } from '@/api/client';
import {
  CAMERA_FRAME_MS,
  EXTENSION_XPI,
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
 * Leaving the page stops the camera and closes the connection. */
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

  useEffect(() => {
    let left = false;
    let stream: MediaStream | null = null;
    let socket: WebSocket | null = null;
    const set = (c: Connection) => !left && setConnection(c);

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
      if (ws.readyState !== WebSocket.OPEN) return set({ kind: 'disconnected' });
      set({ kind: 'connected' });

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
        if (ws.readyState !== WebSocket.OPEN) {
          set({ kind: 'disconnected' });
          break;
        }
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
      left = true;
      stream?.getTracks().forEach((t) => t.stop());
      if (video.current) video.current.srcObject = null;
      if (socket) {
        socket.onmessage = null;
        socket.close();
      }
    };
  }, []);

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
    <div className="flex flex-col items-center gap-4">
      <div className="relative w-full max-w-2xl">
        <video ref={video} className="w-full rounded-xl bg-muted" autoPlay playsInline muted aria-label="Camera" />
        <canvas ref={overlay} className="pointer-events-none absolute inset-0 h-full w-full rounded-xl" aria-hidden="true" />
        <div className="absolute top-2 left-2" role="status" aria-label="Connection">
          {status}
        </div>
        <Button
          size="sm"
          variant="secondary"
          className="absolute top-2 right-2"
          aria-pressed={debug}
          onClick={() => setDebug((d) => !d)}
        >
          {debug ? '✕ debug' : 'debug'}
        </Button>
      </div>
      <p className="w-full max-w-2xl text-center text-xs text-muted-foreground">
        Your latest roll's picture is kept for 10 minutes so you can flag it as wrong from the browser extension. It's
        only saved for training if you flag it or turn on sharing in your profile.
      </p>
      <p className="w-full max-w-2xl text-center text-2xl font-bold tabular-nums" aria-live="polite">
        {lastRoll === null ? (
          <span className="text-base font-normal text-muted-foreground italic">Roll some dice</span>
        ) : (
          <>
            <span className="font-normal text-muted-foreground">Last roll: </span>
            {lastRoll}
          </>
        )}
      </p>
      <Card className="w-full max-w-2xl">
        <CardContent className="flex flex-col gap-1 text-sm">
          <p>
            <span className="font-semibold">Playing on Roll20? </span>
            The Firefox extension types each roll into your game's chat, where Roll20 shows the total.{' '}
            <a className="font-medium text-primary underline underline-offset-4" href={EXTENSION_XPI}>
              Install the extension
            </a>
          </p>
          <p className="text-xs text-muted-foreground">
            Firefox asks you to confirm. If it only downloads the file, open it from the downloads list. It updates itself
            after that.
          </p>
        </CardContent>
      </Card>
      {debug && (
        <div className="flex w-full max-w-2xl flex-col gap-2 rounded-xl border bg-card p-3 font-mono text-xs">
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
        </div>
      )}
      {/* Off-screen: the frame is drawn here to be encoded, never shown. */}
      <canvas ref={capture} hidden />
    </div>
  );
}
