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
} from 'lucide-react';

export const RemoteView: React.FC = () => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  const { isConnected, peerAddress, peerName, latency, fps, setLatency, setFps, disconnect } =
    useConnectionStore();

  const [isFullscreen, setIsFullscreen] = useState(false);
  const [quality, setQuality] = useState<'ultra' | 'balanced' | 'speed'>('balanced');
  const [isAudioMuted, setIsAudioMuted] = useState(false);
  const [showSettingsDropdown, setShowSettingsDropdown] = useState(false);
  const [inputLocked, setInputLocked] = useState(true);
  const [remoteRes, setRemoteRes] = useState<{ w: number; h: number }>({ w: 1920, h: 1080 });
  const [toastMsg, setToastMsg] = useState<string | null>(null);
  const lastMoveSentRef = useRef<number>(0);

  const showToast = useCallback((msg: string) => {
    setToastMsg(msg);
    setTimeout(() => setToastMsg((cur) => (cur === msg ? null : cur)), 2500);
  }, []);

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

  // Real screen-frame listener from Tauri backend
  useEffect(() => {
    if (!isConnected) {
      setHasReceivedFrame(false);
      return;
    }
    let unlisten: (() => void) | undefined;
    let frameCount = 0;
    let lastFpsTime = performance.now();

    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        unlisten = await listen<{ image: string; width: number; height: number; timestamp: number }>(
          'screen-frame',
          (event) => {
            const { image, timestamp, width, height } = event.payload;
            const canvas = canvasRef.current;
            if (!canvas) return;
            const ctx = canvas.getContext('2d');
            if (!ctx) return;

            const img = new Image();
            img.onload = () => {
              if (canvas.width !== width || canvas.height !== height) {
                canvas.width = width;
                canvas.height = height;
                setRemoteRes({ w: width, h: height });
              }
              ctx.drawImage(img, 0, 0, width, height);
              setHasReceivedFrame(true);

              frameCount++;
              const now = performance.now();
              if (now - lastFpsTime >= 1000) {
                setFps(Math.round((frameCount * 1000) / (now - lastFpsTime)));
                frameCount = 0;
                lastFpsTime = now;
              }
              setLatency(Math.max(1, Date.now() - timestamp));
            };
            img.src = image;
          }
        );
      } catch (err) {
        console.warn('Real screen-frame listener error or web mode:', err);
      }
    })();

    // Auto-focus container to capture keyboard events immediately
    containerRef.current?.focus();

    return () => {
      if (unlisten) unlisten();
    };
  }, [isConnected, setFps, setLatency]);

  // Compute exact remote desktop coordinates accounting for object-contain letterboxing
  const mapCanvasCoords = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    const rect = e.currentTarget.getBoundingClientRect();
    const cw = canvas?.width || 1920;
    const ch = canvas?.height || 1080;

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
      x: Math.round((relX / drawWidth) * cw),
      y: Math.round((relY / drawHeight) * ch),
    };
  }, []);

  // Forward Mouse Events (throttled for Move so clicks/shortcuts never queue behind move spam)
  const handleMouseMove = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      if (!isConnected || !inputLocked) return;
      const now = performance.now();
      if (now - lastMoveSentRef.current < 33) return;
      lastMoveSentRef.current = now;

      const { x, y } = mapCanvasCoords(e);

      safeInvoke('send_mouse_event', {
        event: {
          x,
          y,
          button: null,
          action: 'Move',
          delta_x: null,
          delta_y: null,
        },
      }).catch(() => {});
    },
    [isConnected, inputLocked, mapCanvasCoords]
  );

  const handleMouseDown = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
      containerRef.current?.focus();
      if (!isConnected || !inputLocked) return;
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
      }).catch((err) => console.warn('MouseDown send error:', err));
    },
    [isConnected, inputLocked, mapCanvasCoords]
  );

  const handleMouseUp = useCallback(
    (e: React.MouseEvent<HTMLCanvasElement>) => {
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
      }).catch((err) => console.warn('MouseUp send error:', err));
    },
    [isConnected, inputLocked, mapCanvasCoords]
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (!isConnected || !inputLocked) return;
      e.preventDefault();
      safeInvoke('send_key_event', { key: e.key, action: 'Down' }).catch(() => {});
    },
    [isConnected, inputLocked]
  );

  const handleKeyUp = useCallback(
    (e: React.KeyboardEvent) => {
      if (!isConnected || !inputLocked) return;
      e.preventDefault();
      safeInvoke('send_key_event', { key: e.key, action: 'Up' }).catch(() => {});
    },
    [isConnected, inputLocked]
  );

  const handleWheel = useCallback(
    (e: React.WheelEvent<HTMLCanvasElement>) => {
      if (!isConnected || !inputLocked) return;
      const { x, y } = mapCanvasCoords(e);
      const dy = e.deltaY > 0 ? -1 : 1;

      safeInvoke('send_mouse_event', {
        event: {
          x,
          y,
          button: null,
          action: 'Scroll',
          delta_x: null,
          delta_y: dy,
        },
      }).catch(() => {});
    },
    [isConnected, inputLocked, mapCanvasCoords]
  );

  const sendShortcut = async (shortcut: string) => {
    try {
      await safeInvoke('send_shortcut', { shortcut });
      if (shortcut === 'CtrlAltDel') {
        showToast('已发送 Ctrl+Alt+Del (注：Windows 用户态安全策略可能拦截该组合键，建议用「任务管理器」)');
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

  return (
    <div
      ref={containerRef}
      tabIndex={0}
      onKeyDown={handleKeyDown}
      onKeyUp={handleKeyUp}
      className="flex-1 flex flex-col h-full bg-black relative select-none outline-none"
    >
      {/* Top Remote Control Header */}
      <div className="h-12 bg-surface/90 border-b border-surface-lighter/60 px-4 flex items-center justify-between backdrop-blur z-20 shrink-0">
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-2">
            <span
              className={`w-2.5 h-2.5 rounded-full ${
                isConnected ? 'bg-emerald-400 animate-pulse' : 'bg-zinc-500'
              }`}
            />
            <span className="text-xs font-semibold text-white tracking-wide">
              {isConnected ? peerName || peerAddress : '未连接到远程桌面'}
            </span>
          </div>

          {isConnected && (
            <div className="hidden sm:flex items-center gap-3 text-xs bg-surface-dark/80 px-2.5 py-1 rounded-md border border-surface-lighter font-mono text-zinc-300">
              <span className="flex items-center gap-1 text-primary-light">
                <Activity className="w-3 h-3" />
                {latency || 9} ms
              </span>
              <span className="text-zinc-500">|</span>
              <span className="text-emerald-400">{fps || 60} FPS</span>
              <span className="text-zinc-500">|</span>
              <span className="text-zinc-400">{remoteRes.w}x{remoteRes.h}</span>
            </div>
          )}
        </div>

        {/* Toolbar Controls */}
        <div
          className="flex items-center gap-2"
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
                className={`p-1.5 rounded-lg border text-xs font-medium flex items-center gap-1.5 transition-colors ${
                  inputLocked
                    ? 'bg-primary/20 border-primary/40 text-primary-light'
                    : 'bg-surface-light border-surface-lighter text-zinc-400'
                }`}
                title={inputLocked ? '输入捕获已开启' : '输入捕获已禁用'}
              >
                <MousePointer className="w-3.5 h-3.5" />
                <span className="hidden md:inline">{inputLocked ? '按键拦截中' : '仅查看'}</span>
              </button>

              {/* Audio toggle */}
              <button
                type="button"
                onClick={() => {
                  const next = !isAudioMuted;
                  setIsAudioMuted(next);
                  showToast(next ? '已静音远程音频' : '已恢复远程音频');
                }}
                className="p-1.5 rounded-lg bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-zinc-300 transition-colors"
                title={isAudioMuted ? '取消静音' : '静音'}
              >
                {isAudioMuted ? <VolumeX className="w-3.5 h-3.5 text-rose-400" /> : <Volume2 className="w-3.5 h-3.5" />}
              </button>

              {/* Quality Settings Dropdown */}
              <div className="relative">
                <button
                  type="button"
                  onClick={() => setShowSettingsDropdown(!showSettingsDropdown)}
                  className="p-1.5 rounded-lg bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-zinc-300 flex items-center gap-1 text-xs transition-colors"
                  title="画质与编码设置"
                >
                  <Sliders className="w-3.5 h-3.5" />
                  <span className="uppercase font-mono text-[11px]">{quality}</span>
                </button>

                {showSettingsDropdown && (
                  <div className="absolute right-0 mt-2 w-48 bg-surface border border-surface-lighter rounded-xl shadow-2xl p-2 z-50 text-xs animate-in fade-in zoom-in-95">
                    <div className="text-[10px] text-zinc-400 font-semibold px-2 py-1 uppercase">画质模式</div>
                    <button
                      type="button"
                      onClick={() => {
                        setQuality('ultra');
                        setShowSettingsDropdown(false);
                        showToast('已切换画质模式: 超清原画');
                      }}
                      className={`w-full text-left px-2.5 py-1.5 rounded-lg transition-colors ${
                        quality === 'ultra' ? 'bg-primary text-white font-medium' : 'text-zinc-300 hover:bg-surface-light'
                      }`}
                    >
                      超清画质 (原画 4K/60)
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setQuality('balanced');
                        setShowSettingsDropdown(false);
                        showToast('已切换画质模式: 平衡流畅');
                      }}
                      className={`w-full text-left px-2.5 py-1.5 rounded-lg transition-colors ${
                        quality === 'balanced' ? 'bg-primary text-white font-medium' : 'text-zinc-300 hover:bg-surface-light'
                      }`}
                    >
                      平衡流畅 (推荐 1080P)
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setQuality('speed');
                        setShowSettingsDropdown(false);
                        showToast('已切换画质模式: 极速低延迟');
                      }}
                      className={`w-full text-left px-2.5 py-1.5 rounded-lg transition-colors ${
                        quality === 'speed' ? 'bg-primary text-white font-medium' : 'text-zinc-300 hover:bg-surface-light'
                      }`}
                    >
                      极速竞技 (超低延迟 120Hz)
                    </button>
                  </div>
                )}
              </div>

              {/* Shortcut buttons */}
              <div className="hidden lg:flex items-center gap-1 border-l border-surface-lighter pl-2">
                <button
                  type="button"
                  onClick={() => sendShortcut('CtrlAltDel')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono transition-colors"
                  title="向被控端发送 Ctrl+Alt+Del"
                >
                  Ctrl+Alt+Del
                </button>
                <button
                  type="button"
                  onClick={() => sendShortcut('TaskMgr')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono transition-colors"
                  title="向被控端发送 Ctrl+Shift+Esc 打开任务管理器"
                >
                  任务管理器
                </button>
                <button
                  type="button"
                  onClick={() => sendShortcut('WinD')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono transition-colors"
                  title="向被控端发送 Win+D 显示桌面"
                >
                  Win+D
                </button>
                <button
                  type="button"
                  onClick={() => sendShortcut('WinE')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono transition-colors"
                  title="向被控端发送 Win+E 打开我的电脑"
                >
                  资源管理器
                </button>
                <button
                  type="button"
                  onClick={() => sendShortcut('AltTab')}
                  className="px-2 py-1 rounded bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-[11px] text-zinc-300 font-mono transition-colors"
                  title="向被控端发送 Alt+Tab 切换窗口"
                >
                  Alt+Tab
                </button>
              </div>

              {/* Fullscreen Button */}
              <button
                type="button"
                onClick={toggleFullscreen}
                className="p-1.5 rounded-lg bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-zinc-300 transition-colors"
                title={isFullscreen ? '退出全屏' : '全屏显示'}
              >
                {isFullscreen ? <Minimize className="w-3.5 h-3.5" /> : <Maximize className="w-3.5 h-3.5" />}
              </button>

              {/* Disconnect Button */}
              <button
                type="button"
                onClick={handleDisconnect}
                className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-semibold text-rose-300 bg-rose-500/10 hover:bg-rose-500/20 border border-rose-500/30 transition-colors"
              >
                <Unlink className="w-3.5 h-3.5" />
                <span>结束远程</span>
              </button>
            </>
          )}
        </div>
      </div>

      {/* Toast Notification */}
      {toastMsg && (
        <div className="absolute top-14 left-1/2 -translate-x-1/2 z-50 px-4 py-1.5 rounded-full bg-surface/95 border border-primary/40 text-xs text-primary-light shadow-xl backdrop-blur pointer-events-none">
          {toastMsg}
        </div>
      )}

      {/* Screen Canvas Area / Placeholder */}
      <div className="flex-1 flex items-center justify-center relative overflow-hidden bg-zinc-950 select-none">
        {isConnected ? (
          <>
            <canvas
              ref={canvasRef}
              width={1920}
              height={1080}
              onMouseMove={handleMouseMove}
              onMouseDown={handleMouseDown}
              onMouseUp={handleMouseUp}
              onWheel={handleWheel}
              onContextMenu={(e) => e.preventDefault()}
              className={`w-full h-full object-contain cursor-crosshair ${hasReceivedFrame ? 'block' : 'hidden'}`}
            />
            {!hasReceivedFrame && (
              <div className="flex flex-col items-center justify-center gap-3 text-zinc-400">
                <div className="w-8 h-8 border-2 border-primary border-t-transparent rounded-full animate-spin" />
                <p className="text-xs tracking-wide">已建立传输通道，等待远程屏幕画面推流...</p>
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
