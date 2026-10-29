import React from 'react';
import { NavLink } from 'react-router-dom';
import { Monitor, Cast, FolderSync, Settings, ShieldCheck } from 'lucide-react';
import { ConnectionStatus } from './ConnectionStatus';

export const Sidebar: React.FC = () => {
  const navItems = [
    { to: '/', label: '设备列表', icon: Monitor },
    { to: '/remote', label: '远程桌面', icon: Cast },
    { to: '/files', label: '文件传输', icon: FolderSync },
    { to: '/settings', label: '系统设置', icon: Settings },
  ];

  return (
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

      {/* Footer / Status */}
      <div className="p-4 border-t border-surface-lighter/50 space-y-3 bg-surface-dark/40">
        <div className="text-[11px] font-semibold text-zinc-400 uppercase tracking-wider px-1">
          当前状态
        </div>
        <div className="overflow-hidden">
          <ConnectionStatus />
        </div>
      </div>
    </aside>
  );
};
