import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { at } from './fixtures';

/** A message of the Arcane contract (contracts/arcane), as ai_pipeline sends it. */
const CONTRACTS = join(dirname(fileURLToPath(import.meta.url)), '../../../../contracts/arcane');
const fixture = (name: string): string => readFileSync(join(CONTRACTS, `${name}.json`), 'utf8');

beforeEach(() => vi.resetModules());
afterEach(cleanup);

describe('the Arcane messages', () => {
  it('read the contract fixtures, and nothing else', async () => {
    const { readMessage, rollText } = await import('@/lib/arcane');
    const frame = readMessage(fixture('frame'));
    expect(frame?.type).toBe('frame');
    const roll = readMessage(fixture('roll'));
    expect(roll?.type === 'roll' && rollText(roll)).toBe('16');
    const unread = readMessage(fixture('roll_unread'));
    expect(unread?.type === 'roll' && rollText(unread)).toBe('16 + ?');
    expect(readMessage(fixture('error'))).toEqual({ type: 'error', error: "the frame isn't a JPEG or PNG image" });
    expect(readMessage('{"type":"other"}')).toBeNull();
    expect(readMessage('not json')).toBeNull();
    expect(rollText({ dice: [{ value: '5', conf: 1, box: [] }, { value: '12', conf: 1, box: [] }], total: 17 })).toBe('5 + 12 = 17');
  });

  it('colour and label a box by its confidence', async () => {
    const { boxColor, boxLabel, detectionLines, readMessage } = await import('@/lib/arcane');
    const frame = readMessage(fixture('frame'));
    if (frame?.type !== 'frame') throw new Error('not a frame');
    const d = frame.detections[0]!;
    expect(boxLabel(d, false)).toBe('16 75%');
    expect(boxLabel(d, true)).toBe('16 75% | y:88%');
    expect(boxLabel({ ...d, confident: false }, false)).toBe('?');
    expect(boxColor(d, false)).toBe('rgba(52,211,153,0.95)');
    expect(boxColor(d, true)).toBe('rgba(251,191,36,0.95)');
    expect(boxColor({ ...d, confident: false }, true)).toBe('rgba(156,163,175,0.95)');
    expect(detectionLines([d])).toBe('det[0] dice=16 (75.0%)  yolo_cls=0 yolo_conf=87.5%  bbox=[0.250,0.500 → 0.375,0.625]');
  });

  it('time the page’s frames over a second', async () => {
    const { PageTimings } = await import('@/lib/arcane');
    const t = new PageTimings();
    t.frame(0, 4, 20, 2048);
    expect(t.report(500, 640, 480)).toBeNull();
    t.frame(1000, 6, 40, 4096);
    expect(t.report(1000, 640, 480)).toBe('1.0 fps · encode 5 ms · reply 30 ms · 640x480 3 kB');
  });

  it('a WebSocket address follows the page: wss on https', async () => {
    const { wsUrl } = await import('@/api/client');
    expect(wsUrl('/ws/arcane', at('https://milesstorm.test/arcane').location)).toBe('wss://milesstorm.test/ws/arcane');
    expect(wsUrl('/ws/arcane', { href: 'http://localhost:8080/arcane' })).toBe('ws://localhost:8080/ws/arcane');
  });
});

/** A WebSocket stand-in: what the page sent, and a way to answer as the server. */
class FakeSocket extends EventTarget {
  static last: FakeSocket | null = null;
  static OPEN = 1;
  readyState = 0;
  binaryType = 'blob';
  sent: unknown[] = [];
  closed = false;
  onmessage: ((e: MessageEvent) => void) | null = null;
  constructor(readonly url: string) {
    super();
    FakeSocket.last = this;
    setTimeout(() => {
      this.readyState = 1;
      this.dispatchEvent(new Event('open'));
    });
  }
  send(data: unknown) {
    this.sent.push(data);
  }
  close() {
    this.closed = true;
    this.readyState = 3;
  }
  answer(text: string) {
    this.onmessage?.(new MessageEvent('message', { data: text }));
  }
}

describe('the camera page', () => {
  it('sends the camera’s frames, shows the answers and the roll, and lets go when left', async () => {
    const page = at('https://milesstorm.test/arcane');
    vi.stubGlobal('location', page.location);
    vi.stubGlobal('WebSocket', FakeSocket);
    const track = { stop: vi.fn() };
    vi.stubGlobal('navigator', { mediaDevices: { getUserMedia: vi.fn(async () => ({ getTracks: () => [track] })) } });
    const ctx = { clearRect() {}, drawImage() {}, strokeRect() {}, fillRect() {}, fillText() {} };
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(ctx as unknown as CanvasRenderingContext2D);
    vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((done) => done(new Blob(['jpeg'])));

    const { ArcaneCamera } = await import('@/islands/ArcaneCamera');
    const { unmount } = render(<ArcaneCamera />);
    expect(screen.getByText('connecting')).toBeTruthy();
    const video = screen.getByLabelText('Camera') as HTMLVideoElement;
    await waitFor(() => expect(navigator.mediaDevices.getUserMedia).toHaveBeenCalledWith({ video: { facingMode: { ideal: 'environment' } } }));
    act(() => void video.dispatchEvent(new Event('loadedmetadata')));
    await waitFor(() => expect(FakeSocket.last?.sent.length).toBeGreaterThan(0));
    const socket = FakeSocket.last!;
    expect(socket.url).toBe('wss://milesstorm.test/ws/arcane');
    expect(socket.binaryType).toBe('arraybuffer');
    expect(socket.sent[0]).toBeInstanceOf(Blob);

    act(() => socket.answer(fixture('frame')));
    expect(await screen.findByText('42ms')).toBeTruthy();
    act(() => socket.answer(fixture('roll_unread')));
    expect(await screen.findByText('16 + ?')).toBeTruthy();

    await userEvent.click(screen.getByRole('button', { name: 'debug' }));
    expect(screen.getByText(/det\[0\] dice=16/)).toBeTruthy();
    act(() => socket.answer(fixture('error')));
    expect(await screen.findByText("server: the frame isn't a JPEG or PNG image")).toBeTruthy();

    unmount();
    expect(track.stop).toHaveBeenCalled();
    expect(socket.closed).toBe(true);
  });

  it('says when the camera is refused', async () => {
    vi.stubGlobal('location', at('https://milesstorm.test/arcane').location);
    vi.stubGlobal('navigator', { mediaDevices: { getUserMedia: vi.fn(async () => Promise.reject(new Error('NotAllowedError'))) } });
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({} as CanvasRenderingContext2D);
    const { ArcaneCamera } = await import('@/islands/ArcaneCamera');
    render(<ArcaneCamera />);
    expect(await screen.findByText('camera permission denied')).toBeTruthy();
  });
});
