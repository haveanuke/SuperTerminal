// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from 'vitest';
import { useToastStore, toastError, toastInfo } from './toast-store';

describe('toast-store', () => {
  beforeEach(() => {
    useToastStore.setState({ toasts: [] });
  });

  it('push adds an error toast by default', () => {
    useToastStore.getState().push('boom');
    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0].message).toBe('boom');
    expect(toasts[0].level).toBe('error');
    expect(typeof toasts[0].id).toBe('number');
  });

  it('push adds an info toast when level=info', () => {
    useToastStore.getState().push('hello', 'info');
    expect(useToastStore.getState().toasts[0].level).toBe('info');
  });

  it('dismiss removes the toast with the given id', () => {
    useToastStore.getState().push('a');
    useToastStore.getState().push('b');
    const ids = useToastStore.getState().toasts.map((t) => t.id);
    useToastStore.getState().dismiss(ids[0]);
    const remaining = useToastStore.getState().toasts;
    expect(remaining).toHaveLength(1);
    expect(remaining[0].message).toBe('b');
  });

  it('multiple toasts get unique increasing ids', () => {
    useToastStore.getState().push('a');
    useToastStore.getState().push('b');
    const [a, b] = useToastStore.getState().toasts;
    expect(b.id).toBeGreaterThan(a.id);
  });

  it('toastError and toastInfo helpers route to push with the right level', () => {
    toastError('err');
    toastInfo('info');
    const toasts = useToastStore.getState().toasts;
    expect(toasts.map((t) => t.level)).toEqual(['error', 'info']);
  });
});
