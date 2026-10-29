import React, { useState, useEffect } from 'react';
import { useTransferStore } from '../stores/transferStore';
import { useConnectionStore } from '../stores/connectionStore';
import { TransferItem } from '../components/TransferItem';
import { safeInvoke } from '../hooks/useTauri';
import { TransferProgress } from '../types';
import {
  Folder,
  File,
  UploadCloud,
  ArrowRight,
  HardDrive,
  RefreshCw,
  FolderOpen,
  Send,
  Trash2,
  CheckCircle,
} from 'lucide-react';

interface FileEntry {
  name: string;
  size: string;
  bytes: number;
  type: 'file' | 'folder';
  modified: string;
}

const LOCAL_FILES: FileEntry[] = [
  { name: 'OpenRemote_Setup_v0.1.0.exe', size: '48.5 MB', bytes: 50855936, type: 'file', modified: '2026-10-12 10:15' },
  { name: 'GameCaptures_4K60.mp4', size: '820.0 MB', bytes: 859832320, type: 'file', modified: '2026-10-11 18:20' },
  { name: 'Documents', size: '--', bytes: 0, type: 'folder', modified: '2026-10-10 14:00' },
  { name: 'config_backup.json', size: '12.4 KB', bytes: 12697, type: 'file', modified: '2026-10-09 09:30' },
];

const REMOTE_FILES: FileEntry[] = [
  { name: 'Downloads', size: '--', bytes: 0, type: 'folder', modified: '2026-10-12 08:00' },
  { name: 'NVIDIA_DisplayDriver.zip', size: '640.2 MB', bytes: 671291801, type: 'file', modified: '2026-10-08 19:40' },
  { name: 'workspace.code-workspace', size: '2.1 KB', bytes: 2150, type: 'file', modified: '2026-10-07 11:12' },
];

export const FileTransfer: React.FC = () => {
  const { isConnected, peerName } = useConnectionStore();
  const { transfers, addTransfer, updateProgress, updateStatus, removeTransfer, clearCompleted } =
    useTransferStore();

  const [selectedLocal, setSelectedLocal] = useState<string | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [filePathInput, setFilePathInput] = useState('');

  // Poll transfer progress from backend if active transfers exist
  useEffect(() => {
    const activeTransfers = transfers.filter((t) => t.status === 'Transferring');
    if (activeTransfers.length === 0) return;

    const interval = setInterval(async () => {
      for (const t of activeTransfers) {
        try {
          const res = await safeInvoke<{ transferred_bytes: number; total_bytes: number; speed_bps: number; status: string }>(
            'get_transfer_progress',
            { id: t.id }
          );
          if (res) {
            updateProgress(t.id, res.transferred_bytes, res.speed_bps);
            if (res.status === 'Completed' || res.status === 'Failed') {
              updateStatus(t.id, res.status as any);
            }
          }
        } catch {
          // Simulation fallback for browser preview
          const delta = Math.min(t.total_bytes - t.transferred_bytes, Math.floor(Math.random() * 8000000) + 4000000);
          const nextBytes = t.transferred_bytes + delta;
          const speed = delta * 2;
          updateProgress(t.id, nextBytes, speed);
        }
      }
    }, 500);

    return () => clearInterval(interval);
  }, [transfers, updateProgress, updateStatus]);

  const handleSendPath = async (rawPath: string) => {
    if (!rawPath.trim()) return;
    const cleanPath = rawPath.trim().replace(/^"|"$/g, '');
    const filename = cleanPath.split(/[/\\]/).pop() || 'file';

    try {
      const transferId = await safeInvoke<string>('send_file', {
        path: cleanPath,
        peer_address: (useConnectionStore.getState().peerAddress) || '127.0.0.1:28843',
      });

      if (transferId) {
        addTransfer({
          id: transferId,
          filename,
          total_bytes: 0,
          transferred_bytes: 0,
          speed_bps: 0,
          status: 'Transferring',
        });
        setFilePathInput('');
      }
    } catch (err) {
      alert(`发起文件传输失败: ${err}`);
    }
  };

  const handleSendFile = async (entry: FileEntry) => {
    if (entry.type === 'folder') return;
    await handleSendPath(entry.name);
  };

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(true);
  };

  const handleDragLeave = () => {
    setIsDragging(false);
  };

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setIsDragging(false);
    if (e.dataTransfer.files && e.dataTransfer.files.length > 0) {
      const droppedFile = e.dataTransfer.files[0];
      const newTransfer: TransferProgress = {
        id: `trans-${Date.now()}`,
        filename: droppedFile.name,
        total_bytes: droppedFile.size || 10485760,
        transferred_bytes: 0,
        speed_bps: 0,
        status: 'Transferring',
      };
      addTransfer(newTransfer);
    }
  };

  return (
    <div className="flex-1 flex flex-col h-full bg-surface-dark overflow-y-auto">
      {/* Top Header */}
      <div className="bg-surface/80 border-b border-surface-lighter/60 px-8 py-5 backdrop-blur shrink-0 flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold text-white">高速文件传输</h1>
          <p className="text-xs text-zinc-400 mt-0.5">
            点对点加密通道传输，无中间服务器中转，速度高达局域网与宽带上限。
          </p>
        </div>

        <div className="flex items-center gap-2">
          <button
            onClick={() => safeInvoke('open_download_dir')}
            className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium text-primary-light bg-primary/10 hover:bg-primary/20 border border-primary/30 transition-colors"
            title="在系统资源管理器中打开接收文件保存目录"
          >
            <FolderOpen className="w-3.5 h-3.5" />
            <span>打开接收目录</span>
          </button>
          {transfers.length > 0 && (
            <button
              onClick={clearCompleted}
              className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs text-zinc-400 hover:text-white bg-surface-light border border-surface-lighter hover:border-surface-lighter/80 transition-colors"
            >
              <Trash2 className="w-3.5 h-3.5" />
              清空已完成
            </button>
          )}
        </div>
      </div>

      <div className="p-8 space-y-6 flex-1">
        {/* Split View: Local vs Remote */}
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          {/* Local files */}
          <div className="bg-surface border border-surface-lighter rounded-2xl p-5 flex flex-col h-[340px]">
            <div className="flex items-center justify-between pb-3 border-b border-surface-lighter/50 mb-3">
              <div className="flex items-center gap-2 text-sm font-semibold text-white">
                <HardDrive className="w-4 h-4 text-primary-light" />
                <span>本机文件 (Local)</span>
              </div>
              <span className="text-xs text-zinc-500 font-mono">C:\OpenRemote\Files</span>
            </div>

            <div className="flex-1 overflow-y-auto space-y-1 pr-1">
              {LOCAL_FILES.map((file) => (
                <div
                  key={file.name}
                  onClick={() => setSelectedLocal(file.name)}
                  className={`flex items-center justify-between p-2.5 rounded-xl text-xs cursor-pointer transition-colors ${
                    selectedLocal === file.name
                      ? 'bg-primary/20 text-white border border-primary/40'
                      : 'text-zinc-300 hover:bg-surface-light/60'
                  }`}
                >
                  <div className="flex items-center gap-2.5 truncate">
                    {file.type === 'folder' ? (
                      <Folder className="w-4 h-4 text-amber-400 shrink-0" />
                    ) : (
                      <File className="w-4 h-4 text-primary-light shrink-0" />
                    )}
                    <span className="truncate">{file.name}</span>
                  </div>
                  <div className="flex items-center gap-3 shrink-0 text-zinc-400 font-mono text-[11px]">
                    <span>{file.size}</span>
                    {file.type === 'file' && (
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          handleSendFile(file);
                        }}
                        className="p-1 rounded bg-primary/20 hover:bg-primary text-primary-light hover:text-white transition-colors"
                        title="发送至远端"
                      >
                        <Send className="w-3 h-3" />
                      </button>
                    )}
                  </div>
                </div>
              ))}
            </div>

            {/* Direct Path Input */}
            <div className="pt-2 border-t border-surface-lighter/50 flex items-center gap-2">
              <input
                type="text"
                placeholder="输入本地文件绝对路径 (如 C:\test.zip)..."
                value={filePathInput}
                onChange={(e) => setFilePathInput(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && handleSendPath(filePathInput)}
                className="flex-1 bg-surface-dark border border-surface-lighter/80 rounded-lg px-2.5 py-1 text-xs text-white placeholder-zinc-500 focus:outline-none focus:border-primary"
              />
              <button
                onClick={() => handleSendPath(filePathInput)}
                disabled={!filePathInput.trim()}
                className="px-2.5 py-1 rounded-lg bg-primary hover:bg-primary-dark disabled:opacity-40 text-white text-xs font-medium flex items-center gap-1 transition-colors"
              >
                <Send className="w-3 h-3" />
                <span>发送</span>
              </button>
            </div>
          </div>

          {/* Remote files */}
          <div className="bg-surface border border-surface-lighter rounded-2xl p-5 flex flex-col h-[340px]">
            <div className="flex items-center justify-between pb-3 border-b border-surface-lighter/50 mb-3">
              <div className="flex items-center gap-2 text-sm font-semibold text-white">
                <FolderOpen className="w-4 h-4 text-emerald-400" />
                <span>远程设备文件 ({isConnected ? peerName || '已连接' : '未连接'})</span>
              </div>
              <span className="text-xs text-zinc-500 font-mono">/home/user/downloads</span>
            </div>

            {isConnected ? (
              <div className="flex-1 overflow-y-auto space-y-1 pr-1">
                {REMOTE_FILES.map((file) => (
                  <div
                    key={file.name}
                    className="flex items-center justify-between p-2.5 rounded-xl text-xs text-zinc-300 hover:bg-surface-light/60 transition-colors"
                  >
                    <div className="flex items-center gap-2.5 truncate">
                      {file.type === 'folder' ? (
                        <Folder className="w-4 h-4 text-amber-400 shrink-0" />
                      ) : (
                        <File className="w-4 h-4 text-emerald-400 shrink-0" />
                      )}
                      <span className="truncate">{file.name}</span>
                    </div>
                    <span className="text-zinc-400 font-mono text-[11px] shrink-0">{file.size}</span>
                  </div>
                ))}
              </div>
            ) : (
              <div className="flex-1 flex flex-col items-center justify-center text-center p-6 text-zinc-500">
                <FolderOpen className="w-10 h-10 mb-2 opacity-30" />
                <p className="text-xs">连接远程设备后即可浏览与同步文件</p>
              </div>
            )}
          </div>
        </div>

        {/* Drag and Drop Zone */}
        <div
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
          onDrop={handleDrop}
          className={`border-2 border-dashed rounded-2xl p-8 flex flex-col items-center justify-center text-center transition-all cursor-pointer ${
            isDragging
              ? 'border-primary bg-primary/10 scale-[0.99]'
              : 'border-surface-lighter hover:border-zinc-500 bg-surface/30'
          }`}
        >
          <div className="w-12 h-12 rounded-2xl bg-surface-light flex items-center justify-center text-primary-light mb-3 shadow-md">
            <UploadCloud className="w-6 h-6" />
          </div>
          <h3 className="text-sm font-semibold text-white mb-1">
            拖拽文件到此处立即传输
          </h3>
          <p className="text-xs text-zinc-400">
            支持拖拽单文件或多文件，系统将以 P2P 加密直连流分块发送
          </p>
        </div>

        {/* Transfer Queue Section */}
        <div>
          <div className="flex items-center justify-between mb-3">
            <h2 className="text-sm font-bold text-white flex items-center gap-2">
              <span>传输任务队列</span>
              <span className="px-2 py-0.5 rounded-full bg-surface-light text-zinc-400 text-xs font-mono font-normal">
                {transfers.length}
              </span>
            </h2>
          </div>

          {transfers.length > 0 ? (
            <div className="space-y-2.5">
              {transfers.map((t) => (
                <TransferItem
                  key={t.id}
                  transfer={t}
                  onPause={(id) => updateStatus(id, 'Paused')}
                  onResume={(id) => updateStatus(id, 'Transferring')}
                  onCancel={(id) => removeTransfer(id)}
                />
              ))}
            </div>
          ) : (
            <div className="p-6 bg-surface/40 border border-surface-lighter/40 rounded-xl text-center text-xs text-zinc-500">
              暂无进行中的传输任务
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
