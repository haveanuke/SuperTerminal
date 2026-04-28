import { describe, it, expect } from 'vitest';
import { runBuddyAgent } from './buddy-agent';

describe('runBuddyAgent', () => {
  it('rejects an empty command', async () => {
    const res = await runBuddyAgent({ command: '', args: [], prompt: 'hi' });
    expect(res.ok).toBe(false);
    expect(res.error).toContain('empty command');
  });

  it('rejects a non-string command', async () => {
    const res = await runBuddyAgent({ command: 123 as unknown as string, args: [], prompt: 'hi' });
    expect(res.ok).toBe(false);
  });

  it('rejects too many args', async () => {
    const res = await runBuddyAgent({
      command: 'claude',
      args: new Array(100).fill('x'),
      prompt: 'hi',
    });
    expect(res.ok).toBe(false);
    expect(res.error).toBe('invalid args');
  });

  it('rejects an arg that is too long', async () => {
    const res = await runBuddyAgent({
      command: 'claude',
      args: ['x'.repeat(10_000)],
      prompt: 'hi',
    });
    expect(res.ok).toBe(false);
    expect(res.error).toBe('invalid args');
  });

  it('rejects a non-string prompt', async () => {
    const res = await runBuddyAgent({
      command: 'claude',
      args: ['{prompt}'],
      prompt: 42 as unknown as string,
    });
    expect(res.ok).toBe(false);
    expect(res.error).toBe('invalid prompt');
  });
});
