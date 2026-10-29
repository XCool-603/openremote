import React, { useState, useEffect } from 'react';
import { useNavigate } from 'react-router-dom';
import { DeviceCard } from '../components/DeviceCard';
import { useConnectionStore } from '../stores/connectionStore';
import { safeInvoke } from '../hooks/useTauri';
import { PeerDevice, DeviceInfo } from '../types';
import {
  Plus,
  RefreshCw,
  Copy,
  Check,
  Laptop,
  Radio,
  Search,
  ExternalLink,
} from 'lucide-react';

const INITIAL_DEVICES: PeerDevice[] = [
  {
    id: 'dev-001',
    name: '办公室工作站 (Windows 11)',
    address: '192.168.1.105:28843',
    status: 'online',
    last_seen: '刚刚',
    mac_address: '00:1A:2B:3C:4D:5E',
  },
  {
    id: 'dev-002',
    name: '家庭影音服务器 (Ubuntu)',
    address: '192.168.1.200:28843',
    status: 'online',
    last_seen: '5分钟前',
    mac_address: '00:11:22:33:44:55',
  },
  {
    id: 'dev-003',
    name: '客厅游戏主机 (RTX 4090)',
    address: '192.168.1.188:28843',
    status: 'offline',
    last_seen: '昨天 22:30',
    mac_address: '04:D4:C4:B4:A4:94',
  },
];

export const Home: React.FC = () => {
  const navigate = useNavigate();
  const { isConnected, peerAddress, connect, disconnect, setDeviceInfo, deviceInfo } =
    useConnectionStore();

  const [devices, setDevices] = useState<PeerDevice[]>(INITIAL_DEVICES);
  const [searchQuery, setSearchQuery] = useState('');
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [connectAddress, setConnectAddress] = useState('');
  const [copied, setCopied] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [statusNotice, setStatusNotice] = useState<string | null>(null);

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
      await safeInvoke('connect_to_device', { address });
      connect(address, name);
      setStatusNotice(null);
      navigate('/remote');
    } catch (err) {
      console.error('Failed to connect:', err);
      setStatusNotice(`连接到 ${name} 失败: ${err || '请确认目标主机已开启且在同一网络'}`);
    } finally {
      setConnecting(false);
    }
  };

  const handleDirectConnect = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!connectAddress.trim()) return;

    setConnecting(true);
    setStatusNotice(`正在直连到 ${connectAddress}...`);
    try {
      await safeInvoke('connect_to_device', { address: connectAddress });
      connect(connectAddress, `远程设备 (${connectAddress})`);
      setIsModalOpen(false);
      setStatusNotice(null);
      navigate('/remote');
    } catch (err) {
      console.error('Direct connect failed:', err);
      setStatusNotice(`直连失败: ${err || '请检查地址格式与端口 (例如 192.168.1.50:28843)'}`);
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
              <span className="flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-[11px] font-medium bg-emerald-500/10 text-emerald-400 border border-emerald-500/20">
                <Radio className="w-3 h-3 animate-pulse" /> 串流引擎在线
              </span>
            </div>

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

            <div className="mt-3 text-[11px] text-zinc-400 flex items-center gap-2">
              <span className="w-1.5 h-1.5 rounded-full bg-primary-light"></span>
              <span>单机测试提示：在输入框直接填 <strong>127.0.0.1</strong> 即可立即自连测试！</span>
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
              onDisconnect={disconnect}
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
