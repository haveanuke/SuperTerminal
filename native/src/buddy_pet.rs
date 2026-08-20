//! The buddy pet: a deterministic ASCII companion (art and generation ported
//! verbatim from the old app's claude-buddy engine).
//!
//! The pet supplies the personality — species, rarity, stats, a name — while
//! everything it *says* comes from the reviewer (`superterminal_core::buddy`).
//! Bones are derived deterministically from a saved `user_id`, so only the id,
//! name and pet count need persisting.

use serde::{Deserialize, Serialize};

pub const SALT: &str = "superterminal-buddy-2026";

pub const SPECIES: [&str; 18] = [
    "duck", "goose", "blob", "cat", "dragon", "octopus", "owl", "penguin", "turtle", "snail",
    "ghost", "axolotl", "capybara", "cactus", "robot", "rabbit", "mushroom", "chonk",
];

pub const RARITIES: [&str; 5] = ["common", "uncommon", "rare", "epic", "legendary"];
const RARITY_WEIGHTS: [f64; 5] = [60.0, 25.0, 10.0, 4.0, 1.0];
const RARITY_FLOOR: [i64; 5] = [5, 15, 25, 35, 50];
pub const RARITY_COLOR: [u32; 5] = [0x999999, 0x4eba65, 0xb1b9f9, 0xaf87ff, 0xffc107];

pub const STAT_NAMES: [&str; 5] = ["DEBUGGING", "PATIENCE", "CHAOS", "WISDOM", "SNARK"];

const EYES: [&str; 6] = ["\u{b7}", "\u{2726}", "\u{d7}", "\u{25c9}", "@", "\u{b0}"];

const HATS: [&str; 8] = [
    "none",
    "crown",
    "tophat",
    "propeller",
    "halo",
    "wizard",
    "beanie",
    "tinyduck",
];

const HAT_ART: [&str; 8] = [
    "",
    r"   \^^^/    ",
    r"   [___]    ",
    r"    -+-     ",
    r"   (   )    ",
    r"    /^\     ",
    r"   (___)    ",
    r"    ,>      ",
];

const FALLBACK_NAMES: [&str; 24] = [
    "Crumpet", "Soup", "Pickle", "Biscuit", "Moth", "Gravy", "Nugget", "Sprocket", "Miso",
    "Waffle", "Pixel", "Ember", "Thimble", "Marble", "Sesame", "Cobalt", "Rusty", "Nimbus",
    "Mochi", "Bean", "Pepper", "Turnip", "Clover", "Fizz",
];

/// 3 animation frames x 5 lines per species; `{E}` is the eye placeholder.
#[rustfmt::skip]
const SPECIES_ART: [[[&str; 5]; 3]; 18] = [
    // duck
    [
        ["            ", "    __      ", "  <({E} )___  ", "   (  ._>   ", r"    `--'    "],
        ["            ", "    __      ", "  <({E} )___  ", "   (  ._>   ", r"    `--'~   "],
        ["            ", "    __      ", "  <({E} )___  ", "   (  .__>  ", r"    `--'    "],
    ],
    // goose
    [
        ["            ", "     ({E}>    ", "     ||     ", "   _(__)_   ", "    ^^^^    "],
        ["            ", "    ({E}>     ", "     ||     ", "   _(__)_   ", "    ^^^^    "],
        ["            ", "     ({E}>>   ", "     ||     ", "   _(__)_   ", "    ^^^^    "],
    ],
    // blob
    [
        ["            ", "   .----.   ", "  ( {E}  {E} )  ", "  (      )  ", r"   `----'   "],
        ["            ", "  .------.  ", " (  {E}  {E}  ) ", " (        ) ", r"  `------'  "],
        ["            ", "    .--.    ", "   ({E}  {E})   ", "   (    )   ", r"    `--'    "],
    ],
    // cat
    [
        ["            ", r"   /\_/\    ", "  ( {E}   {E})  ", "  (  \u{3c9}  )   ", r#"  (")_(")   "#],
        ["            ", r"   /\_/\    ", "  ( {E}   {E})  ", "  (  \u{3c9}  )   ", r#"  (")_(")~  "#],
        ["            ", r"   /\-/\    ", "  ( {E}   {E})  ", "  (  \u{3c9}  )   ", r#"  (")_(")   "#],
    ],
    // dragon
    [
        ["            ", r"  /^\  /^\  ", " <  {E}  {E}  > ", " (   ~~   ) ", r"  `-vvvv-'  "],
        ["            ", r"  /^\  /^\  ", " <  {E}  {E}  > ", " (        ) ", r"  `-vvvv-'  "],
        ["   ~    ~   ", r"  /^\  /^\  ", " <  {E}  {E}  > ", " (   ~~   ) ", r"  `-vvvv-'  "],
    ],
    // octopus
    [
        ["            ", "   .----.   ", "  ( {E}  {E} )  ", "  (______)  ", r"  /\/\/\/\  "],
        ["            ", "   .----.   ", "  ( {E}  {E} )  ", "  (______)  ", r"  \/\/\/\/  "],
        ["     o      ", "   .----.   ", "  ( {E}  {E} )  ", "  (______)  ", r"  /\/\/\/\  "],
    ],
    // owl
    [
        ["            ", r"   /\  /\   ", "  (({E})({E}))  ", "  (  ><  )  ", r"   `----'   "],
        ["            ", r"   /\  /\   ", "  (({E})({E}))  ", "  (  ><  )  ", "   .----.   "],
        ["            ", r"   /\  /\   ", "  (({E})(-))  ", "  (  ><  )  ", r"   `----'   "],
    ],
    // penguin
    [
        ["            ", "  .---.     ", "  ({E}>{E})     ", r" /(   )\    ", r"  `---'     "],
        ["            ", "  .---.     ", "  ({E}>{E})     ", " |(   )|    ", r"  `---'     "],
        ["  .---.     ", "  ({E}>{E})     ", r" /(   )\    ", r"  `---'     ", "   ~ ~      "],
    ],
    // turtle
    [
        ["            ", "   _,--._   ", "  ( {E}  {E} )  ", r" /[______]\ ", "  ``    ``  "],
        ["            ", "   _,--._   ", "  ( {E}  {E} )  ", r" /[______]\ ", "   ``  ``   "],
        ["            ", "   _,--._   ", "  ( {E}  {E} )  ", r" /[======]\ ", "  ``    ``  "],
    ],
    // snail
    [
        ["            ", " {E}    .--.  ", r"  \  ( @ )  ", r"   \_`--'   ", "  ~~~~~~~   "],
        ["            ", "  {E}   .--.  ", "  |  ( @ )  ", r"   \_`--'   ", "  ~~~~~~~   "],
        ["            ", " {E}    .--.  ", r"  \  ( @  ) ", r"   \_`--'   ", "   ~~~~~~   "],
    ],
    // ghost
    [
        ["            ", "   .----.   ", r"  / {E}  {E} \  ", "  |      |  ", "  ~`~``~`~  "],
        ["            ", "   .----.   ", r"  / {E}  {E} \  ", "  |      |  ", "  `~`~~`~`  "],
        ["    ~  ~    ", "   .----.   ", r"  / {E}  {E} \  ", "  |      |  ", "  ~~`~~`~~  "],
    ],
    // axolotl
    [
        ["            ", "}~(______)~{", "}~({E} .. {E})~{", "  ( .--. )  ", r"  (_/  \_)  "],
        ["            ", "~}(______){~", "~}({E} .. {E}){~", "  ( .--. )  ", r"  (_/  \_)  "],
        ["            ", "}~(______)~{", "}~({E} .. {E})~{", "  (  --  )  ", r"  ~_/  \_~  "],
    ],
    // capybara
    [
        ["            ", "  n______n  ", " ( {E}    {E} ) ", " (   oo   ) ", r"  `------'  "],
        ["            ", "  n______n  ", " ( {E}    {E} ) ", " (   Oo   ) ", r"  `------'  "],
        ["    ~  ~    ", "  u______n  ", " ( {E}    {E} ) ", " (   oo   ) ", r"  `------'  "],
    ],
    // cactus
    [
        ["            ", " n  ____  n ", " | |{E}  {E}| | ", " |_|    |_| ", "   |    |   "],
        ["            ", "    ____    ", " n |{E}  {E}| n ", " |_|    |_| ", "   |    |   "],
        [" n        n ", " |  ____  | ", " | |{E}  {E}| | ", " |_|    |_| ", "   |    |   "],
    ],
    // robot
    [
        ["            ", "   .[||].   ", "  [ {E}  {E} ]  ", "  [ ==== ]  ", r"  `------'  "],
        ["            ", "   .[||].   ", "  [ {E}  {E} ]  ", "  [ -==- ]  ", r"  `------'  "],
        ["     *      ", "   .[||].   ", "  [ {E}  {E} ]  ", "  [ ==== ]  ", r"  `------'  "],
    ],
    // rabbit
    [
        ["            ", r"   (\__/)   ", "  ( {E}  {E} )  ", " =(  ..  )= ", r#"  (")__(")  "#],
        ["            ", "   (|__/)   ", "  ( {E}  {E} )  ", " =(  ..  )= ", r#"  (")__(")  "#],
        ["            ", r"   (\__/)   ", "  ( {E}  {E} )  ", " =( .  . )= ", r#"  (")__(")  "#],
    ],
    // mushroom
    [
        ["            ", " .-o-OO-o-. ", "(__________)", "   |{E}  {E}|   ", "   |____|   "],
        ["            ", " .-O-oo-O-. ", "(__________)", "   |{E}  {E}|   ", "   |____|   "],
        ["   . o  .   ", " .-o-OO-o-. ", "(__________)", "   |{E}  {E}|   ", "   |____|   "],
    ],
    // chonk
    [
        ["            ", r"  /\    /\  ", " ( {E}    {E} ) ", " (   ..   ) ", r"  `------'  "],
        ["            ", r"  /\    /|  ", " ( {E}    {E} ) ", " (   ..   ) ", r"  `------'  "],
        ["            ", r"  /\    /\  ", " ( {E}    {E} ) ", " (   ..   ) ", r"  `------'~ "],
    ],
];

/// cyrb53 string hash, ported bit-for-bit (JS `Math.imul` = wrapping i32
/// multiply, `charCodeAt` = UTF-16 code units).
pub fn hash_string(s: &str, seed: u32) -> u32 {
    let mut h1: u32 = 0xdead_beef ^ seed;
    let mut h2: u32 = 0x41c6_ce57 ^ seed;
    for unit in s.encode_utf16() {
        let ch = u32::from(unit);
        h1 = (h1 ^ ch).wrapping_mul(2_654_435_761);
        h2 = (h2 ^ ch).wrapping_mul(1_597_334_677);
    }
    h1 = (h1 ^ (h1 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h2 ^ (h2 >> 13)).wrapping_mul(3_266_489_909);
    h2 = (h2 ^ (h2 >> 16)).wrapping_mul(2_246_822_507)
        ^ (h1 ^ (h1 >> 13)).wrapping_mul(3_266_489_909);
    h2 ^ (h1 & 0xffff)
}

/// mulberry32 PRNG, ported bit-for-bit from the old engine.
struct Mulberry32(u32);

impl Mulberry32 {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b_79f5);
        let a = self.0;
        let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }

    fn pick(&mut self, len: usize) -> usize {
        ((self.next() * len as f64) as usize).min(len - 1)
    }
}

/// Everything derivable from the user id. Indices into the const tables.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bones {
    pub rarity: usize,
    pub species: usize,
    pub eye: usize,
    pub hat: usize,
    pub shiny: bool,
    pub stats: [i64; 5],
    pub peak: usize,
    pub dump: usize,
}

fn roll_rarity(rng: &mut Mulberry32) -> usize {
    let total: f64 = RARITY_WEIGHTS.iter().sum();
    let mut roll = rng.next() * total;
    for (index, weight) in RARITY_WEIGHTS.iter().enumerate() {
        roll -= weight;
        if roll < 0.0 {
            return index;
        }
    }
    0
}

pub fn generate_bones(user_id: &str) -> Bones {
    let mut rng = Mulberry32(hash_string(&format!("{user_id}{SALT}"), 0));
    let rarity = roll_rarity(&mut rng);
    let species = rng.pick(SPECIES.len());
    let eye = rng.pick(EYES.len());
    // Matches the JS call order exactly: commons skip the hat roll entirely.
    let hat = if rarity == 0 { 0 } else { rng.pick(HATS.len()) };
    let shiny = rng.next() < 0.01;

    let peak = rng.pick(STAT_NAMES.len());
    let mut dump = rng.pick(STAT_NAMES.len());
    while dump == peak {
        dump = rng.pick(STAT_NAMES.len());
    }

    let floor = RARITY_FLOOR[rarity];
    let mut stats = [0i64; 5];
    for (index, stat) in stats.iter_mut().enumerate() {
        *stat = if index == peak {
            (floor + 50 + (rng.next() * 30.0) as i64).min(100)
        } else if index == dump {
            (floor - 10 + (rng.next() * 15.0) as i64).max(1)
        } else {
            floor + (rng.next() * 40.0) as i64
        };
    }

    Bones {
        rarity,
        species,
        eye,
        hat,
        shiny,
        stats,
        peak,
        dump,
    }
}

/// The persisted slice of a companion; bones regenerate from `user_id`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CompanionSave {
    pub user_id: String,
    pub name: String,
    pub pet_count: u32,
    pub hatched_at: u64,
}

#[derive(Clone, Debug)]
pub struct Companion {
    pub save: CompanionSave,
    pub bones: Bones,
}

fn fallback_name(user_id: &str) -> &'static str {
    let seed = f64::from(hash_string(&format!("{user_id}:name"), 0) % 100_000) / 100_000.0;
    let index = (seed * FALLBACK_NAMES.len() as f64) as usize;
    FALLBACK_NAMES[index.min(FALLBACK_NAMES.len() - 1)]
}

fn random_user_id() -> String {
    let mut bytes = [0u8; 16];
    let read_ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_ok();
    if !read_ok {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&nanos.to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Companion {
    pub fn hatch() -> Companion {
        let user_id = random_user_id();
        let bones = generate_bones(&user_id);
        let name = fallback_name(&user_id).to_string();
        let hatched_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Companion {
            save: CompanionSave {
                user_id,
                name,
                pet_count: 0,
                hatched_at,
            },
            bones,
        }
    }

    pub fn from_save(mut save: CompanionSave) -> Companion {
        if save.user_id.is_empty() {
            return Companion::hatch();
        }
        if save.name.trim().is_empty() {
            save.name = fallback_name(&save.user_id).to_string();
        }
        let bones = generate_bones(&save.user_id);
        Companion { save, bones }
    }

    pub fn species_name(&self) -> &'static str {
        SPECIES[self.bones.species]
    }

    pub fn rarity_name(&self) -> &'static str {
        RARITIES[self.bones.rarity]
    }

    pub fn rarity_color(&self) -> u32 {
        RARITY_COLOR[self.bones.rarity]
    }

    pub fn stars(&self) -> String {
        "\u{2605}".repeat(self.bones.rarity + 1)
    }

    /// One 5-line art frame with the eye substituted and the hat applied.
    pub fn art_frame(&self, frame: usize, blink: bool) -> Vec<String> {
        let frames = &SPECIES_ART[self.bones.species];
        let art = &frames[frame % frames.len()];
        let eye = if blink { "-" } else { EYES[self.bones.eye] };
        let mut lines: Vec<String> = art.iter().map(|line| line.replace("{E}", eye)).collect();
        let hat_line = HAT_ART[self.bones.hat];
        if !hat_line.is_empty() && lines[0].trim().is_empty() {
            lines[0] = hat_line.to_string();
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_with_spread() {
        assert_ne!(hash_string("a", 0), hash_string("b", 0));
        assert_ne!(hash_string("a", 0), hash_string("a", 1));
        assert_eq!(hash_string("buddy", 0), hash_string("buddy", 0));
    }

    #[test]
    fn bones_are_deterministic_and_in_range() {
        let a = generate_bones("test-user-1");
        let b = generate_bones("test-user-1");
        assert_eq!(a, b);
        assert!(a.rarity < RARITIES.len());
        assert!(a.species < SPECIES.len());
        assert!(a.eye < EYES.len());
        assert!(a.hat < HATS.len());
        assert_ne!(a.peak, a.dump);
        for stat in a.stats {
            assert!((1..=100).contains(&stat), "stat out of range: {stat}");
        }
        assert_ne!(generate_bones("test-user-1"), generate_bones("test-user-2"));
    }

    #[test]
    fn common_rarity_never_gets_a_hat() {
        for i in 0..200 {
            let bones = generate_bones(&format!("user-{i}"));
            if bones.rarity == 0 {
                assert_eq!(bones.hat, 0);
            }
        }
    }

    #[test]
    fn art_frame_substitutes_eyes_and_hats() {
        let mut companion = Companion::from_save(CompanionSave {
            user_id: "art-test".into(),
            name: "Test".into(),
            pet_count: 0,
            hatched_at: 0,
        });
        companion.bones.species = 2; // blob: eyes on line 2, empty line 0
        companion.bones.hat = 1; // crown
        let lines = companion.art_frame(0, false);
        assert_eq!(lines.len(), 5);
        assert!(!lines.iter().any(|l| l.contains("{E}")));
        assert_eq!(lines[0], HAT_ART[1]);
        let blink = companion.art_frame(0, true);
        assert!(blink[2].contains('-'));
    }

    #[test]
    fn every_species_has_three_five_line_frames() {
        for frames in &SPECIES_ART {
            for frame in frames {
                assert_eq!(frame.len(), 5);
            }
        }
    }

    #[test]
    fn from_save_heals_missing_names_and_ids() {
        let healed = Companion::from_save(CompanionSave {
            user_id: "abc".into(),
            name: "  ".into(),
            pet_count: 3,
            hatched_at: 1,
        });
        assert!(!healed.save.name.trim().is_empty());
        assert_eq!(healed.save.pet_count, 3);
        let hatched = Companion::from_save(CompanionSave::default());
        assert!(!hatched.save.user_id.is_empty());
    }

    #[test]
    fn hatch_produces_unique_ids() {
        assert_ne!(
            Companion::hatch().save.user_id,
            Companion::hatch().save.user_id
        );
    }
}
