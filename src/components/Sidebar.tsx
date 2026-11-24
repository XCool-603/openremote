import React, { useEffect, useState } from 'react';
import { NavLink } from 'react-router-dom';
import { Monitor, Cast, FolderSync, Settings, ShieldCheck, User, LogOut, KeyRound } from 'lucide-react';
import { ConnectionStatus } from './ConnectionStatus';
import { useConnectionStore } from '../stores/connectionStore';
import { safeInvoke } from '../hooks/useTauri';

interface AuthStatus {
  authenticated: boolean;
  username?: string | null;
  token?: string | null;
}

export const Sidebar: React.FC = () => {
  const { authUser, setAuthUser, isAuthModalOpen, setAuthModalOpen } = useConnectionStore();
  const [authMode, setAuthMode] = useState<'login' | 'register'>('login');
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [authLoading, setAuthLoading] = useState(false);
  const [authError, setAuthError] = useState<string | null>(null);

  useEffect(() => {
    safeInvoke<AuthStatus>('get_auth_status')
      .then((res) => {
        if (res && res.authenticated && res.username) {
          setAuthUser(res.username);
        }
      })
      .catch(() => {});
  }, [setAuthUser]);

  const handleAuthSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!username.trim() || !password) return;
    setAuthLoading(true);
    setAuthError(null);

    try {
      const cmd = authMode === 'login' ? 'login_account' : 'register_account';
      const res = await safeInvoke<AuthStatus>(cmd, {
        username: username.trim(),
        password,
      });
      if (res && res.authenticated && res.username) {
        setAuthUser(res.username);
        setAuthModalOpen(false);
        setPassword('');
      }
    } catch (err) {
      setAuthError(String(err || '认证失败，请确认服务器已更新并在线'));
    } finally {
      setAuthLoading(false);
    }
  };

  const handleLogout = async () => {
    try {
      await safeInvoke('logout_account');
      setAuthUser(null);
    } catch (err) {
      console.warn('Logout error:', err);
    }
  };

  const navItems = [
    { to: '/', label: '设备列表', icon: Monitor },
    { to: '/remote', label: '远程桌面', icon: Cast },
    { to: '/files', label: '文件传输', icon: FolderSync },
    { to: '/settings', label: '系统设置', icon: Settings },
  ];

  return (
    <>
      <aside className="w-60 bg-surface border-r border-surface-lighter flex flex-col justify-between h-screen select-none shrink-0">
        <div>
          {/* App Branding */}
          <div className="h-16 flex items-center px-6 border-b border-surface-lighter/50 gap-3">
            <div className="w-9 h-9 rounded-xl bg-gradient-to-tr from-primary-dark to-primary flex items-center justify-center shadow-lg shadow-primary/20">
              <ShieldCheck className="w-5 h-5 text-white" />
            </div>
            <div>
              <span className="font-bold text-base tracking-tight text-white block">
                OpenRemote
              </span>
              <span className="text-[10px] text-zinc-400 font-medium tracking-wider uppercase block">
                高性能远程控制
              </span>
            </div>
          </div>

          {/* Navigation list */}
          <nav className="p-3 space-y-1">
            {navItems.map((item) => {
              const Icon = item.icon;
              return (
                <NavLink
                  key={item.to}
                  to={item.to}
                  className={({ isActive }) =>
                    `flex items-center gap-3 px-3.5 py-2.5 rounded-lg text-sm font-medium transition-all ${
                      isActive
                        ? 'bg-primary text-white shadow-md shadow-primary/25 font-semibold'
                        : 'text-zinc-400 hover:text-zinc-200 hover:bg-surface-light/60'
                    }`
                  }
                >
                  <Icon className="w-4 h-4 shrink-0" />
                  <span>{item.label}</span>
                </NavLink>
              );
            })}
          </nav>
        </div>

        {/* Account & Footer Status */}
        <div className="p-4 border-t border-surface-lighter/50 space-y-3 bg-surface-dark/40">
          {/* Account Card */}
          <div className="p-3 rounded-xl bg-surface-light/70 border border-surface-lighter">
            {authUser ? (
              <div className="flex items-center justify-between gap-2">
                <div className="flex items-center gap-2.5 min-w-0">
                  <div className="w-8 h-8 rounded-lg bg-emerald-500/20 border border-emerald-500/30 flex items-center justify-center text-emerald-400 shrink-0">
                    <User className="w-4 h-4" />
                  </div>
                  <div className="min-w-0">
                    <div className="text-xs font-bold text-white truncate">{authUser}</div>
                    <div className="text-[10px] text-emerald-400 font-medium">
                      ✓ 中继服务器已授权
                    </div>
                  </div>
                </div>
                <button
                  onClick={handleLogout}
                  className="p-1.5 rounded-lg hover:bg-surface-lighter text-zinc-400 hover:text-rose-400 transition-colors shrink-0"
                  title="退出登录"
                >
                  <LogOut className="w-3.5 h-3.5" />
                </button>
              </div>
            ) : (
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <span className="text-xs font-semibold text-zinc-200">未登录账号</span>
                  <span className="text-[10px] px-1.5 py-0.5 rounded bg-amber-500/15 text-amber-400 border border-amber-500/30">
                    中继需登录
                  </span>
                </div>
                <button
                  onClick={() => {
                    setAuthError(null);
                    setAuthModalOpen(true);
                  }}
                  className="w-full py-1.5 px-3 rounded-lg bg-primary hover:bg-primary-dark text-white text-xs font-semibold flex items-center justify-center gap-1.5 shadow shadow-primary/25 transition-all"
                >
                  <KeyRound className="w-3.5 h-3.5" />
                  <span>登录 / 注册账号</span>
                </button>
              </div>
            )}
          </div>

          <div className="text-[11px] font-semibold text-zinc-400 uppercase tracking-wider px-1">
            当前状态
          </div>
          <div className="overflow-hidden">
            <ConnectionStatus />
          </div>
        </div>
      </aside>

      {/* Account Login / Register Modal */}
      {isAuthModalOpen && (
        <div className="fixed inset-0 z-50 bg-black/65 backdrop-blur-sm flex items-center justify-center p-4">
          <div className="bg-surface border border-surface-lighter rounded-2xl w-full max-w-md p-6 shadow-2xl">
            <div className="flex items-center justify-between mb-4">
              <div>
                <h2 className="text-lg font-bold text-white">
                  {authMode === 'login' ? '登录中继服务器账号' : '注册中继服务器账号'}
                </h2>
                <p className="text-xs text-zinc-400 mt-0.5">
                  登录后即可授权使用 dxcool.cn:28845 高速云端中继穿透
                </p>
              </div>
              <button
                onClick={() => setAuthModalOpen(false)}
                className="text-zinc-400 hover:text-white text-sm px-2 py-1"
              >
                ✕
              </button>
            </div>

            <div className="flex rounded-xl bg-surface-dark p-1 mb-4 border border-surface-lighter">
              <button
                type="button"
                onClick={() => {
                  setAuthMode('login');
                  setAuthError(null);
                }}
                className={`flex-1 py-1.5 text-xs font-semibold rounded-lg transition-all ${
                  authMode === 'login'
                    ? 'bg-primary text-white shadow'
                    : 'text-zinc-400 hover:text-white'
                }`}
              >
                账号登录
              </button>
              <button
                type="button"
                onClick={() => {
                  setAuthMode('register');
                  setAuthError(null);
                }}
                className={`flex-1 py-1.5 text-xs font-semibold rounded-lg transition-all ${
                  authMode === 'register'
                    ? 'bg-primary text-white shadow'
                    : 'text-zinc-400 hover:text-white'
                }`}
              >
                注册新账号
              </button>
            </div>

            <form onSubmit={handleAuthSubmit} className="space-y-3.5">
              <div>
                <label className="block text-xs font-semibold text-zinc-300 mb-1">
                  用户名
                </label>
                <input
                  type="text"
                  required
                  autoFocus
                  placeholder="请输入用户名 (默认测试账号: admin)"
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                  className="w-full bg-surface-dark border border-surface-lighter rounded-xl px-3.5 py-2.5 text-sm text-white placeholder-zinc-500 focus:outline-none focus:border-primary"
                />
              </div>

              <div>
                <label className="block text-xs font-semibold text-zinc-300 mb-1">
                  密码
                </label>
                <input
                  type="password"
                  required
                  placeholder="请输入密码 (默认测试密码: 123456)"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  className="w-full bg-surface-dark border border-surface-lighter rounded-xl px-3.5 py-2.5 text-sm text-white placeholder-zinc-500 focus:outline-none focus:border-primary"
                />
              </div>

              {authMode === 'login' && (
                <div className="flex items-center justify-between text-[11px] text-zinc-400 bg-surface-dark/60 px-3 py-2 rounded-lg border border-surface-lighter/50">
                  <span>服务器预置默认账号: <strong>admin / 123456</strong></span>
                  <button
                    type="button"
                    onClick={() => {
                      setUsername('admin');
                      setPassword('123456');
                    }}
                    className="text-primary-light hover:underline font-medium"
                  >
                    一键填入
                  </button>
                </div>
              )}

              {authError && (
                <div className="px-3 py-2 rounded-lg bg-rose-500/15 border border-rose-500/30 text-xs text-rose-300">
                  {authError}
                </div>
              )}

              <div className="flex items-center justify-end gap-2.5 pt-2">
                <button
                  type="button"
                  onClick={() => setAuthModalOpen(false)}
                  className="px-4 py-2 rounded-xl text-xs font-medium text-zinc-400 hover:text-white hover:bg-surface-light transition-colors"
                >
                  取消
                </button>
                <button
                  type="submit"
                  disabled={authLoading || !username.trim() || !password}
                  className="px-5 py-2 rounded-xl bg-primary hover:bg-primary-dark disabled:opacity-50 text-white text-xs font-semibold shadow-lg shadow-primary/25 transition-all"
                >
                  {authLoading
                    ? '正在验证...'
                    : authMode === 'login'
                    ? '立即登录'
                    : '注册并登录'}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </>
  );
};
