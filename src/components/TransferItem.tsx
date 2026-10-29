import React from 'react';
import { TransferProgress } from '../types';
import {
  FileText,
  Pause,
  Play,
  X,
  CheckCircle2,
  AlertCircle,
  Loader2,
} from 'lucide-react';

interface TransferItemProps {
  transfer: TransferProgress;
  onPause?: (id: string) => void;
  onResume?: (id: string) => void;
  onCancel?: (id: string) => void;
}

function formatBytes(bytes: number, decimals = 1): string {
  if (bytes <= 0) return '0 B';
  const k = 1024;
  const dm = decimals < 0 ? 0 : decimals;
  const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(k));
  return `${parseFloat((bytes / Math.pow(k, i)).toFixed(dm))} ${sizes[i]}`;
}

export const TransferItem: React.FC<TransferItemProps> = ({
  transfer,
  onPause,
  onResume,
  onCancel,
}) => {
  const { id, filename, total_bytes, transferred_bytes, speed_bps, status } = transfer;

  const percent =
    total_bytes > 0
      ? Math.min(100, Math.round((transferred_bytes / total_bytes) * 100))
      : 0;

  return (
    <div className="bg-surface-light border border-surface-lighter rounded-xl p-3.5 hover:border-surface-lighter/80 transition-all">
      <div className="flex items-center justify-between gap-3 mb-2">
        <div className="flex items-center gap-2.5 min-w-0">
          <div className="w-8 h-8 rounded-lg bg-surface flex items-center justify-center text-primary-light shrink-0">
            <FileText className="w-4 h-4" />
          </div>
          <div className="min-w-0">
            <p className="text-sm font-medium text-white truncate" title={filename}>
              {filename}
            </p>
            <div className="flex items-center gap-2 text-xs text-zinc-400 mt-0.5">
              <span>{formatBytes(transferred_bytes)} / {formatBytes(total_bytes)}</span>
              {status === 'Transferring' && (
                <>
                  <span>•</span>
                  <span className="text-primary-light font-mono font-medium">
                    {formatBytes(speed_bps)}/s
                  </span>
                </>
              )}
            </div>
          </div>
        </div>

        {/* Status & Action buttons */}
        <div className="flex items-center gap-2 shrink-0">
          {status === 'Transferring' && (
            <div className="flex items-center gap-1.5 text-xs text-indigo-400 font-medium">
              <Loader2 className="w-3.5 h-3.5 animate-spin" />
              <span>传输中</span>
            </div>
          )}
          {status === 'Pending' && (
            <span className="text-xs text-zinc-400">等待中</span>
          )}
          {status === 'Paused' && (
            <span className="text-xs text-amber-400">已暂停</span>
          )}
          {status === 'Completed' && (
            <div className="flex items-center gap-1 text-xs text-emerald-400 font-medium">
              <CheckCircle2 className="w-3.5 h-3.5" />
              <span>已完成</span>
            </div>
          )}
          {status === 'Failed' && (
            <div className="flex items-center gap-1 text-xs text-rose-400 font-medium">
              <AlertCircle className="w-3.5 h-3.5" />
              <span>失败</span>
            </div>
          )}

          {/* Buttons */}
          <div className="flex items-center gap-1 ml-1">
            {status === 'Transferring' && onPause && (
              <button
                onClick={() => onPause(id)}
                className="p-1.5 rounded-md hover:bg-surface text-zinc-400 hover:text-zinc-200 transition-colors"
                title="暂停"
              >
                <Pause className="w-3.5 h-3.5" />
              </button>
            )}
            {status === 'Paused' && onResume && (
              <button
                onClick={() => onResume(id)}
                className="p-1.5 rounded-md hover:bg-surface text-zinc-400 hover:text-zinc-200 transition-colors"
                title="继续"
              >
                <Play className="w-3.5 h-3.5" />
              </button>
            )}
            {status !== 'Completed' && onCancel && (
              <button
                onClick={() => onCancel(id)}
                className="p-1.5 rounded-md hover:bg-rose-500/10 text-zinc-400 hover:text-rose-400 transition-colors"
                title="取消"
              >
                <X className="w-3.5 h-3.5" />
              </button>
            )}
          </div>
        </div>
      </div>

      {/* Progress bar */}
      <div className="w-full bg-surface-dark h-1.5 rounded-full overflow-hidden">
        <div
          className={`h-full transition-all duration-200 rounded-full ${
            status === 'Completed'
              ? 'bg-emerald-500'
              : status === 'Failed'
              ? 'bg-rose-500'
              : status === 'Paused'
              ? 'bg-amber-500'
              : 'bg-primary'
          }`}
          style={{ width: `${percent}%` }}
        />
      </div>
      <div className="flex justify-between items-center text-[10px] text-zinc-500 mt-1">
        <span>{percent}%</span>
        <span>{status}</span>
      </div>
    </div>
  );
};
