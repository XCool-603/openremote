import React, { useState, useEffect, useCallback } from 'react';
import { useTransferStore } from '../stores/transferStore';
import { useConnectionStore } from '../stores/connectionStore';
import { TransferItem } from '../components/TransferItem';
import { safeInvoke } from '../hooks/useTauri';
import {
  Folder,
  File,
  UploadCloud,
  HardDrive,
  RefreshCw,
  FolderOpen,
  Send,
  Trash2,
  Download,
  ArrowUp,
  ClipboardCopy,
  CheckCircle,
} from 'lucide-react';

interface DirEntryInfo {
  name: string;
  path: string;
  is_dir: boolean;
  size_bytes: number;
  modified: string;
}

interface DirListingResult {
  current_path: string;
  parent_path: string | null;
  entries: DirEntryInfo[];
  error: string | null;
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return '0 B';
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(2)} GB`;
}

export const FileTransfer: React.FC = () => {
  const { isConnected, peerName } = useConnectionStore();
  const { transfers, addTransfer, updateProgress, updateStatus, removeTransfer, clearCompleted } =
    useTransferStore();

  const [selectedLocal, setSelectedLocal] = useState<string | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [filePathInput, setFilePathInput] = useState('');
  const [noticeMsg, setNoticeMsg] = useState<string | null>(null);

  // Real local file listing
  const [localListing, setLocalListing] = useState<DirListingResult>({
    current_path: '',
    parent_path: null,
    entries: [],
    error: null,
  });
  const [loadingLocal, setLoadingLocal] = useState(false);

  // Real remote file listing
  const [remoteListing, setRemoteListing] = useState<DirListingResult>({
    current_path: '',
    parent_path: null,
    entries: [],
    error: null,
  });
  const [loadingRemote, setLoadingRemote] = useState(false);

  const loadLocalDir = useCallback(async (targetPath?: string) => {
    setLoadingLocal(true);
    try {
      const res = await safeInvoke<DirListingResult>('list_local_files', {
        path: targetPath ?? null,
      });
      if (res) {
        setLocalListing(res);
      }
    } catch (err) {
      setLocalListing((prev) => ({
        ...prev,
        error: `读取本地目录失败: ${err}`,
      }));
    } finally {
      setLoadingLocal(false);
    }
  }, []);

  const loadRemoteDir = useCallback(async (targetPath?: string) => {
    setLoadingRemote(true);
    try {
      const res = await safeInvoke<DirListingResult>('list_remote_files', {
        path: targetPath ?? null,
      });
      if (res) {
        setRemoteListing(res);
      }
    } catch (err) {
      setRemoteListing((prev) => ({
        ...prev,
        error: `读取远程目录失败: ${err}`,
      }));
    } finally {
      setLoadingRemote(false);
    }
  }, []);

  useEffect(() => {
    loadLocalDir();
  }, [loadLocalDir]);

  useEffect(() => {
    if (isConnected) {
      loadRemoteDir();
    } else {
      setRemoteListing({
        current_path: '',
        parent_path: null,
        entries: [],
        error: null,
      });
    }
  }, [isConnected, loadRemoteDir]);

  // Poll transfer progress from backend if active transfers exist
  useEffect(() => {
    const activeTransfers = transfers.filter((t) => t.status === 'Transferring');
    if (activeTransfers.length === 0) return;

    const interval = setInterval(async () => {
      for (const t of activeTransfers) {
        try {
          const res = await safeInvoke<{
            transferred_bytes: number;
            total_bytes: number;
            speed_bps: number;
            status: string;
          }>('get_transfer_progress', { id: t.id });
          if (res) {
            updateProgress(t.id, res.transferred_bytes, res.speed_bps);
            if (res.status === 'Completed' || res.status === 'Failed') {
              updateStatus(t.id, res.status as any);
            }
          }
        } catch {
          // Ignore transient poll errors
        }
      }
    }, 400);

    return () => clearInterval(interval);
  }, [transfers, updateProgress, updateStatus]);

  const handleSendPath = useCallback(
    async (rawPath: string, totalBytesHint = 0) => {
      if (!rawPath.trim()) return;
      const cleanPath = rawPath.trim().replace(/^"|"$/g, '');
      const filename = cleanPath.split(/[/\\]/).pop() || 'file';

      try {
        const transferId = await safeInvoke<string>('send_file', {
          path: cleanPath,
          peerAddress: useConnectionStore.getState().peerAddress || '127.0.0.1:28843',
        });

        if (transferId) {
          addTransfer({
            id: transferId,
            filename,
            total_bytes: totalBytesHint,
            transferred_bytes: 0,
            speed_bps: 0,
            status: 'Transferring',
          });
          setFilePathInput('');
          setNoticeMsg(`正在发送 "${filename}" 到远程设备的下载目录`);
        }
      } catch (err) {
        setNoticeMsg(`发起文件传输失败: ${err}`);
      }
    },
    [addTransfer]
  );

  // Listen for native OS drag-and-drop via Tauri Webview API (provides real filesystem paths!)
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let mounted = true;

    (async () => {
      try {
        const { getCurrentWebview } = await import('@tauri-apps/api/webview');
        const webview = getCurrentWebview();
        const off = await webview.onDragDropEvent((event) => {
          if (!mounted) return;
          if (event.payload.type === 'enter' || event.payload.type === 'over') {
            setIsDragging(true);
          } else if (event.payload.type === 'leave') {
            setIsDragging(false);
          } else if (event.payload.type === 'drop') {
            setIsDragging(false);
            const paths = event.payload.paths || [];
            for (const p of paths) {
              handleSendPath(p);
            }
          }
        });
        if (mounted) {
          unlisten = off;
        } else {
          off();
        }
      } catch {
        // Fallback outside Tauri runtime (browser dev mode)
      }
    })();

    return () => {
      mounted = false;
      if (unlisten) unlisten();
    };
  }, [handleSendPath]);

  const handleDownloadRemoteFile = async (entry: DirEntryInfo) => {
    if (entry.is_dir) return;
    try {
      const saveDir = await safeInvoke<string>('download_remote_file', {
        remotePath: entry.path,
      });
      setNoticeMsg(`已请求下载 "${entry.name}" -> 将保存至 ${saveDir || 'Downloads\\OpenRemote'}`);
    } catch (err) {
      setNoticeMsg(`下载远程文件失败: ${err}`);
    }
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
  };

  return (
    <div className="flex-1 flex flex-col h-full bg-surface-dark overflow-y-auto">
      {/* Top Header */}
      <div className="bg-surface/80 border-b border-surface-lighter/60 px-8 py-5 backdrop-blur shrink-0 flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold text-white">高速文件传输与剪贴板互传</h1>
          <p className="text-xs text-zinc-400 mt-0.5">
            支持两端目录浏览下载、拖拽上传，以及 Ctrl+C / Ctrl+V 跨机直接复制粘贴文件。
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

      <div className="p-8 space-y-5 flex-1">
        {/* Clipboard file sync hint banner */}
        <div className="flex items-center justify-between gap-3 px-4 py-3 rounded-xl bg-primary/10 border border-primary/25 text-xs text-zinc-200">
          <div className="flex items-center gap-2.5">
            <ClipboardCopy className="w-4 h-4 text-primary-light shrink-0" />
            <span>
              <strong className="text-white">剪贴板文件互通已开启：</strong>
              在本地或远程桌面选中文件按 <code className="px-1 py-0.5 rounded bg-surface-dark text-primary-light">Ctrl+C</code>，另一端等待提示完成后直接 <code className="px-1 py-0.5 rounded bg-surface-dark text-primary-light">Ctrl+V</code> 粘贴即可（单批最大 256 MB）。
            </span>
          </div>
          {noticeMsg && (
            <span className="inline-flex items-center gap-1.5 text-emerald-300 shrink-0 font-medium">
              <CheckCircle className="w-3.5 h-3.5" />
              {noticeMsg}
            </span>
          )}
        </div>

        {/* Split View: Local vs Remote */}
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
          {/* Local files */}
          <div className="bg-surface border border-surface-lighter rounded-2xl p-5 flex flex-col h-[360px]">
            <div className="flex items-center justify-between gap-2 pb-3 border-b border-surface-lighter/50 mb-3">
              <div className="flex items-center gap-2 text-sm font-semibold text-white shrink-0">
                <HardDrive className="w-4 h-4 text-primary-light" />
                <span>本机文件</span>
              </div>
              <div className="flex items-center gap-1.5 min-w-0">
                <span
                  className="text-[11px] text-zinc-400 font-mono truncate max-w-[200px]"
                  title={localListing.current_path}
                >
                  {localListing.current_path || '加载中...'}
                </span>
                <button
                  onClick={() => localListing.parent_path && loadLocalDir(localListing.parent_path)}
                  disabled={!localListing.parent_path || loadingLocal}
                  className="p-1 rounded hover:bg-surface-light text-zinc-400 hover:text-white disabled:opacity-30"
                  title="返回上级目录"
                >
                  <ArrowUp className="w-3.5 h-3.5" />
                </button>
                <button
                  onClick={() => loadLocalDir(localListing.current_path || undefined)}
                  disabled={loadingLocal}
                  className="p-1 rounded hover:bg-surface-light text-zinc-400 hover:text-white"
                  title="刷新本地目录"
                >
                  <RefreshCw className={`w-3.5 h-3.5 ${loadingLocal ? 'animate-spin' : ''}`} />
                </button>
              </div>
            </div>

            {localListing.error ? (
              <div className="flex-1 flex items-center justify-center text-xs text-amber-400 p-4 text-center">
                {localListing.error}
              </div>
            ) : (
              <div className="flex-1 overflow-y-auto space-y-1 pr-1">
                {localListing.entries.map((entry) => (
                  <div
                    key={entry.path}
                    onClick={() => setSelectedLocal(entry.path)}
                    onDoubleClick={() => entry.is_dir && loadLocalDir(entry.path)}
                    className={`flex items-center justify-between p-2 rounded-xl text-xs cursor-pointer transition-colors ${
                      selectedLocal === entry.path
                        ? 'bg-primary/20 text-white border border-primary/40'
                        : 'text-zinc-300 hover:bg-surface-light/60'
                    }`}
                  >
                    <div className="flex items-center gap-2.5 truncate">
                      {entry.is_dir ? (
                        <Folder className="w-4 h-4 text-amber-400 shrink-0" />
                      ) : (
                        <File className="w-4 h-4 text-primary-light shrink-0" />
                      )}
                      <span className="truncate" title={entry.path}>
                        {entry.name}
                      </span>
                    </div>
                    <div className="flex items-center gap-2.5 shrink-0 text-zinc-400 font-mono text-[11px]">
                      <span>{entry.is_dir ? '--' : formatBytes(entry.size_bytes)}</span>
                      {entry.is_dir ? (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            loadLocalDir(entry.path);
                          }}
                          className="px-1.5 py-0.5 rounded bg-surface-light hover:bg-surface-lighter text-zinc-300 text-[10px]"
                        >
                          打开
                        </button>
                      ) : (
                        <button
                          onClick={(e) => {
                            e.stopPropagation();
                            handleSendPath(entry.path, entry.size_bytes);
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
                {!loadingLocal && localListing.entries.length === 0 && (
                  <div className="text-center text-xs text-zinc-500 py-8">当前目录为空</div>
                )}
              </div>
            )}

            {/* Direct Path Input */}
            <div className="pt-2.5 border-t border-surface-lighter/50 flex items-center gap-2">
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
          <div className="bg-surface border border-surface-lighter rounded-2xl p-5 flex flex-col h-[360px]">
            <div className="flex items-center justify-between gap-2 pb-3 border-b border-surface-lighter/50 mb-3">
              <div className="flex items-center gap-2 text-sm font-semibold text-white shrink-0">
                <FolderOpen className="w-4 h-4 text-emerald-400" />
                <span>远程文件 ({isConnected ? peerName || '已连接' : '未连接'})</span>
              </div>
              {isConnected && (
                <div className="flex items-center gap-1.5 min-w-0">
                  <span
                    className="text-[11px] text-zinc-400 font-mono truncate max-w-[180px]"
                    title={remoteListing.current_path}
                  >
                    {remoteListing.current_path || '桌面'}
                  </span>
                  <button
                    onClick={() => remoteListing.parent_path && loadRemoteDir(remoteListing.parent_path)}
                    disabled={!remoteListing.parent_path || loadingRemote}
                    className="p-1 rounded hover:bg-surface-light text-zinc-400 hover:text-white disabled:opacity-30"
                    title="返回远程上级目录"
                  >
                    <ArrowUp className="w-3.5 h-3.5" />
                  </button>
                  <button
                    onClick={() => loadRemoteDir(remoteListing.current_path || undefined)}
                    disabled={loadingRemote}
                    className="p-1 rounded hover:bg-surface-light text-zinc-400 hover:text-white"
                    title="刷新远程目录"
                  >
                    <RefreshCw className={`w-3.5 h-3.5 ${loadingRemote ? 'animate-spin' : ''}`} />
                  </button>
                </div>
              )}
            </div>

            {isConnected ? (
              remoteListing.error ? (
                <div className="flex-1 flex items-center justify-center text-xs text-amber-400 p-4 text-center">
                  {remoteListing.error}
                </div>
              ) : (
                <div className="flex-1 overflow-y-auto space-y-1 pr-1">
                  {remoteListing.entries.map((entry) => (
                    <div
                      key={entry.path}
                      onDoubleClick={() => entry.is_dir && loadRemoteDir(entry.path)}
                      className="flex items-center justify-between p-2 rounded-xl text-xs text-zinc-300 hover:bg-surface-light/60 cursor-pointer transition-colors"
                    >
                      <div className="flex items-center gap-2.5 truncate">
                        {entry.is_dir ? (
                          <Folder className="w-4 h-4 text-amber-400 shrink-0" />
                        ) : (
                          <File className="w-4 h-4 text-emerald-400 shrink-0" />
                        )}
                        <span className="truncate" title={entry.path}>
                          {entry.name}
                        </span>
                      </div>
                      <div className="flex items-center gap-2.5 shrink-0 text-zinc-400 font-mono text-[11px]">
                        <span>{entry.is_dir ? '--' : formatBytes(entry.size_bytes)}</span>
                        {entry.is_dir ? (
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              loadRemoteDir(entry.path);
                            }}
                            className="px-1.5 py-0.5 rounded bg-surface-light hover:bg-surface-lighter text-zinc-300 text-[10px]"
                          >
                            打开
                          </button>
                        ) : (
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              handleDownloadRemoteFile(entry);
                            }}
                            className="p-1 rounded bg-emerald-500/20 hover:bg-emerald-500 text-emerald-300 hover:text-white transition-colors"
                            title="下载到本地 Downloads\OpenRemote"
                          >
                            <Download className="w-3 h-3" />
                          </button>
                        )}
                      </div>
                    </div>
                  ))}
                  {!loadingRemote && remoteListing.entries.length === 0 && (
                    <div className="text-center text-xs text-zinc-500 py-8">当前远程目录为空</div>
                  )}
                </div>
              )
            ) : (
              <div className="flex-1 flex flex-col items-center justify-center text-center p-6 text-zinc-500">
                <FolderOpen className="w-10 h-10 mb-2 opacity-30" />
                <p className="text-xs">连接远程设备后即可浏览与下载远程文件</p>
              </div>
            )}
          </div>
        </div>

        {/* Drag and Drop Zone */}
        <div
          onDragOver={handleDragOver}
          onDragLeave={handleDragLeave}
          onDrop={handleDrop}
          className={`border-2 border-dashed rounded-2xl p-7 flex flex-col items-center justify-center text-center transition-all ${
            isDragging
              ? 'border-primary bg-primary/10 scale-[0.99]'
              : 'border-surface-lighter hover:border-zinc-500 bg-surface/30'
          }`}
        >
          <div className="w-11 h-11 rounded-2xl bg-surface-light flex items-center justify-center text-primary-light mb-2.5 shadow-md">
            <UploadCloud className="w-5 h-5" />
          </div>
          <h3 className="text-sm font-semibold text-white mb-1">
            将电脑文件拖拽至此处直接发送给远程端
          </h3>
          <p className="text-xs text-zinc-400">
            支持单文件与多文件同时拖拽上传（自动保存至远端 Downloads\OpenRemote）
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
