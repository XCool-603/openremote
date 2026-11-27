import React, { useEffect, useState } from 'react';
import { useConnectionStore } from '../stores/connectionStore';
import { safeInvoke } from '../hooks/useTauri';
import { HostControlStatus } from '../types';
import { Activity, MonitorCheck, Wifi, WifiOff } from 'lucide-react';

export const ConnectionStatus: React.FC = () => {
  const { isConnected, latency, fps, peerName } = useConnectionStore();
  const [hostControl, setHostControl] = useState<HostControlStatus | null>(null);

  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const st = await safeInvoke<HostControlStatus>('get_host_control_status');
        if (!cancelled) {
          setHostControl(st?.active ? st : null);
        }
      } catch {
        // ignore in browser preview
      }
    };
    poll();
    const timer = window.setInterval(poll, 1200);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  const isHostActive = Boolean(hostControl?.active);
  const anyActive = isConnected || isHostActive;

  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded-full bg-surface-light/80 border border-surface-lighter text-xs backdrop-blur-sm shadow-sm select-none">
      <span className="relative flex h-2 w-2 shrink-0">
        {anyActive ? (
          <>
            <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-accent-green opacity-75"></span>
            <span className="relative inline-flex rounded-full h-2 w-2 bg-accent-green"></span>
          </>
        ) : (
          <span className="relative inline-flex rounded-full h-2 w-2 bg-zinc-500"></span>
        )}
      </span>

      {isConnected ? (
        <div className="flex items-center gap-3 min-w-0">
          <div className="flex items-center gap-1 text-emerald-400 font-medium min-w-0">
            <Wifi className="w-3.5 h-3.5 shrink-0" />
            <span className="truncate max-w-[110px]" title={peerName || '已连接'}>
              {peerName || '已连接'}
            </span>
          </div>
          <div className="flex items-center gap-1 text-zinc-300 shrink-0">
            <Activity className="w-3.5 h-3.5 text-primary-light" />
            <span>{latency > 0 ? `${latency} ms` : '测速中'}</span>
          </div>
          <div className="text-zinc-400 font-mono shrink-0">
            {fps > 0 ? `${fps} FPS` : '-- FPS'}
          </div>
        </div>
      ) : isHostActive && hostControl ? (
        <div className="flex items-center gap-2 min-w-0">
          <MonitorCheck className="w-3.5 h-3.5 text-emerald-400 shrink-0" />
          <span
            className="text-emerald-300 font-medium truncate max-w-[140px]"
            title={`${hostControl.controller_id} (${hostControl.remote_addr})`}
          >
            被控中 · {hostControl.transport_mode}
          </span>
        </div>
      ) : (
        <div className="flex items-center gap-1.5 text-zinc-400">
          <WifiOff className="w-3.5 h-3.5 text-zinc-500" />
          <span>未连接（待机就绪）</span>
        </div>
      )}
    </div>
  );
};
