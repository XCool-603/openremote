import React, { useState } from 'react';
import { Monitor, Power, Link2, Unlink, Clock, Network, Cpu } from 'lucide-react';

interface DeviceCardProps {
  id: string;
  name: string;
  address: string;
  status: 'online' | 'offline';
  lastSeen?: string;
  macAddress?: string;
  isCurrentConnection?: boolean;
  onConnect: (id: string, address: string, name: string) => void;
  onDisconnect?: () => void;
  onWakeUp?: (mac: string) => void;
}

export const DeviceCard: React.FC<DeviceCardProps> = ({
  id,
  name,
  address,
  status,
  lastSeen,
  macAddress,
  isCurrentConnection = false,
  onConnect,
  onDisconnect,
  onWakeUp,
}) => {
  const [showWolInput, setShowWolInput] = useState(false);
  const [wolMac, setWolMac] = useState(macAddress || '');
  const [isWaking, setIsWaking] = useState(false);

  const isOnline = status === 'online';

  const handleWake = async () => {
    if (!wolMac.trim() || !onWakeUp) return;
    setIsWaking(true);
    try {
      await onWakeUp(wolMac.trim());
      setShowWolInput(false);
    } finally {
      setIsWaking(false);
    }
  };

  return (
    <div
      className={`group relative flex flex-col justify-between p-5 rounded-xl border transition-all duration-200 bg-surface-light/60 hover:bg-surface-light ${
        isCurrentConnection
          ? 'border-primary ring-1 ring-primary/40 shadow-lg shadow-primary/10'
          : 'border-surface-lighter/60 hover:border-surface-lighter shadow-md hover:shadow-xl'
      }`}
    >
      {/* Header */}
      <div>
        <div className="flex items-start justify-between gap-3 mb-3">
          <div className="flex items-center gap-3">
            <div
              className={`w-11 h-11 rounded-lg flex items-center justify-center transition-colors ${
                isOnline
                  ? 'bg-primary/20 text-primary-light border border-primary/30'
                  : 'bg-zinc-800 text-zinc-500 border border-zinc-700/50'
              }`}
            >
              <Monitor className="w-5 h-5" />
            </div>
            <div>
              <h3 className="font-semibold text-white text-base tracking-wide group-hover:text-primary-light transition-colors">
                {name}
              </h3>
              <div className="flex items-center gap-1.5 text-xs text-zinc-400 mt-0.5">
                <Network className="w-3 h-3 text-zinc-500" />
                <span className="font-mono">{address || '0.0.0.0'}</span>
              </div>
            </div>
          </div>

          {/* Status Badge */}
          <div
            className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium border ${
              isOnline
                ? 'bg-emerald-500/10 text-emerald-400 border-emerald-500/20'
                : 'bg-zinc-800/80 text-zinc-400 border-zinc-700/50'
            }`}
          >
            <span
              className={`w-1.5 h-1.5 rounded-full ${
                isOnline ? 'bg-emerald-400 animate-pulse' : 'bg-zinc-500'
              }`}
            />
            {isOnline ? '在线' : '离线'}
          </div>
        </div>

        {/* Device metadata */}
        <div className="space-y-1.5 my-3 text-xs text-zinc-400 border-t border-surface-lighter/40 pt-3">
          {lastSeen && (
            <div className="flex items-center gap-1.5">
              <Clock className="w-3.5 h-3.5 text-zinc-500" />
              <span>最后活跃: {lastSeen}</span>
            </div>
          )}
          {macAddress && (
            <div className="flex items-center gap-1.5">
              <Cpu className="w-3.5 h-3.5 text-zinc-500" />
              <span className="font-mono">MAC: {macAddress}</span>
            </div>
          )}
        </div>
      </div>

      {/* WOL Modal/Input Form inline */}
      {showWolInput && (
        <div className="mt-2 mb-3 p-3 bg-surface rounded-lg border border-surface-lighter">
          <label className="block text-xs font-medium text-zinc-300 mb-1.5">
            物理网卡 MAC 地址
          </label>
          <div className="flex gap-2">
            <input
              type="text"
              placeholder="AA:BB:CC:DD:EE:FF"
              value={wolMac}
              onChange={(e) => setWolMac(e.target.value)}
              className="flex-1 bg-surface-dark border border-surface-lighter rounded px-2.5 py-1.5 text-xs text-white placeholder-zinc-500 focus:outline-none focus:border-primary font-mono"
            />
            <button
              onClick={handleWake}
              disabled={isWaking || !wolMac.trim()}
              className="px-3 py-1.5 bg-primary hover:bg-primary-dark disabled:opacity-50 text-white rounded text-xs font-medium transition-colors"
            >
              {isWaking ? '唤醒中...' : '发送'}
            </button>
            <button
              onClick={() => setShowWolInput(false)}
              className="px-2.5 py-1.5 bg-zinc-800 hover:bg-zinc-700 text-zinc-300 rounded text-xs transition-colors"
            >
              取消
            </button>
          </div>
        </div>
      )}

      {/* Actions */}
      <div className="pt-2 flex items-center gap-2">
        {isCurrentConnection ? (
          <button
            onClick={onDisconnect}
            className="flex-1 inline-flex items-center justify-center gap-1.5 py-2 px-3 rounded-lg text-xs font-semibold text-rose-300 bg-rose-500/10 hover:bg-rose-500/20 border border-rose-500/30 transition-all"
          >
            <Unlink className="w-3.5 h-3.5" />
            断开连接
          </button>
        ) : (
          <button
            onClick={() => onConnect(id, address, name)}
            disabled={!isOnline}
            className="flex-1 inline-flex items-center justify-center gap-1.5 py-2 px-3 rounded-lg text-xs font-semibold text-white bg-primary hover:bg-primary-dark disabled:bg-surface-lighter disabled:text-zinc-500 disabled:cursor-not-allowed shadow-md hover:shadow-primary/25 transition-all"
          >
            <Link2 className="w-3.5 h-3.5" />
            连接设备
          </button>
        )}

        {!isOnline && onWakeUp && !showWolInput && (
          <button
            onClick={() => {
              if (macAddress) {
                onWakeUp(macAddress);
              } else {
                setShowWolInput(true);
              }
            }}
            title="远程开机 (Wake-on-LAN)"
            className="inline-flex items-center justify-center gap-1.5 py-2 px-3 rounded-lg text-xs font-medium text-amber-300 bg-amber-500/10 hover:bg-amber-500/20 border border-amber-500/30 transition-all"
          >
            <Power className="w-3.5 h-3.5" />
            远程开机
          </button>
        )}
      </div>
    </div>
  );
};
