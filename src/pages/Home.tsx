import React, { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { DeviceCard } from '../components/DeviceCard';
import { useConnectionStore } from '../stores/connectionStore';
import { safeInvoke } from '../hooks/useTauri';
import { PeerDevice, DeviceInfo, HostControlStatus } from '../types';
import {
  Plus,
  RefreshCw,
  Copy,
  Check,
  Laptop,
  Radio,
  Search,
  ExternalLink,
  ShieldAlert,
  PowerOff,
} from 'lucide-react';

const SAVED_DEVICES_KEY = 'openremote_saved_devices';

export const Home: React.FC = () => {
  const navigate = useNavigate();
  const {
    isConnected,
    peerAddress,
    connect,
    disconnect,
    setDeviceInfo,
    deviceInfo,
    authUser,
    setAuthModalOpen,
  } = useConnectionStore();

  const [devices, setDevices] = useState<PeerDevice[]>(() => {
    try {
      const saved = localStorage.getItem(SAVED_DEVICES_KEY);
      if (saved) {
        return JSON.parse(saved);
      }
    } catch (e) {
      console.warn('Failed to parse saved devices:', e);
    }
    return [];
  });
  const [searchQuery, setSearchQuery] = useState('');
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [connectAddress, setConnectAddress] = useState('');
  const [copied, setCopied] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [serviceBusy, setServiceBusy] = useState(false);
  const [hostControl, setHostControl] = useState<HostControlStatus | null>(null);
  const [statusNotice, setStatusNotice] = useState<string | null>(null);

  const saveDeviceRecord = (address: string, name?: string) => {
    setDevices((prev) => {
      const existing = prev.find((d) => d.address === address);
      let updated: PeerDevice[];
      if (existing) {
        updated = prev.map((d) =>
          d.address === address
            ? { ...d, last_seen: '刚刚', name: name || d.name, status: 'online' as const }
            : d
        );
      } else {
        const newDevice: PeerDevice = {
          id: `dev_${Date.now()}`,
          name: name || `远程设备 (${address})`,
          address,
          status: 'online',
          last_seen: '刚刚',
        };
        updated = [newDevice, ...prev];
      }
      try {
        localStorage.setItem(SAVED_DEVICES_KEY, JSON.stringify(updated));
      } catch (e) {
        console.warn('Failed to save devices to storage:', e);
      }
      return updated;
    });
  };

  // Load device info on mount
  useEffect(() => {
    async function fetchDeviceInfo() {
      try {
        const info = await safeInvoke<DeviceInfo>('get_device_info');
        if (info) {
          setDeviceInfo(info);
        }
      } catch {
        // Fallback info for dev preview
        if (!deviceInfo) {
          setDeviceInfo({
            device_id: 'DEV-882190',
            device_code: '739-281-904',
            access_password: '391024',
            device_name: '本地主机 (OpenRemote-PC)',
            local_ip: '192.168.1.100',
            listen_port: 28843,
          });
        }
      }
    }
    fetchDeviceInfo();
  }, [setDeviceInfo]);

  // Poll incoming remote control status every 1s so the controlled host PC ALWAYS displays live state!
  useEffect(() => {
    let cancelled = false;
    const poll = async () => {
      try {
        const st = await safeInvoke<HostControlStatus>('get_host_control_status');
        if (!cancelled) {
          setHostControl(st?.active ? st : null);
        }
      } catch {
        // ignore in web preview
      }
    };
    poll();
    const timer = window.setInterval(poll, 1000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  const handleCopyId = () => {
    const code = deviceInfo?.device_code || '739-281-904';
    navigator.clipboard.writeText(code);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleConnect = async (_id: string, address: string, name: string) => {
    setConnecting(true);
    setStatusNotice(`正在连接到 ${name} (${address})...`);
    try {
      const res = await safeInvoke<{ mode: string; target: string; stream_port: number }>('connect_to_device', { address });
      saveDeviceRecord(address, name);
      connect(address, name, res?.mode, res?.target, res?.stream_port);
      setStatusNotice(null);
      navigate('/remote');
    } catch (err) {
      console.error('Failed to connect:', err);
      const errStr = String(err || '');
      if (errStr.includes('请先登录账号')) {
        setAuthModalOpen(true);
      }
      setStatusNotice(`连接到 ${name} 失败: ${err || '请确认目标主机已开启且在同一网络'}`);
    } finally {
      setConnecting(false);
    }
  };

  const handleDirectConnect = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!connectAddress.trim()) return;

    setConnecting(true);
    setStatusNotice(`正在连接目标设备 ${connectAddress}...`);
    try {
      const res = await safeInvoke<{ mode: string; target: string; stream_port: number }>('connect_to_device', {
        address: connectAddress,
      });
      const deviceTitle = `远程电脑 (${connectAddress})`;
      saveDeviceRecord(connectAddress, deviceTitle);
      connect(connectAddress, deviceTitle, res?.mode, res?.target, res?.stream_port);
      setIsModalOpen(false);
      setStatusNotice(null);
      navigate('/remote');
    } catch (err) {
      console.error('Direct connect failed:', err);
      const errStr = String(err || '');
      if (errStr.includes('请先登录账号')) {
        setAuthModalOpen(true);
      }
      setStatusNotice(`连接失败: ${err || '请确认 9 位代码或 IP 地址正确'}`);
    } finally {
      setConnecting(false);
    }
  };

  const handleWakeUp = async (mac: string) => {
    setStatusNotice(`已向 ${mac} 发送网络唤醒 (Wake-on-LAN) 数据包...`);
    try {
      await safeInvoke('wake_device', { mac_address: mac });
    } catch (err) {
      console.warn('wake_device invoke error:', err);
    }
    setTimeout(() => {
      setStatusNotice(null);
    }, 3500);
  };

  const filteredDevices = devices.filter(
    (d) =>
      d.name.toLowerCase().includes(searchQuery.toLowerCase()) ||
      d.address.toLowerCase().includes(searchQuery.toLowerCase())
  );

  return (
    <div className="flex-1 flex flex-col h-full bg-surface-dark overflow-y-auto">
      {/* Top Banner / UU Remote Dual-Panel Layout */}
      <div className="bg-surface/90 border-b border-surface-lighter/60 px-8 py-6 backdrop-blur">
        <div className="grid grid-cols-1 lg:grid-cols-2 gap-6 max-w-6xl">
          {/* Left Panel: Allow this device to be controlled */}
          <div className="bg-surface-light/80 border border-surface-lighter rounded-2xl p-5 relative overflow-hidden shadow-sm">
            <div className="flex items-center justify-between mb-3">
              <div className="flex items-center gap-2">
                <div className="w-8 h-8 rounded-xl bg-primary/20 flex items-center justify-center text-primary-light">
                  <Laptop className="w-4 h-4" />
                </div>
                <span className="font-bold text-white text-sm">此电脑 (被控端)</span>
              </div>
              {hostControl?.active ? (
                <span className="flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-[11px] font-semibold bg-emerald-500/20 text-emerald-300 border border-emerald-500/40 animate-pulse">
                  <ShieldAlert className="w-3 h-3" /> 正在被远程控制中 ({hostControl.transport_mode})
                </span>
              ) : (
                <span className="flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-[11px] font-medium bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
                  <Radio className="w-3 h-3 animate-pulse" /> 串流引擎在线
                </span>
              )}
            </div>

            {hostControl?.active && (
              <div className="mb-3 p-3 rounded-xl bg-emerald-500/15 border border-emerald-500/40 flex items-center justify-between gap-3 text-xs">
                <div className="min-w-0">
                  <div className="font-semibold text-emerald-300 truncate">
                    控制端已接入: {hostControl.controller_id || '远程控制端'}
                  </div>
                  <div className="text-[11px] text-emerald-200/80 font-mono mt-0.5 truncate">
                    通道: {hostControl.transport_mode} · 来源: {hostControl.remote_addr}
                  </div>
                </div>
                <button
                  type="button"
                  onClick={async () => {
                    try {
                      await safeInvoke('stop_incoming_control');
                      setHostControl(null);
                      setStatusNotice('已主动断开当前远程控制连接');
                    } catch (err) {
                      setStatusNotice(`断开失败: ${err}`);
                    }
                  }}
                  className="inline-flex items-center gap-1 px-2.5 py-1.5 rounded-lg bg-rose-500/20 hover:bg-rose-500/30 border border-rose-500/40 text-rose-200 font-semibold whitespace-nowrap shrink-0 transition-colors"
                >
                  <PowerOff className="w-3.5 h-3.5" />
                  <span>断开控制</span>
                </button>
              </div>
            )}

            <div className="grid grid-cols-2 gap-4 mt-3">
              <div className="bg-surface-dark/80 p-3.5 rounded-xl border border-surface-lighter/60">
                <span className="text-[11px] text-zinc-400 font-medium block mb-1">本机设备代码</span>
                <div className="flex items-center justify-between">
                  <span className="font-mono text-xl font-bold tracking-wider text-white">
                    {deviceInfo?.device_code || '739-281-904'}
                  </span>
                  <button
                    onClick={handleCopyId}
                    className="p-1.5 hover:bg-surface-lighter rounded-lg text-primary-light hover:text-white transition-colors"
                    title="复制设备代码"
                  >
                    {copied ? <Check className="w-4 h-4 text-emerald-400" /> : <Copy className="w-4 h-4" />}
                  </button>
                </div>
              </div>

              <div className="bg-surface-dark/80 p-3.5 rounded-xl border border-surface-lighter/60">
                <span className="text-[11px] text-zinc-400 font-medium block mb-1">本机访问密码</span>
                <div className="flex items-center justify-between">
                  <span className="font-mono text-xl font-bold tracking-wider text-primary-light">
                    {deviceInfo?.access_password || '391024'}
                  </span>
                  <span className="text-[10px] text-zinc-500 font-medium">免打扰保护</span>
                </div>
              </div>
            </div>

            <div className="mt-3 flex flex-wrap items-center justify-between text-[11px] text-zinc-400 font-mono gap-2 pt-2 border-t border-surface-lighter/40">
              <span>内网: {deviceInfo?.local_ip || '127.0.0.1'}:{deviceInfo?.listen_port || 28843}</span>
              {deviceInfo?.public_ipv6 ? (
                <span className="text-emerald-400 bg-emerald-500/10 px-2 py-0.5 rounded border border-emerald-500/20 truncate max-w-[260px]" title={`公网 IPv6 全球直连地址: [${deviceInfo.public_ipv6}]:${deviceInfo?.listen_port || 28843}`}>
                  🌐 IPv6 公网直连就绪
                </span>
              ) : (
                <span className="text-zinc-500">双栈中继直连</span>
              )}
            </div>

            {/* Windows LocalSystem Service Control (enables Lock Screen, UAC, pre-login & zero os error 5) */}
            <div className="mt-3 p-2.5 rounded-xl bg-surface-dark/90 border border-surface-lighter/70 flex items-center justify-between gap-2 text-[11px]">
              {deviceInfo?.service_running ? (
                <>
                  <div className="text-emerald-400 leading-snug">
                    <span className="font-semibold">✓ SYSTEM 系统服务运行中</span>
                    <span className="text-zinc-400 ml-1.5">已支持锁屏解锁、UAC 弹窗与开机直连</span>
                  </div>
                  <div className="flex items-center gap-1.5 shrink-0">
                    <button
                      type="button"
                      disabled={serviceBusy}
                      onClick={async () => {
                        setServiceBusy(true);
                        setStatusNotice('正在更新并重启 SYSTEM 系统服务...');
                        try {
                          await safeInvoke('install_system_service');
                          const refreshed = await safeInvoke<DeviceInfo>('get_device_info');
                          if (refreshed) setDeviceInfo(refreshed);
                          setStatusNotice('✓ SYSTEM 系统服务已更新并启动（已同步最新版内核）');
                        } catch (err) {
                          setStatusNotice(`更新系统服务失败: ${err}`);
                        } finally {
                          setServiceBusy(false);
                        }
                      }}
                      className="px-2 py-1 rounded-lg bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-zinc-300 whitespace-nowrap disabled:opacity-50 transition-colors"
                    >
                      {serviceBusy ? '处理中...' : '更新服务'}
                    </button>
                    <button
                      type="button"
                      disabled={serviceBusy}
                      onClick={async () => {
                        setServiceBusy(true);
                        try {
                          await safeInvoke('uninstall_system_service');
                          const refreshed = await safeInvoke<DeviceInfo>('get_device_info');
                          if (refreshed) setDeviceInfo(refreshed);
                          setStatusNotice('已停止并卸载 OpenRemote SYSTEM 系统服务');
                        } catch (err) {
                          setStatusNotice(`卸载系统服务失败: ${err}`);
                        } finally {
                          setServiceBusy(false);
                        }
                      }}
                      className="px-2 py-1 rounded-lg bg-rose-500/10 hover:bg-rose-500/20 border border-rose-500/30 text-rose-300 whitespace-nowrap disabled:opacity-50 transition-colors"
                    >
                      卸载
                    </button>
                  </div>
                </>
              ) : (
                <>
                  <div className="text-zinc-300 leading-snug">
                    <span className="text-amber-300 font-semibold">未启用 SYSTEM 系统服务</span>
                    <span className="text-zinc-400 ml-1.5">安装后支持锁屏输密码、UAC 点击与开机免登录直连</span>
                  </div>
                  <button
                    type="button"
                    disabled={serviceBusy}
                    onClick={async () => {
                      setServiceBusy(true);
                      setStatusNotice('正在安装并启动 OpenRemote LocalSystem 系统服务...');
                      try {
                        await safeInvoke('install_system_service');
                        const refreshed = await safeInvoke<DeviceInfo>('get_device_info');
                        if (refreshed) setDeviceInfo(refreshed);
                        setStatusNotice('✓ SYSTEM 系统服务已安装并启动！现在锁屏、UAC 和开机未登录均可直接连接控制。');
                      } catch (err) {
                        setStatusNotice(`安装 SYSTEM 服务失败: ${err}`);
                      } finally {
                        setServiceBusy(false);
                      }
                    }}
                    className="px-2.5 py-1.5 rounded-lg bg-primary hover:bg-primary-dark text-white font-semibold whitespace-nowrap shrink-0 disabled:opacity-50 transition-colors shadow-sm"
                  >
                    {serviceBusy ? '安装中...' : '一键安装 SYSTEM 服务'}
                  </button>
                </>
              )}
            </div>

            {deviceInfo?.is_rdp_session && (
              <div className="mt-3 p-2.5 rounded-xl bg-amber-500/10 border border-amber-500/30 flex items-center justify-between gap-2 text-[11px]">
                <span className="text-amber-300 leading-snug">
                  检测到当前处于 Windows 远程桌面 (RDP #{deviceInfo.session_id ?? 0})，关闭或最小化 RDP 会导致黑屏和键鼠失效。
                </span>
                <button
                  type="button"
                  onClick={async () => {
                    try {
                      const msg = await safeInvoke<string>('detach_local_rdp');
                      setStatusNotice(msg);
                    } catch (err) {
                      setStatusNotice(`切回本地桌面失败: ${err}`);
                    }
                  }}
                  className="px-2.5 py-1.5 rounded-lg bg-amber-500 hover:bg-amber-400 text-zinc-950 font-semibold whitespace-nowrap shrink-0 transition-colors"
                >
                  一键切回本地桌面
                </button>
              </div>
            )}
          </div>

          {/* Right Panel: Connect to remote device */}
          <div className="bg-surface-light/80 border border-surface-lighter rounded-2xl p-5 shadow-sm flex flex-col justify-between">
            <div>
              <div className="flex items-center justify-between mb-3">
                <span className="font-bold text-white text-sm">控制远程电脑 (控制端)</span>
                <span className="text-[11px] text-zinc-400">支持 9 位代码或 IP</span>
              </div>

              <form onSubmit={handleDirectConnect} className="space-y-3 mt-3">
                <div className="flex gap-2">
                  <div className="relative flex-1">
                    <input
                      type="text"
                      required
                      placeholder="输入伙伴的 9位设备代码 或 IP (如 127.0.0.1)"
                      value={connectAddress}
                      onChange={(e) => setConnectAddress(e.target.value)}
                      className="w-full bg-surface-dark border border-surface-lighter rounded-xl px-4 py-2.5 text-sm text-white placeholder-zinc-500 focus:outline-none focus:border-primary font-mono tracking-wide"
                    />
                  </div>
                  <button
                    type="submit"
                    disabled={connecting || !connectAddress.trim()}
                    className="inline-flex items-center gap-2 px-5 py-2.5 rounded-xl bg-primary hover:bg-primary-dark disabled:opacity-50 text-white text-sm font-semibold shadow-lg shadow-primary/25 transition-all shrink-0"
                  >
                    <span>{connecting ? '连接中...' : '一键连接'}</span>
                    <ExternalLink className="w-4 h-4" />
                  </button>
                </div>
              </form>
            </div>

            <div className="mt-3 pt-2 border-t border-surface-lighter/40 text-[11px] text-zinc-400 flex items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <span className="w-1.5 h-1.5 rounded-full bg-primary-light"></span>
                <span>双向剪贴板同步 (Ctrl+C/V) 已默认启用</span>
              </div>
              {authUser ? (
                <span className="text-emerald-400 font-medium">
                  ✓ 中继账号已登录 ({authUser})
                </span>
              ) : (
                <button
                  type="button"
                  onClick={() => setAuthModalOpen(true)}
                  className="text-amber-400 hover:underline font-medium"
                >
                  ⚠️ 跨网中继需先登录账号 (点击登录)
                </button>
              )}
            </div>
          </div>
        </div>

        {statusNotice && (
          <div className="mt-4 px-4 py-2 rounded-lg bg-primary/15 border border-primary/30 text-xs text-primary-light flex items-center justify-between animate-fade-in max-w-6xl">
            <span>{statusNotice}</span>
            <button onClick={() => setStatusNotice(null)} className="text-zinc-400 hover:text-white">
              ✕
            </button>
          </div>
        )}
      </div>

      {/* Main Content Area */}
      <div className="p-8 flex-1">
        {/* Search and Filter toolbar */}
        <div className="flex flex-wrap items-center justify-between gap-4 mb-6">
          <div className="relative flex-1 max-w-md">
            <Search className="w-4 h-4 text-zinc-400 absolute left-3.5 top-1/2 -translate-y-1/2" />
            <input
              type="text"
              placeholder="按名称或 IP 搜索设备..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full bg-surface-light border border-surface-lighter rounded-xl pl-10 pr-4 py-2 text-sm text-white placeholder-zinc-500 focus:outline-none focus:border-primary transition-all"
            />
          </div>

          <div className="flex items-center gap-2 text-xs text-zinc-400">
            <span>共 {devices.length} 台设备 ({devices.filter(d => d.status === 'online').length} 台在线)</span>
            <button
              onClick={() => {
                setStatusNotice('设备列表已刷新');
                setTimeout(() => setStatusNotice(null), 2000);
              }}
              className="p-2 rounded-lg bg-surface-light hover:bg-surface-lighter border border-surface-lighter text-zinc-300 transition-colors"
              title="刷新列表"
            >
              <RefreshCw className="w-4 h-4" />
            </button>
          </div>
        </div>

        {/* Device Cards Grid */}
        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-5">
          {filteredDevices.map((device) => (
            <DeviceCard
              key={device.id}
              id={device.id}
              name={device.name}
              address={device.address}
              status={device.status}
              lastSeen={device.last_seen}
              macAddress={device.mac_address}
              isCurrentConnection={isConnected && peerAddress === device.address}
              onConnect={handleConnect}
              onDisconnect={async () => {
                try {
                  await safeInvoke('disconnect');
                } catch (err) {
                  console.warn('Backend disconnect error:', err);
                }
                disconnect();
              }}
              onWakeUp={handleWakeUp}
            />
          ))}
        </div>

        {filteredDevices.length === 0 && (
          <div className="text-center py-16 text-zinc-500">
            <Search className="w-10 h-10 mx-auto mb-3 opacity-40" />
            <p className="text-sm">没有匹配到相关设备</p>
          </div>
        )}
      </div>

      {/* Direct Connect Modal */}
      {isModalOpen && (
        <div className="fixed inset-0 z-50 bg-black/60 backdrop-blur-sm flex items-center justify-center p-4">
          <div className="bg-surface border border-surface-lighter rounded-2xl w-full max-w-md p-6 shadow-2xl animate-in fade-in zoom-in-95 duration-150">
            <h2 className="text-lg font-bold text-white mb-1.5">连接远端设备</h2>
            <p className="text-xs text-zinc-400 mb-5">
              输入远程电脑的 IP 地址及端口号（或识别码）发起远程桌面连接。
            </p>

            <form onSubmit={handleDirectConnect} className="space-y-4">
              <div>
                <label className="block text-xs font-semibold text-zinc-300 mb-1.5">
                  设备地址 / IP:Port
                </label>
                <input
                  type="text"
                  required
                  autoFocus
                  placeholder="例如: 192.168.1.150:8443 或 设备ID"
                  value={connectAddress}
                  onChange={(e) => setConnectAddress(e.target.value)}
                  className="w-full bg-surface-dark border border-surface-lighter rounded-xl px-3.5 py-2.5 text-sm text-white placeholder-zinc-500 focus:outline-none focus:border-primary font-mono"
                />
              </div>

              <div className="flex items-center justify-end gap-3 pt-3">
                <button
                  type="button"
                  onClick={() => setIsModalOpen(false)}
                  className="px-4 py-2 rounded-xl text-xs font-medium text-zinc-400 hover:text-white hover:bg-surface-light transition-colors"
                >
                  取消
                </button>
                <button
                  type="submit"
                  disabled={connecting || !connectAddress.trim()}
                  className="inline-flex items-center gap-2 px-5 py-2 rounded-xl bg-primary hover:bg-primary-dark disabled:opacity-50 text-white text-xs font-semibold shadow-lg shadow-primary/25 transition-all"
                >
                  {connecting ? '正在建立连接...' : '立即连接'}
                  <ExternalLink className="w-3.5 h-3.5" />
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </div>
  );
};
