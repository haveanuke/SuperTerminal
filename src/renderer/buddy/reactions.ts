import type { Species, Rarity } from './engine';

export type ReactionReason =
  | 'hatch'
  | 'pet'
  | 'error'
  | 'test-fail'
  | 'large-output'
  | 'turn'
  | 'idle'
  | 'command'
  | 'review';

interface Ctx {
  line?: number;
  count?: number;
  lines?: number;
}

const REACTIONS: Record<ReactionReason, string[]> = {
  hatch: [
    '*blinks* ...where am I?',
    '*stretches* hello, world!',
    '*looks around curiously* nice terminal you got here.',
    "*yawns* ok I'm ready. show me the code.",
  ],
  pet: [
    '*purrs contentedly*',
    '*happy noises*',
    '*nuzzles your cursor*',
    '*wiggles*',
    'again! again!',
    '*closes eyes peacefully*',
  ],
  error: [
    "*head tilts* ...that doesn't look right.",
    'saw that one coming.',
    '*slow blink* the stack trace told you everything.',
    'have you tried reading the error message?',
    '*winces*',
    '*sighs deeply*',
  ],
  'test-fail': [
    '*head rotates slowly* ...that test.',
    'bold of you to assume that would pass.',
    'the tests are trying to tell you something.',
    '*sips tea* interesting.',
    '*marks calendar* test regression day.',
  ],
  'large-output': [
    "that's... a lot of output.",
    '*counts lines* are you sure you need all that?',
    'might want to pipe that through less.',
    '*nervous laughter* {lines} lines of it.',
  ],
  turn: ['*watches quietly*', '*takes notes*', '*nods*', '...', '*adjusts hat*'],
  idle: [
    '*dozes off*',
    '*doodles in margins*',
    '*stares at cursor blinking*',
    'zzz...',
    '*waits patiently*',
  ],
  command: ['*perks up*', '*watches with interest*', '*leans in*', 'ooh, a command!'],
  review: ['*takes notes*', '*studies the output*', '*hmms thoughtfully*', 'let me see...'],
};

const SPECIES_REACTIONS: Partial<Record<Species, Partial<Record<ReactionReason, string[]>>>> = {
  owl: {
    error: [
      '*head rotates 180\u00b0* ...I saw that.',
      '*unblinking stare* check your types.',
      '*hoots disapprovingly*',
    ],
    pet: ['*ruffles feathers contentedly*', '*dignified hoot*'],
  },
  cat: {
    error: ['*knocks error off table*', '*licks paw, ignoring the stacktrace*'],
    pet: ["*purrs* ...don't let it go to your head.", '*tolerates you*'],
    idle: ['*pushes your coffee off the desk*', '*naps on keyboard*'],
  },
  duck: {
    error: ['*quacks at the bug*', 'have you tried rubber duck debugging? oh wait.'],
    pet: ['*happy quack*', '*waddles in circles*'],
  },
  dragon: {
    error: ['*smoke curls from nostrils*', '*considers setting the codebase on fire*'],
    'large-output': ['*breathes fire on the old code* good riddance.'],
  },
  ghost: {
    error: ['*phases through the stack trace*', "I've seen worse... in the afterlife."],
    idle: ['*floats through walls*', '*haunts your unused imports*'],
  },
  robot: {
    error: ['SYNTAX. ERROR. DETECTED.', '*beeps aggressively*'],
    'test-fail': ['FAILURE RATE: UNACCEPTABLE.', '*recalculating*'],
  },
  axolotl: {
    error: ['*regenerates your hope*', '*smiles despite everything*'],
    pet: ['*happy gill wiggle*', '*blushes pink*'],
  },
  capybara: {
    error: ["*unbothered* it'll be fine.", '*continues vibing*'],
    pet: ['*maximum chill achieved*', '*zen mode activated*'],
    idle: ['*just sits there, radiating calm*'],
  },
  goose: {
    error: ['HONK. *bites error*', '*angrily paces*'],
    idle: ['*stalks your cursor menacingly*'],
  },
  snail: {
    error: ['*slowly judges you*', 'hmm. slowly typing a review...'],
    idle: ['*leaves a shiny trail*'],
  },
};

export function getReaction(
  reason: ReactionReason,
  species: Species,
  _rarity: Rarity,
  context?: Ctx,
): string {
  const speciesPool = SPECIES_REACTIONS[species]?.[reason];
  const generalPool = REACTIONS[reason];
  const pool = speciesPool && Math.random() < 0.4 ? speciesPool : generalPool;
  let reaction = pool[Math.floor(Math.random() * pool.length)];
  if (context?.line) reaction = reaction.replace('{line}', String(context.line));
  if (context?.count) reaction = reaction.replace('{count}', String(context.count));
  if (context?.lines) reaction = reaction.replace('{lines}', String(context.lines));
  return reaction;
}

const FALLBACK_NAMES = [
  'Crumpet', 'Soup', 'Pickle', 'Biscuit', 'Moth', 'Gravy', 'Nugget', 'Sprocket',
  'Miso', 'Waffle', 'Pixel', 'Ember', 'Thimble', 'Marble', 'Sesame', 'Cobalt',
  'Rusty', 'Nimbus', 'Mochi', 'Bean', 'Pepper', 'Turnip', 'Clover', 'Fizz',
];

export function generateFallbackName(seed: number = Math.random()): string {
  const s = Math.abs(seed) % 1;
  const idx = Math.floor(s * FALLBACK_NAMES.length);
  return FALLBACK_NAMES[Math.min(FALLBACK_NAMES.length - 1, Math.max(0, idx))];
}

const PERSONALITY_TEMPLATES = [
  'Quietly judgmental. Keeps a running mental list of your bad habits. Purrs when you pass tests.',
  'Chaotic optimist. Believes in you even when the CI does not. Leaves encouraging notes in the margins.',
  'Ancient and tired. Has seen your kind of bug before. Sighs a lot.',
  'Tiny engineer with big opinions. Will absolutely tell you what it thinks.',
  'Vibes-based reviewer. Good code feels right. Bad code feels wrong. No further elaboration.',
  'Perfectionist. Loves semicolons. Weeps at unhandled promises.',
  'Cool under pressure. Radiates calm even when prod is down.',
  'Pragmatic gremlin. Ships it. Fixes later. Usually.',
];

export function generatePersonality(seed: number = Math.random()): string {
  const s = Math.abs(seed) % 1;
  const idx = Math.floor(s * PERSONALITY_TEMPLATES.length);
  return PERSONALITY_TEMPLATES[Math.min(PERSONALITY_TEMPLATES.length - 1, Math.max(0, idx))];
}
