//! Colours by how they look: the OKLCH colour space, whose lightness,
//! chroma and hue are perceived as such, which the channels of sRGB and HSV
//! are not. From Björn Ottosson's Oklab
//! (<https://bottosson.github.io/posts/oklab/>), small enough to have here
//! rather than take a colour crate for.

/// The strongest sRGB colour of hue `h` in degrees: the one with the most
/// chroma of any lightness from `min_lightness` (0 to 1) up, which is what
/// a neon colour is. Where a hue is strongest low down, as blue is, the
/// floor keeps it light enough to show on a dark ground. Held back by
/// [`GAMUT_MARGIN`] so that rounding does not push a channel over.
pub fn neon(h: f64, min_lightness: f64) -> slint::Color {
    let (l, c) = (0..=LIGHTNESS_STEPS)
        .map(|step| f64::from(step) / f64::from(LIGHTNESS_STEPS))
        .filter(|&l| l >= min_lightness)
        .map(|l| (l, max_chroma(l, h)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .expect("the lightest step is above any floor");
    encode(oklch_to_linear(l, c * GAMUT_MARGIN, h))
}

/// How many steps the lightness is searched in for the strongest colour.
/// The chroma peaks gently, so steps this fine lose nothing to see.
const LIGHTNESS_STEPS: u32 = 80;

/// The most chroma sRGB can show at lightness `l` and hue `h`, to within
/// [`CHROMA_STEPS`] halvings of [`MAX_CHROMA`].
fn max_chroma(l: f64, h: f64) -> f64 {
    let (mut inside, mut outside) = (0.0, MAX_CHROMA);
    for _ in 0..CHROMA_STEPS {
        let chroma = (inside + outside) / 2.0;
        if in_gamut(oklch_to_linear(l, chroma, h)) {
            inside = chroma;
        } else {
            outside = chroma;
        }
    }
    inside
}

/// More chroma than any sRGB colour has.
const MAX_CHROMA: f64 = 0.4;
/// Halvings of the chroma range, enough to settle it well within a
/// channel's rounding.
const CHROMA_STEPS: u32 = 24;
const GAMUT_MARGIN: f64 = 0.98;

/// The linear sRGB channels of the OKLCH colour with lightness `l`, chroma
/// `c` and hue `h` in degrees.
fn oklch_to_linear(l: f64, c: f64, h: f64) -> [f64; 3] {
    let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
    oklab_to_linear(l, a, b)
}

/// The linear sRGB channels of an Oklab colour, past 0 or 1 when sRGB
/// cannot show it. In `f64`, which the published matrices are given to.
fn oklab_to_linear(l: f64, a: f64, b: f64) -> [f64; 3] {
    // Oklab to the cone responses it is defined over.
    let long = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let medium = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let short = (l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    // Those to linear sRGB.
    [
        4.076_741_662_1 * long - 3.307_711_591_3 * medium + 0.230_969_929_2 * short,
        -1.268_438_004_6 * long + 2.609_757_401_1 * medium - 0.341_319_396_5 * short,
        -0.004_196_086_3 * long - 0.703_418_614_7 * medium + 1.707_614_701_0 * short,
    ]
}

/// Whether sRGB can show the colour with these linear channels.
fn in_gamut(linear: [f64; 3]) -> bool {
    linear.iter().all(|channel| (0.0..=1.0).contains(channel))
}

/// The sRGB colour with these linear channels, each brought to the nearest
/// edge if it is past one.
fn encode(linear: [f64; 3]) -> slint::Color {
    let channel = |linear: f64| {
        let linear = linear.clamp(0.0, 1.0);
        let encoded = if linear <= 0.003_130_8 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (encoded * 255.0).round() as u8
    };
    slint::Color::from_rgb_u8(channel(linear[0]), channel(linear[1]), channel(linear[2]))
}

#[cfg(test)]
mod tests {
    use super::{encode, in_gamut, max_chroma, neon, oklab_to_linear, oklch_to_linear};

    /// The channels of `color`.
    fn rgb(color: slint::Color) -> (u8, u8, u8) {
        (color.red(), color.green(), color.blue())
    }

    #[test]
    fn matches_the_published_reference_values() {
        // From the Oklab post's table of sRGB primaries.
        assert_eq!(rgb(encode(oklab_to_linear(1.0, 0.0, 0.0))), (255, 255, 255));
        assert_eq!(rgb(encode(oklab_to_linear(0.627_955, 0.224_863, 0.125_846))), (255, 0, 0));
        assert_eq!(rgb(encode(oklab_to_linear(0.866_440, -0.233_888, 0.179_498))), (0, 255, 0));
        assert_eq!(rgb(encode(oklab_to_linear(0.452_014, -0.032_457, -0.311_528))), (0, 0, 255));
    }

    #[test]
    fn a_hue_turns_the_colour_round() {
        let (r, g, b) = (neon(25.0, 0.0), neon(145.0, 0.0), neon(265.0, 0.0));
        assert!(r.red() > r.green() && r.red() > r.blue(), "{r:?}");
        assert!(g.green() > g.red() && g.green() > g.blue(), "{g:?}");
        assert!(b.blue() > b.red() && b.blue() > b.green(), "{b:?}");
    }

    #[test]
    fn a_neon_colour_touches_the_edge_of_the_gamut() {
        // The strongest colour of a hue has a channel at full or at none,
        // give or take the margin kept from the edge.
        for hue in (0..360).step_by(30) {
            let color = neon(f64::from(hue), 0.0);
            let channels = [color.red(), color.green(), color.blue()];
            assert!(
                channels.iter().any(|&channel| channel >= 245 || channel <= 10),
                "{hue}: {channels:?}"
            );
        }
        // Pure green is where its hue is strongest.
        let green = neon(142.0, 0.0);
        assert!(green.green() >= 250 && green.red() < 60 && green.blue() < 60, "{green:?}");
    }

    #[test]
    fn a_lightness_floor_lifts_a_hue_that_is_strongest_low_down() {
        let deep = neon(265.0, 0.0);
        let lifted = neon(265.0, 0.6);
        assert!(lifted.red() > deep.red() && lifted.green() > deep.green(), "{deep:?} {lifted:?}");
        // A hue strongest above the floor is unchanged by it.
        assert_eq!(rgb(neon(145.0, 0.6)), rgb(neon(145.0, 0.0)));
    }

    #[test]
    fn the_most_chroma_sits_just_inside_the_gamut() {
        for hue in (0..360).step_by(15) {
            let hue = f64::from(hue);
            let chroma = max_chroma(0.7, hue);
            assert!(chroma > 0.1, "{hue}: {chroma}");
            assert!(in_gamut(oklch_to_linear(0.7, chroma, hue)), "{hue}: {chroma}");
            assert!(!in_gamut(oklch_to_linear(0.7, chroma * 1.01, hue)), "{hue}: {chroma}");
        }
        // A tighter hue allows less than a wider one.
        assert!(max_chroma(0.7, 265.0) < max_chroma(0.7, 145.0));
    }

    #[test]
    fn a_colour_past_the_gamut_is_brought_to_its_edge() {
        assert_eq!(encode(oklch_to_linear(0.7, 0.4, 265.0)).blue(), 255);
    }
}
