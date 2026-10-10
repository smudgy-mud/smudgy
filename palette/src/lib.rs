//! Ordinal color sequences. A seed fixes the first color; every later
//! position takes a color planned around it, so the nth item of any list can
//! default to the nth color: a map's Secrets in alphabetical order, seeded by
//! the map's name and its first Secret's; a clan's groups, seeded by the
//! clan's name.
//!
//! A sequence has [`LEN`] colors, then repeats. Position 0 is the seed's own
//! hue. Positions 1 to 4 are coordinated with it: its complement, the two
//! colors completing a square around it, and a darker neighbor between the
//! first and third. Positions 5 to 7 fill the square's diagonals. The later
//! positions were chosen once, offline, each as far as possible from every
//! earlier position at any seed: across all seeds the first twenty colors
//! stay at least 0.08 apart (Euclidean distance in the OK Lab space) and all
//! thirty at least 0.068.
//!
//! Each position is an offset from the seed's hue with its own lightness and
//! chroma in OKLCH, brought into sRGB by lowering chroma until it fits. The
//! table and the arithmetic are all there is, so another implementation (a
//! server choosing a default) reproduces these colors from the same table;
//! the tests pin golden values.

use std::fmt;

/// How many colors a sequence holds before it repeats.
pub const LEN: usize = 30;

/// Each position's hue offset from the seed's hue (degrees), OKLCH lightness
/// and chroma.
const POSITIONS: [(f64, f64, f64); LEN] = [
    (0.0, 0.76, 0.15),
    (180.0, 0.76, 0.15),
    (90.0, 0.81, 0.13),
    (270.0, 0.69, 0.16),
    (45.0, 0.66, 0.15),
    (225.0, 0.82, 0.12),
    (135.0, 0.70, 0.15),
    (315.0, 0.80, 0.13),
    (202.5, 0.62, 0.16),
    (337.5, 0.62, 0.16),
    (157.5, 0.86, 0.16),
    (22.5, 0.86, 0.12),
    (97.5, 0.62, 0.16),
    (157.5, 0.62, 0.16),
    (52.5, 0.74, 0.08),
    (277.5, 0.86, 0.16),
    (292.5, 0.62, 0.12),
    (247.5, 0.62, 0.16),
    (240.0, 0.74, 0.16),
    (7.5, 0.68, 0.16),
    (105.0, 0.74, 0.16),
    (180.0, 0.68, 0.12),
    (315.0, 0.68, 0.16),
    (225.0, 0.68, 0.08),
    (135.0, 0.80, 0.12),
    (292.5, 0.74, 0.08),
    (82.5, 0.68, 0.08),
    (330.0, 0.74, 0.16),
    (262.5, 0.80, 0.16),
    (210.0, 0.74, 0.16),
];

/// Bisection steps when lowering chroma into sRGB.
const FIT_STEPS: u32 = 24;

/// An sRGB color with 8-bit channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl fmt::Display for Rgb {
    /// `#rrggbb`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// The colors grown from one seed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorSequence {
    hue: f64,
}

impl ColorSequence {
    /// The sequence whose first color has `hue` (degrees).
    #[must_use]
    pub fn from_hue(hue: f64) -> Self {
        Self {
            hue: hue.rem_euclid(360.0),
        }
    }

    /// The sequence seeded by `parts`, joined by NUL bytes: FNV-1a (32-bit)
    /// over their UTF-8, modulo 3600, gives the first hue in tenths of a
    /// degree.
    #[must_use]
    pub fn seeded(parts: &[&str]) -> Self {
        let mut hash: u32 = 0x811c_9dc5;
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                hash = fnv1a_byte(hash, 0);
            }
            for byte in part.bytes() {
                hash = fnv1a_byte(hash, byte);
            }
        }
        Self::from_hue(f64::from(hash % 3600) / 10.0)
    }

    /// The first color's hue, in degrees.
    #[must_use]
    pub fn hue(&self) -> f64 {
        self.hue
    }

    /// The color at `index`, repeating every [`LEN`] positions.
    #[must_use]
    pub fn color(&self, index: usize) -> Rgb {
        let (offset, lightness, chroma) = POSITIONS[index % LEN];
        oklch_to_rgb(lightness, chroma, (self.hue + offset).rem_euclid(360.0))
    }

    /// The sequence's [`LEN`] colors in order.
    pub fn colors(&self) -> impl Iterator<Item = Rgb> + '_ {
        (0..LEN).map(|index| self.color(index))
    }

    /// The first color of the sequence none of `used` takes, so a new item
    /// (a clan's next group) gets the next free color and a deleted one frees
    /// its color; once all [`LEN`] are taken, the sequence repeats.
    #[must_use]
    pub fn first_unused(&self, used: &[Rgb]) -> Rgb {
        self.colors()
            .find(|color| !used.contains(color))
            .unwrap_or_else(|| self.color(used.len()))
    }
}

fn fnv1a_byte(hash: u32, byte: u8) -> u32 {
    (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
}

/// Linear-light sRGB for an OKLCH color, which may fall outside `0..=1`.
#[allow(clippy::many_single_char_names)] // the OK Lab conversion's own names
fn oklch_to_linear(lightness: f64, chroma: f64, hue: f64) -> [f64; 3] {
    let radians = hue * std::f64::consts::PI / 180.0;
    let a = chroma * radians.cos();
    let b = chroma * radians.sin();
    let l = lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m = lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s = lightness - 0.089_484_177_5 * a - 1.291_485_548 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701 * s,
    ]
}

fn fits(linear: [f64; 3]) -> bool {
    linear.iter().all(|channel| (0.0..=1.0).contains(channel))
}

/// The OKLCH color in sRGB, its chroma lowered by bisection until it fits.
fn oklch_to_rgb(lightness: f64, chroma: f64, hue: f64) -> Rgb {
    let mut linear = oklch_to_linear(lightness, chroma, hue);
    if !fits(linear) {
        let (mut low, mut high) = (0.0, chroma);
        for _ in 0..FIT_STEPS {
            let middle = f64::midpoint(low, high);
            if fits(oklch_to_linear(lightness, middle, hue)) {
                low = middle;
            } else {
                high = middle;
            }
        }
        linear = oklch_to_linear(lightness, low, hue);
    }
    let [r, g, b] = linear.map(encode);
    Rgb { r, g, b }
}

/// One linear channel, gamma-encoded and rounded to 8 bits.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn encode(channel: f64) -> u8 {
    let channel = channel.clamp(0.0, 1.0);
    let encoded = if channel <= 0.003_130_8 {
        12.92 * channel
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hexes(sequence: ColorSequence) -> Vec<String> {
        sequence.colors().map(|color| color.to_string()).collect()
    }

    /// Golden values from the reference implementation; another
    /// implementation must produce the same.
    #[test]
    fn sequences_match_the_reference() {
        assert_eq!(
            hexes(ColorSequence::from_hue(0.0)),
            [
                "#fc85ad", "#00cdb4", "#e1bd53", "#7593fe", "#db703b", "#60d4fe", "#74b24c",
                "#dda4f7", "#0098a2", "#c15ba8", "#64f0a7", "#ffbeba", "#9c8600", "#00a062",
                "#d49d7c", "#c5cdff", "#8977c8", "#128be0", "#39b5ff", "#e76886", "#baaf00",
                "#16b09b", "#bd76db", "#5ea3be", "#9ccf7f", "#aca2da", "#b1945e", "#e282da",
                "#9bbeff", "#00c0d7",
            ]
        );
        assert_eq!(
            hexes(ColorSequence::from_hue(123.4)),
            [
                "#9fc04a", "#c598fe", "#39d6f3", "#ed7056", "#00ac83", "#fea4d1", "#619efa",
                "#ebb352", "#b65fb9", "#988700", "#c8ccff", "#9de6a1", "#0095b6", "#7777e3",
                "#71bca9", "#ffc0aa", "#bc7136", "#d3556e", "#fb7ba0", "#74ab33", "#00bbf2",
                "#a885d5", "#c78c00", "#be84a1", "#95c0ff", "#d29e7a", "#56a7b0", "#cba700",
                "#ffa097", "#e581d5",
            ]
        );
    }

    #[test]
    fn a_seed_hashes_its_parts_joined_by_nul() {
        let secrets = ColorSequence::seeded(&["Midgaard", "Behind The Bookcase"]);
        assert!((secrets.hue() - 71.8).abs() < 1e-9);
        assert_eq!(secrets.color(0).to_string(), "#eb9f2c");
        assert_eq!(secrets.color(4).to_string(), "#8d9d09");
        let clan = ColorSequence::seeded(&["The Wardens"]);
        assert!((clan.hue() - 74.6).abs() < 1e-9);
        assert_eq!(clan.color(1).to_string(), "#78b4ff");
        assert_ne!(
            ColorSequence::seeded(&["ab", "c"]),
            ColorSequence::seeded(&["a", "bc"]),
            "the separator keeps parts apart"
        );
    }

    #[test]
    fn a_new_item_takes_the_first_free_color() {
        let groups = ColorSequence::seeded(&["Lantern Company"]);
        assert_eq!(groups.first_unused(&[]), groups.color(0));
        assert_eq!(
            groups.first_unused(&[groups.color(0), groups.color(1)]),
            groups.color(2)
        );
        // Deleting the first group frees its color for the next one.
        assert_eq!(groups.first_unused(&[groups.color(1)]), groups.color(0));
        let all: Vec<_> = groups.colors().collect();
        assert_eq!(groups.first_unused(&all), groups.color(0));
    }

    #[test]
    fn the_sequence_repeats_after_its_last_color() {
        let sequence = ColorSequence::from_hue(200.0);
        assert_eq!(sequence.color(LEN), sequence.color(0));
        assert_eq!(sequence.color(LEN + 7), sequence.color(7));
        assert_eq!(ColorSequence::from_hue(-160.0), sequence);
    }

    #[test]
    fn every_seed_gets_thirty_distinct_colors() {
        for tenth in 0..3600 {
            let colors: Vec<_> = ColorSequence::from_hue(f64::from(tenth) / 10.0)
                .colors()
                .collect();
            for (i, color) in colors.iter().enumerate() {
                assert!(
                    !colors[..i].contains(color),
                    "hue {} repeats {color} at position {i}",
                    f64::from(tenth) / 10.0
                );
            }
        }
    }
}
