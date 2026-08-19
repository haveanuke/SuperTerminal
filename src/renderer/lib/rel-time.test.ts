import { describe, it, expect } from 'vitest';
import { relTime } from './rel-time';

describe('relTime', () => {
  const now = 1_700_000_000_000; // fixed ms epoch
  const at = (secsAgo: number) => relTime(now / 1000 - secsAgo, now);

  it('formats each magnitude compactly', () => {
    expect(at(10)).toBe('now');
    expect(at(5 * 60)).toBe('5m');
    expect(at(3 * 3600)).toBe('3h');
    expect(at(2 * 86400)).toBe('2d');
    expect(at(70 * 86400)).toBe('2mo');
    expect(at(400 * 86400)).toBe('1y');
  });

  it('clamps future times to now', () => {
    expect(relTime(now / 1000 + 999, now)).toBe('now');
  });
});
