import { useEffect } from 'react';
import { useToastStore, type Toast } from '../stores/toast-store';
import { useThemeStore } from '../stores/theme-store';

const TOAST_TTL_MS = 5000;

function ToastItem({ toast }: { toast: Toast }) {
  const dismiss = useToastStore((s) => s.dismiss);
  const theme = useThemeStore((s) => s.theme);

  useEffect(() => {
    const timer = setTimeout(() => dismiss(toast.id), TOAST_TTL_MS);
    return () => clearTimeout(timer);
  }, [toast.id, dismiss]);

  const accent = toast.level === 'error' ? theme.red : theme.uiAccent;

  return (
    <div
      style={{
        backgroundColor: theme.uiSurface,
        color: theme.uiText,
        border: `1px solid ${accent}`,
        borderLeft: `3px solid ${accent}`,
        borderRadius: 4,
        padding: '8px 12px',
        fontSize: 12,
        minWidth: 200,
        maxWidth: 400,
        display: 'flex',
        alignItems: 'flex-start',
        gap: 8,
        boxShadow: '0 4px 12px rgba(0,0,0,0.3)',
      }}
    >
      <span style={{ flex: 1, wordBreak: 'break-word' }}>{toast.message}</span>
      <button
        onClick={() => dismiss(toast.id)}
        style={{
          background: 'transparent',
          border: 'none',
          color: theme.uiTextMuted,
          cursor: 'pointer',
          padding: 0,
          fontSize: 14,
          lineHeight: 1,
        }}
        aria-label="Dismiss"
      >
        ×
      </button>
    </div>
  );
}

export function Toasts() {
  const toasts = useToastStore((s) => s.toasts);
  if (toasts.length === 0) return null;
  return (
    <div
      style={{
        position: 'fixed',
        bottom: 32,
        right: 16,
        zIndex: 1000,
        display: 'flex',
        flexDirection: 'column',
        gap: 6,
        pointerEvents: 'auto',
      }}
    >
      {toasts.map((t) => <ToastItem key={t.id} toast={t} />)}
    </div>
  );
}
