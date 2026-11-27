import React, { useRef, useEffect, useState, useCallback } from 'react';
import { useConnectionStore } from '../stores/connectionStore';
import { safeInvoke } from '../hooks/useTauri';
import {
  Maximize,
  Minimize,
  Sliders,
  Unlink,
  Activity,
  Tv,
  MousePointer,
  Volume2,
  VolumeX,
  Monitor,
  ChevronDown,
} from 'lucide-react';

// Map browser KeyboardEvent to clean, backward-compatible key identifier
// Prefers physical e.code so Shift+symbols and active local IME still send exact keys!
function normalizeKeyboardKey(e: KeyboardEvent): string | null {
  const code = e.code;
  if (code) {
    if (code.startsWith('Key') && code.length === 4) {
      return code[3].toLowerCase();
    }
    if (code.startsWith('Digit') && code.length === 6) {
      return code[5];
    }
    switch (code) {
      case 'ShiftLeft':
      case 'ShiftRight':
      case 'ControlLeft':
      case 'ControlRight':
      case 'AltLeft':
      case 'AltRight':
      case 'MetaLeft':
      case 'MetaRight':
      case 'NumpadEnter':
      case 'IntlBackslash':
        return code;
      case 'OSLeft':
        return 'MetaLeft';
      case 'OSRight':
        return 'MetaRight';
      case 'Space':
        return ' ';
      case 'Minus':
        return '-';
      case 'Equal':
        return '=';
      case 'BracketLeft':
        return '[';
      case 'BracketRight':
        return ']';
      case 'Backslash':
        return '\\';
      case 'Semicolon':
        return ';';
      case 'Quote':
        return "'";
      case 'Backquote':
        return '`';
      case 'Comma':
        return ',';
      case 'Period':
        return '.';
      case 'Slash':
        return '/';
      case 'Enter':
        return 'Enter';
      case 'Backspace':
      case 'Tab':
      case 'Escape':
      case 'CapsLock':
      case 'NumLock':
      case 'ScrollLock':
      case 'PrintScreen':
      case 'Pause':
      case 'Insert':
      case 'Delete':
      case 'Home':
      case 'End':
      case 'PageUp':
      case 'PageDown':
      case 'ArrowUp':
      case 'ArrowDown':
      case 'ArrowLeft':
      case 'ArrowRight':
      case 'ContextMenu':
        return code;
      default:
        if (/^F([1-9]|1[0-2])$/.test(code) || code.startsWith('Numpad')) {
          return code;
        }
    }
  }
  if (!e.key || e.key === 'Process' || e.key === 'Dead' || e.key === 'Unidentified') {
    return null;
  }
  return e.key;
}

export const RemoteView: React.FC = () => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const videoViewportRef = useRef<HTMLDivElement | null>(null);

  const {
    isConnected,
    peerAddress,
    peerName,
    connectionMode,
    connectionTarget,
    streamPort,
    latency,
    fps,
    setLatency,
    setFps,
    disconnect,
  } = useConnectionStore();

  const [isFullscreen, setIsFullscreen] = useState(false);
  const [quality, setQuality] = useState<'ultra' | 'balanced' | 'speed'>('balanced');
  const [isAudioMuted, setIsAudioMuted] = useState(false);
  const [showSettingsDropdown, setShowSettingsDropdown] = useState(false);
  const [showResolutionDropdown, setShowResolutionDropdown] = useState(false);
  const [inputLocked, setInputLocked] = useState(true);
  const [remoteCursor, setRemoteCursor] = useState<string>('default');
  const [remoteRes, setRemoteRes] = useState<{ w: number; h: number }>({ w: 1920, h: 1080 });
  const [bitrateKbps, setBitrateKbps] = useState<number>(0);
  const [toastMsg, setToastMsg] = useState<string | null>(null);
  const [waitingDetail, setWaitingDetail] = useState<string | null>(null);
  const [showRdpFixBanner, setShowRdpFixBanner] = useState<boolean>(false);
  const [isNativeD3D11, setIsNativeD3D11] = useState<boolean>(false);
  const [rendererLabel, setRendererLabel] = useState<string>('');
  const lastMoveSentRef = useRef<number>(0);
  const moveInFlightRef = useRef<boolean>(false);
  const trailingMoveTimerRef = useRef<number | null>(null);
  const pendingMoveCoordsRef = useRef<{ x: number; y: number } | null>(null);
  const hasFrameRef = useRef<boolean>(false);
  const lastInputErrToastRef = useRef<number>(0);

  const showToast = useCallback((msg: string) => {
    setToastMsg(msg);
    setTimeout(() => setToastMsg((cur) => (cur === msg ? null : cur)), 3200);
  }, []);

  const reportInputError = useCallback(
    (err: unknown) => {
      const now = performance.now();
      if (now - lastInputErrToastRef.current > 4000) {
        lastInputErrToastRef.current = now;
        showToast(`键鼠事件发送失败: ${String(err)}`);
      }
    },
    [showToast]
  );

  // Fullscreen toggle
  const toggleFullscreen = () => {
    if (!containerRef.current) return;
    if (!document.fullscreenElement) {
      containerRef.current.requestFullscreen().then(() => setIsFullscreen(true)).catch(console.error);
    } else {
      document.exitFullscreen().then(() => setIsFullscreen(false)).catch(console.error);
    }
  };

  useEffect(() => {
    const handleFullscreenChange = () => {
      setIsFullscreen(!!document.fullscreenElement);
    };
    document.addEventListener('fullscreenchange', handleFullscreenChange);
    return () => document.removeEventListener('fullscreenchange', handleFullscreenChange);
  }, []);

  const [hasReceivedFrame, setHasReceivedFrame] = useState(false);

  // Zero-copy atomic FRAM batch stream reader + Native D3D11 H.264 lifecycle listeners
  useEffect(() => {
    if (!isConnected) {
      hasFrameRef.current = false;
      setHasReceivedFrame(false);
      setIsNativeD3D11(false);
      setRendererLabel('');
      setWaitingDetail(null);
      setShowRdpFixBanner(false);
      setBitrateKbps(0);
      setRemoteCursor('default');
      safeInvoke('sync_native_viewport', { x: 0, y: 0, width: 0, height: 0, visible: false }).catch(() => {});
      return;
    }

    let aborted = false;
    const abortCtrl = new AbortController();
    let unlistenLatency: (() => void) | undefined;
    let unlistenCursor: (() => void) | undefined;
    let unlistenNotice: (() => void) | undefined;
    let unlistenNativeFirst: (() => void) | undefined;
    let unlistenNativeStats: (() => void) | undefined;
    let keyframeRetryTimer: number | null = null;
    let rafId = 0;

    let frameDrawCount = 0;
    let bytesThisSecond = 0;
    let lastStatsTime = performance.now();

    type DecodedTile = { x: number; y: number; w: number; h: number; bitmap: ImageBitmap };
    type PendingFrame = {
      screenW: number;
      screenH: number;
      ready: boolean;
      failed: boolean;
      tiles: DecodedTile[];
    };

    const frameQueue: PendingFrame[] = [];
    let cachedCtx: CanvasRenderingContext2D | null = null;

    const getCanvasCtx = () => {
      const canvas = canvasRef.current;
      if (!canvas) return null;
      if (!cachedCtx || cachedCtx.canvas !== canvas) {
        cachedCtx =
          (canvas.getContext('2d', { alpha: false } as CanvasRenderingContext2DSettings) as CanvasRenderingContext2D | null) ||
          canvas.getContext('2d');
      }
      return { canvas, ctx: cachedCtx };
    };

    const flushReadyFrames = () => {
      rafId = 0;
      if (aborted) return;

      const target = getCanvasCtx();
      if (!target || !target.ctx) return;
      const { canvas, ctx } = target;

      let paintedAny = false;
      // Drain all ready frames from the head of the queue in strict arrival order
      while (frameQueue.length > 0 && frameQueue[0].ready) {
        const frame = frameQueue.shift()!;
        if (frame.failed || frame.tiles.length === 0) continue;

        if (canvas.width !== frame.screenW || canvas.height !== frame.screenH) {
          canvas.width = frame.screenW;
          canvas.height = frame.screenH;
          setRemoteRes({ w: frame.screenW, h: frame.screenH });
        }

        for (let i = 0; i < frame.tiles.length; i++) {
          const t = frame.tiles[i];
          ctx.drawImage(t.bitmap, t.x, t.y, t.w, t.h);
          t.bitmap.close();
        }
        paintedAny = true;
        frameDrawCount++;
      }

      if (paintedAny && !hasFrameRef.current) {
        hasFrameRef.current = true;
        setHasReceivedFrame(true);
      }

      const now = performance.now();
      const elapsed = now - lastStatsTime;
      if (elapsed >= 1000) {
        setFps(Math.min(120, Math.round((frameDrawCount * 1000) / elapsed)));
        setBitrateKbps(Math.round((bytesThisSecond * 8) / elapsed));
        frameDrawCount = 0;
        bytesThisSecond = 0;
        lastStatsTime = now;
      }

      if (frameQueue.length > 0 && frameQueue[0].ready && !rafId) {
        rafId = requestAnimationFrame(flushReadyFrames);
      }
    };

    const scheduleFlush = () => {
      if (!aborted) {
        // Paint immediately on decode completion (shaves 8-16.6ms vs deferring to next rAF!)
        flushReadyFrames();
      }
    };

    // Start latency & remote cursor shape listeners
    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        unlistenLatency = await listen<number>('connection-latency', (event) => {
          if (typeof event.payload === 'number') {
            setLatency(event.payload);
          }
        });
        unlistenCursor = await listen<string>('remote-cursor-shape', (event) => {
          if (typeof event.payload === 'string' && event.payload.length > 0 && event.payload !== 'none') {
            setRemoteCursor(event.payload);
          }
        });
        unlistenNotice = await listen<{ level?: string; message?: string }>('host-notice', (event) => {
          const msg = event.payload?.message;
          if (typeof msg === 'string' && msg.length > 0) {
            showToast(msg);
            if (!hasFrameRef.current) {
              setWaitingDetail(msg);
            }
            if (msg.includes('os error 5') || msg.includes('拒绝访问') || msg.includes('RDP')) {
              setShowRdpFixBanner(true);
            } else if (msg.includes('切回本地桌面') && msg.includes('成功')) {
              setShowRdpFixBanner(false);
            }
          }
        });
        unlistenNativeFirst = await listen<{ width?: number; height?: number }>('native-first-frame', (event) => {
          if (aborted) return;
          hasFrameRef.current = true;
          setHasReceivedFrame(true);
          setIsNativeD3D11(true);
          setRendererLabel('D3D11 H.264');
          const w = event.payload?.width;
          const h = event.payload?.height;
          if (typeof w === 'number' && typeof h === 'number' && w > 0 && h > 0) {
            setRemoteRes({ w, h });
          }
        });
        unlistenNativeStats = await listen<{
          fps?: number;
          bitrate_kbps?: number;
          width?: number;
          height?: number;
          renderer?: string;
        }>('native-stream-stats', (event) => {
          if (aborted) return;
          const p = event.payload;
          if (!p) return;
          if (typeof p.fps === 'number') setFps(p.fps);
          if (typeof p.bitrate_kbps === 'number') setBitrateKbps(p.bitrate_kbps);
          if (typeof p.width === 'number' && typeof p.height === 'number' && p.width > 0 && p.height > 0) {
            setRemoteRes({ w: p.width, h: p.height });
          }
          if (typeof p.renderer === 'string') setRendererLabel(p.renderer);
        });
      } catch (err) {
        console.warn('Tauri event listener error:', err);
      }
    })();

    // Start binary atomic frame stream reader from local 127.0.0.1:<streamPort>/stream
    if (streamPort && streamPort > 0) {
      (async () => {
        try {
          const response = await fetch(`http://127.0.0.1:${streamPort}/stream`, {
            signal: abortCtrl.signal,
          });
          const reader = response.body?.getReader();
          if (!reader) return;

          // Proactively ask host for a fresh keyframe once our stream reader is live
          safeInvoke('request_keyframe').catch(() => {});
          keyframeRetryTimer = window.setInterval(() => {
            if (!aborted && !hasFrameRef.current) {
              safeInvoke('request_keyframe').catch(() => {});
            } else if (keyframeRetryTimer !== null) {
              window.clearInterval(keyframeRetryTimer);
              keyframeRetryTimer = null;
            }
          }, 2000);

          // Growable ring/cursor buffer: O(1) amortized, zero per-tile reallocation
          let buf = new Uint8Array(512 * 1024);
          let readPos = 0;
          let writePos = 0;

          const appendChunk = (chunk: Uint8Array) => {
            const avail = writePos - readPos;
            const needed = avail + chunk.length;
            if (buf.length - writePos < chunk.length) {
              if (buf.length >= needed) {
                buf.copyWithin(0, readPos, writePos);
                writePos = avail;
                readPos = 0;
              } else {
                let nextCap = buf.length * 2;
                while (nextCap < needed) nextCap *= 2;
                const next = new Uint8Array(nextCap);
                next.set(buf.subarray(readPos, writePos), 0);
                buf = next;
                writePos = avail;
                readPos = 0;
              }
            }
            buf.set(chunk, writePos);
            writePos += chunk.length;
          };

          while (!aborted) {
            const { value, done } = await reader.read();
            if (done || !value) break;
            appendChunk(value);

            // Parse complete atomic "FRAM" batches
            while (writePos - readPos >= 10) {
              // Verify "FRAM" magic (0x46 0x52 0x41 0x4D)
              if (
                buf[readPos] !== 0x46 ||
                buf[readPos + 1] !== 0x52 ||
                buf[readPos + 2] !== 0x41 ||
                buf[readPos + 3] !== 0x4d
              ) {
                readPos += 1;
                continue;
              }

              const hdrView = new DataView(buf.buffer, buf.byteOffset + readPos, 10);
              const screenW = hdrView.getUint16(4, true);
              const screenH = hdrView.getUint16(6, true);
              const tileCount = hdrView.getUint16(8, true);

              // First pass: check if all tiles of this atomic batch have arrived
              let cursor = readPos + 10;
              let complete = true;
              for (let i = 0; i < tileCount; i++) {
                if (writePos - cursor < 12) {
                  complete = false;
                  break;
                }
                const tView = new DataView(buf.buffer, buf.byteOffset + cursor, 12);
                const dataLen = tView.getUint32(8, true);
                cursor += 12 + dataLen;
                if (cursor > writePos) {
                  complete = false;
                  break;
                }
              }

              if (!complete) {
                break; // Wait for rest of this frame batch
              }

              const batchBytes = cursor - readPos;
              bytesThisSecond += batchBytes;

              // Extract tile slices and kick off parallel hardware-accelerated decode
              const rawTiles: Array<{ x: number; y: number; w: number; h: number; blob: Blob }> = [];
              let p = readPos + 10;
              for (let i = 0; i < tileCount; i++) {
                const tView = new DataView(buf.buffer, buf.byteOffset + p, 12);
                const tx = tView.getUint16(0, true);
                const ty = tView.getUint16(2, true);
                const tw = tView.getUint16(4, true);
                const th = tView.getUint16(6, true);
                const dataLen = tView.getUint32(8, true);
                p += 12;
                const jpegCopy = buf.slice(p, p + dataLen);
                p += dataLen;
                rawTiles.push({
                  x: tx,
                  y: ty,
                  w: tw,
                  h: th,
                  blob: new Blob([jpegCopy], { type: 'image/jpeg' }),
                });
              }

              readPos = cursor;
              if (readPos === writePos) {
                readPos = 0;
                writePos = 0;
              }

              if (screenW === 0 || screenH === 0 || rawTiles.length === 0) {
                continue;
              }

              // Tight ultra-low-latency backlog guard: never let >2 frames queue in WebView
              const isFullFrame = rawTiles.some((t) => t.w >= screenW && t.h >= screenH / 4);
              if (isFullFrame && frameQueue.length >= 2) {
                while (frameQueue.length > 0) {
                  const dropped = frameQueue.shift()!;
                  dropped.failed = true; // mark skipped if still decoding
                  dropped.tiles.forEach((dt) => dt.bitmap.close());
                  dropped.tiles = [];
                }
              }

              const pending: PendingFrame = {
                screenW,
                screenH,
                ready: false,
                failed: false,
                tiles: [],
              };
              frameQueue.push(pending);

              Promise.all(
                rawTiles.map(async (rt) => {
                  const bitmap = await createImageBitmap(rt.blob);
                  return { x: rt.x, y: rt.y, w: rt.w, h: rt.h, bitmap };
                })
              )
                .then((decoded) => {
                  if (aborted || pending.failed) {
                    decoded.forEach((d) => d.bitmap.close());
                    return;
                  }
                  pending.tiles = decoded;
                  pending.ready = true;
                  scheduleFlush();
                })
                .catch(() => {
                  pending.failed = true;
                  pending.ready = true;
                  scheduleFlush();
                });
            }
          }
        } catch (err) {
          if (!aborted) {
            console.warn('Binary stream reader error:', err);
          }
        }
      })();
    }

    containerRef.current?.focus();

    return () => {
      aborted = true;
      abortCtrl.abort();
      if (rafId) cancelAnimationFrame(rafId);
      while (frameQueue.length > 0) {
        const f = frameQueue.shift()!;
        f.tiles.forEach((t) => t.bitmap.close());
      }
      if (trailingMoveTimerRef.current !== null) {
        window.clearTimeout(trailingMoveTimerRef.current);
        trailingMoveTimerRef.current = null;
      }
      if (keyframeRetryTimer !== null) {
        window.clearInterval(keyframeRetryTimer);
        keyframeRetryTimer = null;
      }
      moveInFlightRef.current = false;
      pendingMoveCoordsRef.current = null;
      if (unlistenLatency) unlistenLatency();
      if (unlistenCursor) unlistenCursor();
      if (unlistenNotice) unlistenNotice();
      if (unlistenNativeFirst) unlistenNativeFirst();
      if (unlistenNativeStats) unlistenNativeStats();
    };
  }, [isConnected, streamPort, setFps, setLatency, showToast]);

  // Keep native Direct3D11 child HWND aligned cleanly to videoViewportRef (physical pixels)
  useEffect(() => {
    const syncRect = () => {
      const el = videoViewportRef.current;
      if (!el || !isConnected || !isNativeD3D11) {
        safeInvoke('sync_native_viewport', { x: 0, y: 0, width: 0, height: 0, visible: false }).catch(() => {});
        return;
      }
      const rect = el.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      safeInvoke('sync_native_viewport', {
        x: Math.round(rect.left * dpr),
        y: Math.round(rect.top * dpr),
        width: Math.max(1, Math.round(rect.width * dpr)),
        height: Math.max(1, Math.round(rect.height * dpr)),
        visible: rect.width > 8 && rect.height > 8,
      }).catch(() => {});
    };

    syncRect();
    const ro = new ResizeObserver(() => syncRect());
    if (videoViewportRef.current) {
      ro.observe(videoViewportRef.current);
    }
    window.addEventListener('resize', syncRect);
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', syncRect);
    };
  }, [isConnected, isNativeD3D11, showResolutionDropdown, showSettingsDropdown, showRdpFixBanner, toastMsg, isFullscreen]);

  // Hide native D3D11 viewport only on unmount (leaving RemoteView tab)
  useEffect(() => {
    return () => {
      safeInvoke('sync_native_viewport', { x: 0, y: 0, width: 0, height: 0, visible: false }).catch(() => {});
    };
  }, []);

  useEffect(() => {
    safeInvoke('set_native_input_locked', { locked: inputLocked }).catch(() => {});
  }, [inputLocked]);

  // Global window keyboard capture while connected & inputLocked (never drops keys when focus shifts!)
  useEffect(() => {
    if (!isConnected || !inputLocked) return;

    const isEditableTarget = (target: EventTarget | null) => {
      if (!(target instanceof HTMLElement)) return false;
      const tag = target.tagName;
      return tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT' || target.isContentEditable;
    };

    const onKeyDown = (e: KeyboardEvent) => {
      if (isEditableTarget(e.target)) return;
      const key = normalizeKeyboardKey(e);
      if (!key) return;
      e.preventDefault();
      safeInvoke('send_key_event', { key, action: 'Down' }).catch(reportInputError);
    };

    const onKeyUp = (e: KeyboardEvent) => {
      if (isEditableTarget(e.target)) return;
      const key = normalizeKeyboardKey(e);
      if (!key) return;
      e.preventDefault();
      safeInvoke('send_key_event', { key, action: 'Up' }).catch(reportInputError);
    };

    const onCompositionEnd = (e: CompositionEvent) => {
      if (isEditableTarget(e.target) || !e.data) return;
      for (const ch of e.data) {
        if (ch.charCodeAt(0) >= 0x80) {
          safeInvoke('send_key_event', { key: ch, action: 'Click' }).catch(reportInputError);
        }
      }
    };

    // Release modifier keys if controller window loses focus so remote modifiers never stick
    const onBlur = () => {
      for (const mod of ['ControlLeft', 'ControlRight', 'ShiftLeft', 'ShiftRight', 'AltLeft', 'AltRight', 'MetaLeft', 'MetaRight']) {
        safeInvoke('send_key_event', { key: mod, action: 'Up' }).catch(() => {});
      }
    };

    window.addEventListener('keydown', onKeyDown, true);
    window.addEventListener('keyup', onKeyUp, true);
    window.addEventListener('compositionend', onCompositionEnd, true);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKeyDown, true);
      window.removeEventListener('keyup', onKeyUp, true);
      window.removeEventListener('compositionend', onCompositionEnd, true);
      window.removeEventListener('blur', onBlur);
    };
  }, [isConnected, inputLocked, reportInputError]);

  // Compute exact remote desktop coordinates accounting for object-contain letterboxing
  const mapCanvasCoords = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    const rect = e.currentTarget.getBoundingClientRect();
    const cw = canvas?.width || 1920;
    const ch = canvas?.height || 1080;
    if (rect.width <= 0 || rect.height <= 0) {
      return { x: 0, y: 0 };
    }

    const canvasAspect = cw / ch;
    const rectAspect = rect.width / rect.height;

    let drawWidth = rect.width;
    let drawHeight = rect.height;
    let offsetX = 0;
    let offsetY = 0;

    if (rectAspect > canvasAspect) {
      drawWidth = rect.height * canvasAspect;
      offsetX = (rect.width - drawWidth) / 2;
    } else {
      drawHeight = rect.width / canvasAspect;
      offsetY = (rect.height - drawHeight) / 2;
    }

    const relX = Math.max(0, Math.min(drawWidth, e.clientX - rect.left - offsetX));
    const relY = Math.max(0, Math.min(drawHeight, e.clientY - rect.top - offsetY));

    return {
      x: Math.round((relX / Math.max(1, drawWidth)) * cw),
      y: Math.round((relY / Math.max(1, drawHeight)) * ch),
    };
  }, []);

  // Forward Mouse Events with 1-in-flight coalescing (125Hz on direct links, 50Hz on relay).
  // Guarantees zero move-event queuing lag so clicks and cursor stops are instantaneous!
  const flushMouseMove = useCallback(
    (x: number, y: number) => {
      lastMoveSentRef.current = performance.now();
      moveInFlightRef.current = true;
      safeInvoke('send_mouse_event', {
        event: {
          x,
          y,
          button: null,
          action: 'Move',
          delta_x: null,
          delta_y: null,
        },
      })
        .catch(reportInputError)
        .finally(() => {
          moveInFlightRef.current = false;
          const latest = pendingMoveCoordsRef.current;
          if (latest && trailingMoveTimerRef.current === null) {
            pendingMoveCoordsRef.current = null;
            flushMouseMove(latest.x, latest.y);
          }
        });
    },
    [reportInputError]
  );

  const handleMouseMove = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      if (!isConnected || !inputLocked) return;
      const { x, y } = mapCanvasCoords(e);
      const minInterval = connectionMode === 'Relay' ? 20 : 8;
      const now = performance.now();
      const elapsed = now - lastMoveSentRef.current;

      if (moveInFlightRef.current) {
        // Keep only the latest coordinates while previous IPC is in flight
        pendingMoveCoordsRef.current = { x, y };
        return;
      }

      if (elapsed >= minInterval) {
        if (trailingMoveTimerRef.current !== null) {
          window.clearTimeout(trailingMoveTimerRef.current);
          trailingMoveTimerRef.current = null;
        }
        pendingMoveCoordsRef.current = null;
        flushMouseMove(x, y);
      } else {
        pendingMoveCoordsRef.current = { x, y };
        if (trailingMoveTimerRef.current === null) {
          trailingMoveTimerRef.current = window.setTimeout(() => {
            trailingMoveTimerRef.current = null;
            if (moveInFlightRef.current) return;
            const coords = pendingMoveCoordsRef.current;
            pendingMoveCoordsRef.current = null;
            if (coords) {
              flushMouseMove(coords.x, coords.y);
            }
          }, Math.max(1, Math.ceil(minInterval - elapsed)));
        }
      }
    },
    [isConnected, inputLocked, connectionMode, mapCanvasCoords, flushMouseMove]
  );

  const handleMouseDown = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      e.preventDefault();
      setShowSettingsDropdown(false);
      setShowResolutionDropdown(false);
      containerRef.current?.focus();
      if (!isConnected || !inputLocked) return;
      if (trailingMoveTimerRef.current !== null) {
        window.clearTimeout(trailingMoveTimerRef.current);
        trailingMoveTimerRef.current = null;
        pendingMoveCoordsRef.current = null;
      }
      const { x, y } = mapCanvasCoords(e);
      const btn = e.button === 0 ? 'Left' : e.button === 2 ? 'Right' : 'Middle';

      safeInvoke('send_mouse_event', {
        event: {
          x,
          y,
          button: btn,
          action: 'Down',
          delta_x: null,
          delta_y: null,
        },
      }).catch(reportInputError);
    },
    [isConnected, inputLocked, mapCanvasCoords, reportInputError]
  );

  const handleMouseUp = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      e.preventDefault();
      if (!isConnected || !inputLocked) return;
      const { x, y } = mapCanvasCoords(e);
      const btn = e.button === 0 ? 'Left' : e.button === 2 ? 'Right' : 'Middle';

      safeInvoke('send_mouse_event', {
        event: {
          x,
          y,
          button: btn,
          action: 'Up',
          delta_x: null,
          delta_y: null,
        },
      }).catch(reportInputError);
    },
    [isConnected, inputLocked, mapCanvasCoords, reportInputError]
  );

  const handleWheel = useCallback(
    (e: React.WheelEvent<HTMLCanvasElement>) => {
      if (!isConnected || !inputLocked) return;
      if (e.deltaY === 0) return;
      e.preventDefault();
      const { x, y } = mapCanvasCoords(e);
      const notches = Math.max(1, Math.min(5, Math.round(Math.abs(e.deltaY) / 100) || 1));
      const dy = e.deltaY > 0 ? notches : -notches;

      safeInvoke('send_mouse_event', {
        event: {
          x,
          y,
          button: null,
          action: 'Scroll',
          delta_x: null,
          delta_y: dy,
        },
      }).catch(reportInputError);
    },
    [isConnected, inputLocked, mapCanvasCoords, reportInputError]
  );

  const handleSetResolution = async (width: number, height: number, label: string) => {
    setShowResolutionDropdown(false);
    try {
      await safeInvoke('set_remote_resolution', { width, height });
      showToast(`正在切换远程分辨率: ${label}...`);
    } catch (err) {
      showToast(`切换分辨率失败: ${err}`);
    }
    containerRef.current?.focus();
  };

  const handleDetachRemoteRdp = async () => {
    try {
      await safeInvoke('detach_remote_rdp');
      showToast('已向被控端发送「脱离 RDP 切回本地桌面」指令，请稍候 1 秒...');
      setTimeout(() => {
        safeInvoke('request_keyframe').catch(() => {});
      }, 800);
    } catch (err) {
      showToast(`发送切回本地桌面指令失败: ${err}`);
    }
  };

  const sendShortcut = async (shortcut: string) => {
    try {
      await safeInvoke('send_shortcut', { shortcut });
      if (shortcut === 'CtrlAltDel') {
        showToast('已发送原生 Ctrl+Alt+Del (SendSAS) 指令');
      } else {
        showToast(`已发送快捷键指令: ${shortcut}`);
      }
      containerRef.current?.focus();
    } catch (err) {
      showToast(`发送快捷键失败: ${err}`);
    }
  };

  const handleDisconnect = async () => {
    try {
      await safeInvoke('disconnect');
    } catch (err) {
      console.warn('Backend disconnect error:', err);
    }
    disconnect();
  };

  const localScreenW = (Math.round((window.screen?.width || 1920) * (window.devicePixelRatio || 1)) + 1) & ~1;
  const localScreenH = (Math.round((window.screen?.height || 1080) * (window.devicePixelRatio || 1)) + 1) & ~1;

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      className="flex-1 flex flex-col h-full bg-black relative select-none outline-none"
    >
      {/* Top Remote Control Header (overflow-visible so dropdown menus float cleanly above canvas!) */}
      <div className="h-12 bg-surface/90 border-b border-surface-lighter/60 px-3 flex items-center justify-between gap-2 backdrop-blur z-30 shrink-0 overflow-visible">
        <div className="flex items-center gap-2 min-w-0">
          <div className="flex items-center gap-1.5 min-w-0">
            <span
              className={`w-2 h-2 rounded-full shrink-0 ${
                isConnected ? 'bg-emerald-400 animate-pulse' : 'bg-zinc-500'
              }`}
            />
            <span className="text-xs font-semibold text-white tracking-wide whitespace-nowrap max-w-[130px] truncate">
              {isConnected ? peerName || peerAddress : '未连接到远程桌面'}
            </span>
          </div>

          {isConnected && (
            <div className="hidden sm:flex items-center gap-2 text-xs bg-surface-dark/80 px-2 py-1 rounded-md border border-surface-lighter font-mono text-zinc-300 whitespace-nowrap shrink-0">
              {connectionMode && (
                <>
                  <span
                    title={`传输通道: ${connectionTarget || connectionMode}`}
                    className={`px-1.5 py-0.5 rounded text-[10px] font-bold whitespace-nowrap ${
                      connectionMode === 'Relay'
                        ? 'bg-amber-500/20 text-amber-300 border border-amber-500/30'
                        : 'bg-emerald-500/20 text-emerald-300 border border-emerald-500/30'
                    }`}
                  >
                    {connectionMode === 'IPv6'
                      ? 'IPv6 直连'
                      : connectionMode === 'LAN'
                      ? '局域网直连'
                      : connectionMode === 'P2P'
                      ? 'P2P 打洞直连'
                      : '云端中继'}
                  </span>
                  <span className="text-zinc-600">|</span>
                </>
              )}
              <span className="flex items-center gap-1 text-primary-light whitespace-nowrap" title="真实网络往返延迟 (Ping/Pong RTT)">
                <Activity className="w-3 h-3 shrink-0" />
                {latency > 0 ? `${latency} ms` : '测速中'}
              </span>
              <span className="text-zinc-500">|</span>
              <span className="text-emerald-400 whitespace-nowrap">{fps || 0} FPS</span>
              {bitrateKbps > 0 && (
                <>
                  <span className="text-zinc-500 hidden md:inline">|</span>
                  <span className="text-sky-400 whitespace-nowrap hidden md:inline" title="实时画面传输码率">
                    {bitrateKbps >= 1000 ? `${(bitrateKbps / 1000).toFixed(1)} Mbps` : `${bitrateKbps} Kbps`}
                  </span>
                </>
              )}
              {rendererLabel && (
                <>
                  <span className="text-zinc-500 hidden lg:inline">|</span>
                  <span className="text-violet-300 font-semibold whitespace-nowrap hidden lg:inline" title="当前视频渲染链路">
                    {rendererLabel}
                  </span>
                </>
              )}
            </div>
          )}
        </div>

        {/* Toolbar Controls */}
        <div
          className="flex items-center gap-1.5 shrink-0"
          onMouseDown={(e) => e.preventDefault()}
        >
          {isConnected && (
            <>
              {/* Input Control Mode */}
              <button
                type="button"
                onClick={() => {
                  const next = !inputLocked;
                  setInputLocked(next);
                  showToast(next ? '已开启键鼠控制' : '已切换为仅查看模式');
                  containerRef.current?.focus();
                }}
                className={`px-2 py-1.5 rounded-lg border text-xs font-medium flex items-center gap-1 whitespace-nowrap shrink-0 transition-colors ${
                  inputLocked
                    ? 'bg-primary/20 border-primary/40 text-primary-light'
                    : 'bg-surface-light border-surface-lighter text-zinc-400'
                }`}
                title={inputLocked ? '输入捕获已开启' : '输入捕获已禁用'}
              >
                <MousePointer className="w-3.5 h-3.5 shrink-0" />
                <span className="hidden xl:inline">{inputLocked ? '按键拦截中' : '仅查看'}</span>
              </button>

              {/* Audio toggle */}
              <button
                type="button"
                onClick={() => {
                  const next = !isAudioMuted;
                  setIsAudioMuted(next);
                  showToast(next ? '已静音远程音频' : '已恢复远程音频');
                }}
                className="p-1.5 rounded-lg bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-zinc-300 shrink-0 transition-colors"
                title={isAudioMuted ? '取消静音' : '静音'}
              >
                {isAudioMuted ? <VolumeX className="w-3.5 h-3.5 text-rose-400" /> : <Volume2 className="w-3.5 h-3.5" />}
              </button>

              {/* Resolution Switcher Toggle */}
              <button
                type="button"
                onClick={() => {
                  setShowResolutionDropdown((cur) => !cur);
                  setShowSettingsDropdown(false);
                }}
                className={`px-2 py-1.5 rounded-lg border flex items-center gap-1 text-xs whitespace-nowrap transition-colors shrink-0 ${
                  showResolutionDropdown
                    ? 'bg-primary/20 border-primary/50 text-primary-light'
                    : 'bg-surface-light hover:bg-surface-lighter border-surface-lighter text-zinc-200'
                }`}
                title="修改远程桌面分辨率"
              >
                <Monitor className="w-3.5 h-3.5 shrink-0 text-primary-light" />
                <span className="font-mono text-[11px]">{remoteRes.w}x{remoteRes.h}</span>
                <ChevronDown className="w-3 h-3 shrink-0 text-zinc-400" />
              </button>

              {/* Quality Settings Toggle */}
              <button
                type="button"
                onClick={() => {
                  setShowSettingsDropdown((cur) => !cur);
                  setShowResolutionDropdown(false);
                }}
                className={`px-2 py-1.5 rounded-lg border flex items-center gap-1 text-xs whitespace-nowrap transition-colors shrink-0 ${
                  showSettingsDropdown
                    ? 'bg-primary/20 border-primary/50 text-primary-light'
                    : 'bg-surface-light hover:bg-surface-lighter border-surface-lighter text-zinc-300'
                }`}
                title="画质与编码设置"
              >
                <Sliders className="w-3.5 h-3.5 shrink-0" />
                <span className="uppercase font-mono text-[11px]">{quality}</span>
                <ChevronDown className="w-3 h-3 shrink-0 text-zinc-400" />
              </button>

              {/* Shortcut buttons */}
              <div className="hidden xl:flex items-center gap-1 border-l border-surface-lighter pl-2 shrink-0">
                <button
                  type="button"
                  onClick={() => sendShortcut('CtrlAltDel')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono whitespace-nowrap transition-colors"
                  title="向被控端发送 Ctrl+Alt+Del"
                >
                  Ctrl+Alt+Del
                </button>
                <button
                  type="button"
                  onClick={() => sendShortcut('TaskMgr')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono whitespace-nowrap transition-colors"
                  title="向被控端发送 Ctrl+Shift+Esc 打开任务管理器"
                >
                  任务管理器
                </button>
                <button
                  type="button"
                  onClick={() => sendShortcut('WinD')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono whitespace-nowrap transition-colors"
                  title="向被控端发送 Win+D 显示桌面"
                >
                  Win+D
                </button>
                <button
                  type="button"
                  onClick={() => sendShortcut('WinE')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono whitespace-nowrap transition-colors"
                  title="向被控端发送 Win+E 打开我的电脑"
                >
                  资源管理器
                </button>
                <button
                  type="button"
                  onClick={() => sendShortcut('AltTab')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono whitespace-nowrap transition-colors"
                  title="向被控端发送 Alt+Tab 切换窗口"
                >
                  Alt+Tab
                </button>
              </div>

              {/* Fullscreen Button */}
              <button
                type="button"
                onClick={toggleFullscreen}
                className="p-1.5 rounded-lg bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-zinc-300 shrink-0 transition-colors"
                title={isFullscreen ? '退出全屏' : '全屏显示'}
              >
                {isFullscreen ? <Minimize className="w-3.5 h-3.5" /> : <Maximize className="w-3.5 h-3.5" />}
              </button>

              {/* Disconnect Button */}
              <button
                type="button"
                onClick={handleDisconnect}
                className="inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg text-xs font-semibold text-rose-300 bg-rose-500/10 hover:bg-rose-500/20 border border-rose-500/30 whitespace-nowrap shrink-0 transition-colors"
              >
                <Unlink className="w-3.5 h-3.5 shrink-0" />
                <span>结束远程</span>
              </button>
            </>
          )}
        </div>
      </div>

      {/* Inline Resolution Strip (slides video viewport down cleanly — never occluded by D3D11 HWND!) */}
      {isConnected && showResolutionDropdown && (
        <div className="bg-surface/95 border-b border-surface-lighter px-4 py-2 flex items-center flex-wrap gap-2 text-xs shrink-0 z-20">
          <span className="text-[11px] text-zinc-400 font-semibold mr-1">远程桌面分辨率:</span>
          <button
            type="button"
            onClick={() => handleSetResolution(localScreenW, localScreenH, `匹配本机 (${localScreenW}x${localScreenH})`)}
            className="px-2.5 py-1 rounded-lg bg-primary/15 hover:bg-primary/25 border border-primary/30 text-primary-light font-medium transition-colors"
          >
            匹配本机 ({localScreenW}x{localScreenH})
          </button>
          <button
            type="button"
            onClick={() => handleSetResolution(0, 0, '远程原生原画')}
            className="px-2.5 py-1 rounded-lg bg-surface-light hover:bg-surface-lighter text-zinc-300 transition-colors"
          >
            远程原生 (原画)
          </button>
          {[
            { w: 3840, h: 2160 },
            { w: 2560, h: 1440 },
            { w: 1920, h: 1200 },
            { w: 1920, h: 1080 },
            { w: 1680, h: 1050 },
            { w: 1600, h: 900 },
            { w: 1440, h: 900 },
            { w: 1280, h: 800 },
            { w: 1280, h: 720 },
          ].map((preset) => {
            const active = remoteRes.w === preset.w && remoteRes.h === preset.h;
            return (
              <button
                key={`${preset.w}x${preset.h}`}
                type="button"
                onClick={() => handleSetResolution(preset.w, preset.h, `${preset.w}x${preset.h}`)}
                className={`px-2.5 py-1 rounded-lg font-mono transition-colors ${
                  active ? 'bg-primary text-white font-medium' : 'bg-surface-light hover:bg-surface-lighter text-zinc-300'
                }`}
              >
                {preset.w}×{preset.h}
              </button>
            );
          })}
        </div>
      )}

      {/* Inline Quality Strip */}
      {isConnected && showSettingsDropdown && (
        <div className="bg-surface/95 border-b border-surface-lighter px-4 py-2 flex items-center flex-wrap gap-2 text-xs shrink-0 z-20">
          <span className="text-[11px] text-zinc-400 font-semibold mr-1">画质模式:</span>
          <button
            type="button"
            onClick={() => {
              setQuality('ultra');
              setShowSettingsDropdown(false);
              safeInvoke('set_stream_quality', { mode: 'ultra' }).catch(reportInputError);
              showToast('已切换画质模式: 超清原画 (60FPS)');
            }}
            className={`px-2.5 py-1 rounded-lg transition-colors ${
              quality === 'ultra' ? 'bg-primary text-white font-medium' : 'bg-surface-light text-zinc-300 hover:bg-surface-lighter'
            }`}
          >
            超清画质 (原画 60FPS)
          </button>
          <button
            type="button"
            onClick={() => {
              setQuality('balanced');
              setShowSettingsDropdown(false);
              safeInvoke('set_stream_quality', { mode: 'balanced' }).catch(reportInputError);
              showToast('已切换画质模式: 平衡流畅 (1080P/60FPS)');
            }}
            className={`px-2.5 py-1 rounded-lg transition-colors ${
              quality === 'balanced' ? 'bg-primary text-white font-medium' : 'bg-surface-light text-zinc-300 hover:bg-surface-lighter'
            }`}
          >
            平衡流畅 (推荐 60FPS)
          </button>
          <button
            type="button"
            onClick={() => {
              setQuality('speed');
              setShowSettingsDropdown(false);
              safeInvoke('set_stream_quality', { mode: 'speed' }).catch(reportInputError);
              showToast('已切换画质模式: 极速低延迟 (60FPS)');
            }}
            className={`px-2.5 py-1 rounded-lg transition-colors ${
              quality === 'speed' ? 'bg-primary text-white font-medium' : 'bg-surface-light text-zinc-300 hover:bg-surface-lighter'
            }`}
          >
            极速竞技 (超低带宽低延迟)
          </button>
        </div>
      )}

      {/* Inline Toast Bar (never covered by native D3D11 HWND!) */}
      {toastMsg && (
        <div className="bg-primary/15 border-b border-primary/30 px-4 py-1.5 text-center text-xs text-primary-light font-medium shrink-0">
          {toastMsg}
        </div>
      )}

      {/* One-Click RDP Detach Recovery Banner when os error 5 occurs after Frame 1 */}
      {isConnected && hasReceivedFrame && showRdpFixBanner && (
        <div className="bg-amber-500/15 border-b border-amber-500/30 px-4 py-2 flex items-center justify-between gap-3 text-xs z-20 shrink-0">
          <span className="text-amber-200">
            被控端当前处于 Windows RDP 会话且系统拒绝了键鼠/画面访问 (os error 5)，点击右侧按钮可一键切回本地物理桌面恢复全部控制。
          </span>
          <div className="flex items-center gap-2 shrink-0">
            <button
              type="button"
              onClick={handleDetachRemoteRdp}
              className="px-3 py-1 rounded-lg bg-amber-500 hover:bg-amber-400 text-zinc-950 font-semibold whitespace-nowrap transition-colors"
            >
              一键切回本地桌面
            </button>
            <button
              type="button"
              onClick={() => setShowRdpFixBanner(false)}
              className="text-zinc-400 hover:text-white px-1"
            >
              ✕
            </button>
          </div>
        </div>
      )}

      {/* Native Direct3D11 Video Viewport (with automatic fallback Canvas for legacy peers) */}
      <div
        ref={videoViewportRef}
        className="flex-1 flex items-center justify-center relative overflow-hidden bg-zinc-950 select-none"
      >
        {isConnected ? (
          <>
            {!isNativeD3D11 && (
              <canvas
                ref={canvasRef}
                width={1920}
                height={1080}
                onMouseMove={handleMouseMove}
                onMouseDown={handleMouseDown}
                onMouseUp={handleMouseUp}
                onWheel={handleWheel}
                onContextMenu={(e) => e.preventDefault()}
                style={{ cursor: inputLocked ? remoteCursor : 'default' }}
                className={`w-full h-full object-contain ${hasReceivedFrame ? 'block' : 'hidden'}`}
              />
            )}
            {!hasReceivedFrame && (
              <div className="flex flex-col items-center justify-center gap-3 text-zinc-400 px-6 text-center">
                <div className="w-8 h-8 border-2 border-primary border-t-transparent rounded-full animate-spin" />
                <p className="text-xs tracking-wide text-zinc-200">已建立传输通道，等待远程屏幕画面推流...</p>
                {waitingDetail ? (
                  <p className="text-xs text-amber-300 max-w-lg leading-relaxed bg-amber-500/10 border border-amber-500/30 rounded-lg px-3 py-2">
                    {waitingDetail}
                  </p>
                ) : (
                  <p className="text-[11px] text-zinc-500 max-w-md leading-relaxed">
                    提示：若延迟已正常显示但长时间无画面，请检查被控端是否处于锁屏、熄屏或最小化 Windows 远程桌面（RDP）窗口状态，唤醒/恢复被控端桌面即可立即出图。
                  </p>
                )}
                {(showRdpFixBanner || waitingDetail) && (
                  <button
                    type="button"
                    onClick={handleDetachRemoteRdp}
                    className="mt-1 px-4 py-2 rounded-xl bg-amber-500 hover:bg-amber-400 text-zinc-950 text-xs font-semibold shadow-lg transition-colors"
                  >
                    一键脱离 RDP 切回本地桌面（修复黑屏与键鼠）
                  </button>
                )}
              </div>
            )}
          </>
        ) : (
          <div className="max-w-md text-center p-8 bg-surface/50 border border-surface-lighter/50 rounded-2xl backdrop-blur-sm shadow-xl animate-fade-in">
            <div className="w-16 h-16 rounded-2xl bg-surface-light border border-surface-lighter flex items-center justify-center text-zinc-500 mx-auto mb-4">
              <Tv className="w-8 h-8" />
            </div>
            <h2 className="text-lg font-bold text-white mb-2">未连接到任何远程会话</h2>
            <p className="text-xs text-zinc-400 mb-6 leading-relaxed">
              请在“设备列表”中选择在线主机发起连接，或者直接输入远程桌面 IP / 识别码快速进入。
            </p>
            <div className="flex items-center justify-center gap-3">
              <span className="text-xs text-zinc-500">
                请在主页输入伙伴的 9 位设备代码一键发起跨网穿透连接
              </span>
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
