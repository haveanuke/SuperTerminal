import type { Companion, AgentConfig } from './buddy-store';

function buildTalkPrompt(companion: Companion, message: string): string {
  const { bones, name, personality } = companion;
  const statLines = (['DEBUGGING', 'PATIENCE', 'CHAOS', 'WISDOM', 'SNARK'] as const)
    .map((s) => `${s}: ${bones.stats[s]}`).join(', ');

  return [
    `You are ${name}, a ${bones.rarity} ${bones.species} companion living in a developer's terminal.`,
    `Your stats: ${statLines}. Peak stat: ${bones.peak}. Weakest stat: ${bones.dump}.`,
    `Personality: ${personality}`,
    '',
    `The developer is speaking to you directly: "${message}"`,
    '',
    'Respond IN CHARACTER with ONE short sentence (max 20 words).',
    'No preamble, no markdown, no action-asterisks unless they fit. Just your spoken reply.',
    'Let your stats colour the tone — high SNARK = sarcastic, high WISDOM = sage, low PATIENCE = tetchy, high CHAOS = weird.',
    'Output ONLY the reply text.',
  ].join('\n');
}

export async function talkToBuddy(
  companion: Companion,
  agent: AgentConfig,
  message: string,
): Promise<{ ok: boolean; text: string; error?: string }> {
  if (!agent.enabled) {
    return { ok: false, text: '', error: 'Smart mode is off. Enable it in Settings → Buddy.' };
  }
  if (!message.trim()) {
    return { ok: false, text: '', error: 'empty message' };
  }
  const prompt = buildTalkPrompt(companion, message.trim());
  try {
    return await window.superTerminal.buddy.react({
      command: agent.command,
      args: agent.args,
      prompt,
    });
  } catch (err) {
    return { ok: false, text: '', error: String(err) };
  }
}
