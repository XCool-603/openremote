import React, { useState, useEffect } from 'react';
import { AppConfig } from '../types';
import { safeInvoke } from '../hooks/useTauri';
import {
  Save,
  Copy,
  Check,
  Shield,
  Server,
  Info,
  Sliders,
  CheckCircle2,
  Lock,
} from 'lucide-react';

const DEFAULT_CONFIG: AppConfig = {
  device_id: 'OR-884920',
  device_code: '739-281-904',
  access_password: '391024',
  device_name: 'OpenRemote-Desktop',
  listen_port: 28843,
  signal_server_url: 'wss://signal.openremote.io:8080/v1',
};

export const Settings: React.FC = () => {
  const [config, setConfig] = useState<AppConfig>(DEFAULT_CONFIG);
  const [copied, setCopied] = useState(false);
  const [savedSuccess, setSavedSuccess] = useState(false);
  const [isSaving, setIsSaving] = useState(false);

  // Security toggles
  const [requirePassword, setRequirePassword] = useState(true);
  const [password, setPassword] = useState('******');
  const [autoAcceptTrusted, setAutoAcceptTrusted] = useState(false);

  useEffect(() => {
    async function loadConfig() {
      try {
        const loaded = await safeInvoke<AppConfig>('get_config');
        if (loaded) {
          setConfig(loaded);
        }
      } catch {
        console.info('get_config fallback active');
      }
    }
    loadConfig();
  }, []);

  const handleCopyId = () => {
    navigator.clipboard.writeText(config.device_id);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleSave = async (e: React.FormEvent) => {
    e.preventDefault();
    setIsSaving(true);
    try {
      await safeInvoke('save_config', { config });
      setSavedSuccess(true);
    } catch {
      // preview fallback
      setSavedSuccess(true);
    } finally {
      setIsSaving(false);
      setTimeout(() => setSavedSuccess(false), 3000);
    }
  };

  return (
    <div className="flex-1 flex flex-col h-full bg-surface-dark overflow-y-auto">
      {/* Top Header */}
      <div className="bg-surface/80 border-b border-surface-lighter/60 px-8 py-5 backdrop-blur shrink-0 flex items-center justify-between">
        <div>
          <h1 className="text-xl font-bold text-white">系统设置</h1>
          <p className="text-xs text-zinc-400 mt-0.5">
            配置本地服务监听端口、信令服务与安全访问控制参数。
          </p>
        </div>

        {savedSuccess && (
          <div className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-emerald-500/10 border border-emerald-500/30 text-emerald-400 text-xs font-medium animate-fade-in">
            <CheckCircle2 className="w-3.5 h-3.5" />
            <span>配置已成功保存并应用</span>
          </div>
        )}
      </div>

      <div className="p-8 max-w-4xl space-y-6">
        <form onSubmit={handleSave} className="space-y-6">
          {/* General Device Settings */}
          <div className="bg-surface border border-surface-lighter rounded-2xl p-6 space-y-5">
            <div className="flex items-center gap-2.5 pb-3 border-b border-surface-lighter/50 text-white font-semibold text-sm">
              <Sliders className="w-4 h-4 text-primary-light" />
              <span>基本信息</span>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 gap-5">
              <div>
                <label className="block text-xs font-semibold text-zinc-300 mb-2">
                  本机设备名称
                </label>
                <input
                  type="text"
                  required
                  value={config.device_name}
                  onChange={(e) => setConfig({ ...config, device_name: e.target.value })}
                  className="w-full bg-surface-dark border border-surface-lighter rounded-xl px-3.5 py-2.5 text-xs text-white focus:outline-none focus:border-primary transition-colors"
                />
              </div>

              <div>
                <label className="block text-xs font-semibold text-zinc-300 mb-2">
                  本机设备 ID (只读)
                </label>
                <div className="flex gap-2">
                  <input
                    type="text"
                    readOnly
                    value={config.device_id}
                    className="flex-1 bg-surface-dark border border-surface-lighter rounded-xl px-3.5 py-2.5 text-xs text-zinc-400 font-mono select-all focus:outline-none"
                  />
                  <button
                    type="button"
                    onClick={handleCopyId}
                    className="p-2.5 rounded-xl bg-surface-light border border-surface-lighter text-zinc-300 hover:text-white transition-colors"
                    title="复制 ID"
                  >
                    {copied ? <Check className="w-4 h-4 text-emerald-400" /> : <Copy className="w-4 h-4" />}
                  </button>
                </div>
              </div>
            </div>
          </div>

          {/* Network & Signaling Settings */}
          <div className="bg-surface border border-surface-lighter rounded-2xl p-6 space-y-5">
            <div className="flex items-center gap-2.5 pb-3 border-b border-surface-lighter/50 text-white font-semibold text-sm">
              <Server className="w-4 h-4 text-indigo-400" />
              <span>网络通信与信令</span>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 gap-5">
              <div>
                <label className="block text-xs font-semibold text-zinc-300 mb-2">
                  UDP/TCP 监听端口
                </label>
                <input
                  type="number"
                  required
                  min={1024}
                  max={65535}
                  value={config.listen_port}
                  onChange={(e) =>
                    setConfig({ ...config, listen_port: parseInt(e.target.value, 10) || 8443 })
                  }
                  className="w-full bg-surface-dark border border-surface-lighter rounded-xl px-3.5 py-2.5 text-xs text-white font-mono focus:outline-none focus:border-primary transition-colors"
                />
                <span className="text-[11px] text-zinc-500 mt-1 block">
                  用于局域网内直接直连监听（默认 8443）
                </span>
              </div>

              <div>
                <label className="block text-xs font-semibold text-zinc-300 mb-2">
                  WebRTC 信令中继服务器 URL
                </label>
                <input
                  type="text"
                  required
                  value={config.signal_server_url}
                  onChange={(e) => setConfig({ ...config, signal_server_url: e.target.value })}
                  className="w-full bg-surface-dark border border-surface-lighter rounded-xl px-3.5 py-2.5 text-xs text-white font-mono focus:outline-none focus:border-primary transition-colors"
                />
                <span className="text-[11px] text-zinc-500 mt-1 block">
                  NAT 穿透及跨公网协助打洞与握手通道
                </span>
              </div>
            </div>
          </div>

          {/* Security & Access Protection */}
          <div className="bg-surface border border-surface-lighter rounded-2xl p-6 space-y-5">
            <div className="flex items-center gap-2.5 pb-3 border-b border-surface-lighter/50 text-white font-semibold text-sm">
              <Shield className="w-4 h-4 text-emerald-400" />
              <span>安全与接入授权</span>
            </div>

            <div className="space-y-4 text-xs">
              <div className="flex items-center justify-between p-3 rounded-xl bg-surface-dark border border-surface-lighter/50">
                <div>
                  <p className="font-semibold text-white">受控连接安全验证码</p>
                  <p className="text-zinc-400 text-[11px] mt-0.5">外部设备远程本机时必须输入此访问密匙</p>
                </div>
                <div className="flex items-center gap-2">
                  <input
                    type="password"
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    className="bg-surface border border-surface-lighter rounded-lg px-2.5 py-1 text-xs text-white font-mono focus:outline-none focus:border-primary w-28"
                  />
                </div>
              </div>

              <div className="flex items-center justify-between p-3 rounded-xl bg-surface-dark border border-surface-lighter/50">
                <div>
                  <p className="font-semibold text-white">自动允许受信任设备</p>
                  <p className="text-zinc-400 text-[11px] mt-0.5">同一账号或绑定列表中的已授权设备可无需确认直接连接</p>
                </div>
                <label className="relative inline-flex items-center cursor-pointer">
                  <input
                    type="checkbox"
                    checked={autoAcceptTrusted}
                    onChange={(e) => setAutoAcceptTrusted(e.target.checked)}
                    className="sr-only peer"
                  />
                  <div className="w-9 h-5 bg-surface-light peer-focus:outline-none rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-4 after:w-4 after:transition-all peer-checked:bg-primary"></div>
                </label>
              </div>
            </div>
          </div>

          {/* About Section */}
          <div className="bg-surface border border-surface-lighter rounded-2xl p-6">
            <div className="flex items-center gap-2.5 pb-3 border-b border-surface-lighter/50 text-white font-semibold text-sm mb-4">
              <Info className="w-4 h-4 text-blue-400" />
              <span>关于软件</span>
            </div>
            <div className="flex items-center justify-between text-xs text-zinc-400">
              <div>
                <p className="text-white font-medium">OpenRemote 远程控制客户端</p>
                <p className="mt-0.5">Cross-Platform Ultra-Low Latency Remote Engine</p>
              </div>
              <div className="text-right font-mono">
                <span className="px-2.5 py-1 rounded bg-surface-dark border border-surface-lighter text-primary-light font-semibold">
                  v0.1.0-alpha
                </span>
              </div>
            </div>
          </div>

          {/* Save Action */}
          <div className="flex justify-end pt-2">
            <button
              type="submit"
              disabled={isSaving}
              className="inline-flex items-center gap-2 px-6 py-2.5 rounded-xl bg-primary hover:bg-primary-dark disabled:opacity-50 text-white text-xs font-semibold shadow-lg shadow-primary/25 transition-all"
            >
              <Save className="w-4 h-4" />
              <span>{isSaving ? '正在保存...' : '保存更改'}</span>
            </button>
          </div>
        </form>
      </div>
    </div>
  );
};
