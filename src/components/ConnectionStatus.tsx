import React from 'react';
import { useConnectionStore } from '../stores/connectionStore';
import { Activity, Wifi, WifiOff } from 'lucide-react';

export const ConnectionStatus: React.FC = () => {
  const { isConnected, latency, fps, peerName } = useConnectionStore();

  return (
    <div className="flex items-center gap-2 px-3 py-1.5 rounded-full bg-surface-light/80 border border-surface-lighter text-xs backdrop-blur-sm shadow-sm select-none">
      <span className="relative flex h-2 w-2">
        {isConnected ? (
          <>
            <span className="animate-ping absolute inline-flex h-full w-full rounded-full bg-accent-green opacity-75"></span>
            <span className="relative inline-flex rounded-full h-2 w-2 bg-accent-green"></span>
          </>
        ) : (
          <span className="relative inline-flex rounded-full h-2 w-2 bg-zinc-500"></span>
        )}
      </span>

      {isConnected ? (
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-1 text-emerald-400 font-medium">
            <Wifi className="w-3.5 h-3.5" />
            <span className="truncate max-w-[120px]" title={peerName || '已连接'}>
              {peerName || '已连接'}
            </span>
          </div>
          <div className="flex items-center gap-1 text-zinc-300">
            <Activity className="w-3.5 h-3.5 text-primary-light" />
            <span>{latency > 0 ? `${latency} ms` : '< 5 ms'}</span>
          </div>
          <div className="text-zinc-400 font-mono">
            {fps > 0 ? `${fps} FPS` : '60 FPS'}
          </div>
        </div>
      ) : (
        <div className="flex items-center gap-1.5 text-zinc-400">
          <WifiOff className="w-3.5 h-3.5 text-zinc-500" />
          <span>未连接</span>
        </div>
      )}
    </div>
  );
};
