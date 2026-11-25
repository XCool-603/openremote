import { create } from 'zustand';
import { DeviceInfo } from '../types';

interface ConnectionState {
  isConnected: boolean;
  peerAddress: string | null;
  peerName: string | null;
  connectionMode: string | null;
  connectionTarget: string | null;
  latency: number;
  fps: number;
  deviceInfo: DeviceInfo | null;
  authUser: string | null;
  isAuthModalOpen: boolean;
  connect: (address: string, name?: string, mode?: string, target?: string) => void;
  disconnect: () => void;
  setLatency: (latency: number) => void;
  setFps: (fps: number) => void;
  setDeviceInfo: (info: DeviceInfo | null) => void;
  setAuthUser: (username: string | null) => void;
  setAuthModalOpen: (open: boolean) => void;
}

export const useConnectionStore = create<ConnectionState>((set) => ({
  isConnected: false,
  peerAddress: null,
  peerName: null,
  connectionMode: null,
  connectionTarget: null,
  latency: 0,
  fps: 0,
  deviceInfo: null,
  authUser: null,
  isAuthModalOpen: false,
  connect: (address: string, name?: string, mode?: string, target?: string) =>
    set({
      isConnected: true,
      peerAddress: address,
      peerName: name || address,
      connectionMode: mode || null,
      connectionTarget: target || null,
    }),
  disconnect: () =>
    set({
      isConnected: false,
      peerAddress: null,
      peerName: null,
      connectionMode: null,
      connectionTarget: null,
      latency: 0,
      fps: 0,
    }),
  setLatency: (latency: number) => set({ latency }),
  setFps: (fps: number) => set({ fps }),
  setDeviceInfo: (deviceInfo: DeviceInfo | null) => set({ deviceInfo }),
  setAuthUser: (authUser: string | null) => set({ authUser }),
  setAuthModalOpen: (isAuthModalOpen: boolean) => set({ isAuthModalOpen }),
}));
