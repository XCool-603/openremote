import { create } from 'zustand';
import { DeviceInfo } from '../types';

interface ConnectionState {
  isConnected: boolean;
  peerAddress: string | null;
  peerName: string | null;
  latency: number;
  fps: number;
  deviceInfo: DeviceInfo | null;
  connect: (address: string, name?: string) => void;
  disconnect: () => void;
  setLatency: (latency: number) => void;
  setFps: (fps: number) => void;
  setDeviceInfo: (info: DeviceInfo | null) => void;
}

export const useConnectionStore = create<ConnectionState>((set) => ({
  isConnected: false,
  peerAddress: null,
  peerName: null,
  latency: 0,
  fps: 0,
  deviceInfo: null,
  connect: (address: string, name?: string) =>
    set({
      isConnected: true,
      peerAddress: address,
      peerName: name || address,
    }),
  disconnect: () =>
    set({
      isConnected: false,
      peerAddress: null,
      peerName: null,
      latency: 0,
      fps: 0,
    }),
  setLatency: (latency: number) => set({ latency }),
  setFps: (fps: number) => set({ fps }),
  setDeviceInfo: (deviceInfo: DeviceInfo | null) => set({ deviceInfo }),
}));
