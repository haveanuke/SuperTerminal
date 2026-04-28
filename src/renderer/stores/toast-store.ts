import { createStore } from '../lib/create-store';

export type ToastLevel = 'error' | 'info';

export interface Toast {
  id: number;
  message: string;
  level: ToastLevel;
}

interface ToastStoreState {
  toasts: Toast[];
  push: (message: string, level?: ToastLevel) => void;
  dismiss: (id: number) => void;
}

let nextId = 1;

export const useToastStore = createStore<ToastStoreState>((set, get) => ({
  toasts: [],
  push: (message, level = 'error') => {
    const id = nextId++;
    set({ toasts: [...get().toasts, { id, message, level }] });
  },
  dismiss: (id) => {
    set({ toasts: get().toasts.filter((t) => t.id !== id) });
  },
}));

export function toastError(message: string) {
  useToastStore.getState().push(message, 'error');
}

export function toastInfo(message: string) {
  useToastStore.getState().push(message, 'info');
}
