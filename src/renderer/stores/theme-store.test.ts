// @vitest-environment jsdom
import { describe, it, expect, beforeEach } from 'vitest';
import { useThemeStore, builtinThemes, validateTheme, exportThemeJSON } from './theme-store';

describe('theme-store', () => {
  beforeEach(() => {
    localStorage.clear();
    useThemeStore.setState({ theme: builtinThemes[0], customThemes: [] });
  });

  it('defaults to Tokyo Night', () => {
    expect(useThemeStore.getState().theme.name).toBe('Tokyo Night');
  });

  it('setTheme switches the active theme', () => {
    useThemeStore.getState().setTheme(builtinThemes[1]);
    expect(useThemeStore.getState().theme.name).toBe(builtinThemes[1].name);
  });

  it('addCustomTheme persists and exposes the theme via getAllThemes', () => {
    const custom = { ...builtinThemes[0], name: 'Mine' };
    useThemeStore.getState().addCustomTheme(custom);
    expect(useThemeStore.getState().customThemes.find((t) => t.name === 'Mine')).toBeDefined();
    expect(useThemeStore.getState().getAllThemes().some((t) => t.name === 'Mine')).toBe(true);
    expect(localStorage.getItem('superTerminal:customThemes')).toContain('Mine');
  });

  it('addCustomTheme replaces a same-named existing custom theme', () => {
    const v1 = { ...builtinThemes[0], name: 'Mine', background: '#000000' };
    const v2 = { ...builtinThemes[0], name: 'Mine', background: '#ffffff' };
    useThemeStore.getState().addCustomTheme(v1);
    useThemeStore.getState().addCustomTheme(v2);
    const customs = useThemeStore.getState().customThemes.filter((t) => t.name === 'Mine');
    expect(customs).toHaveLength(1);
    expect(customs[0].background).toBe('#ffffff');
  });

  it('removeCustomTheme falls back to default when the active theme is removed', () => {
    const custom = { ...builtinThemes[0], name: 'Mine' };
    useThemeStore.getState().addCustomTheme(custom);
    useThemeStore.getState().setTheme(custom);
    useThemeStore.getState().removeCustomTheme('Mine');
    expect(useThemeStore.getState().theme.name).toBe('Tokyo Night');
  });

  it('validateTheme accepts a complete theme', () => {
    expect(validateTheme(builtinThemes[0])).toBe(true);
  });

  it('validateTheme rejects a theme missing required fields', () => {
    const partial = { name: 'broken', background: '#000' };
    expect(validateTheme(partial)).toBe(false);
  });

  it('validateTheme rejects non-objects', () => {
    expect(validateTheme(null)).toBe(false);
    expect(validateTheme('string')).toBe(false);
    expect(validateTheme([])).toBe(false);
  });

  it('exportThemeJSON produces parseable JSON', () => {
    const json = exportThemeJSON(builtinThemes[0]);
    expect(JSON.parse(json).name).toBe(builtinThemes[0].name);
  });
});
