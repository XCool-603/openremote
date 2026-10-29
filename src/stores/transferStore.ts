import { create } from 'zustand';
import { TransferProgress } from '../types';

interface TransferState {
  transfers: TransferProgress[];
  addTransfer: (transfer: TransferProgress) => void;
  updateProgress: (id: string, transferred_bytes: number, speed_bps: number) => void;
  updateStatus: (id: string, status: TransferProgress['status']) => void;
  removeTransfer: (id: string) => void;
  clearCompleted: () => void;
}

export const useTransferStore = create<TransferState>((set) => ({
  transfers: [],
  addTransfer: (transfer: TransferProgress) =>
    set((state) => ({
      transfers: [transfer, ...state.transfers.filter((t) => t.id !== transfer.id)],
    })),
  updateProgress: (id: string, transferred_bytes: number, speed_bps: number) =>
    set((state) => ({
      transfers: state.transfers.map((t) =>
        t.id === id
          ? {
              ...t,
              transferred_bytes,
              speed_bps,
              status:
                transferred_bytes >= t.total_bytes && t.total_bytes > 0
                  ? 'Completed'
                  : t.status,
            }
          : t
      ),
    })),
  updateStatus: (id: string, status: TransferProgress['status']) =>
    set((state) => ({
      transfers: state.transfers.map((t) => (t.id === id ? { ...t, status } : t)),
    })),
  removeTransfer: (id: string) =>
    set((state) => ({
      transfers: state.transfers.filter((t) => t.id !== id),
    })),
  clearCompleted: () =>
    set((state) => ({
      transfers: state.transfers.filter(
        (t) => t.status !== 'Completed' && t.status !== 'Failed'
      ),
    })),
}));
