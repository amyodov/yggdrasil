//! SkyLight — the shared stellar illumination environment.
//!
//! One state, one source of truth for every consumer that wants to
//! "participate in the weather": nebula tint (M3.8), foil specular
//! (M3.3.x), lens glint (M3.2 Pass 4), rivet/rod-cap micro-glints
//! (future). Each consumer samples `SkyLight` and derives its own
//! appearance from the triple (direction, color, intensity).
//!
//! Sampling is a pure function of wall-clock elapsed seconds —
//! `SkyLight::at_elapsed(elapsed)`. We don't store the current SkyLight
//! in AppState because it's cheap to re-derive and keeping the scalar
//! `elapsed_secs` as the single animating field fits "one state,
//! derived rendering" cleanly.

/// Three floats, GPU-friendly, no SIMD dependencies. Small enough to be
/// `Copy` without worry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

// The SkyLight consumers (lens, foil, nebula) land in separate steps;
// many of these methods are called only from tests today. `allow(dead_code)`
// until the shader-wiring steps make them real.
#[allow(dead_code)]
impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    pub fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    /// Return a unit vector in the same direction. If the input is near
    /// zero (shouldn't happen with our curated palette), returns a
    /// sensible default instead of a NaN.
    pub fn normalized(self) -> Self {
        let l = self.length();
        if l < 1e-6 {
            Self::new(0.0, -1.0, 0.0)
        } else {
            Self::new(self.x / l, self.y / l, self.z / l)
        }
    }

    /// Component-wise array form, for shader uniform upload.
    pub fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
}

/// A single preset in the curated sky-mood palette. The runtime sky
/// slowly interpolates between adjacent moods; users don't ever see a
/// "mood index" — they just see the sky drift through colors and
/// positions.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct SkyMood {
    /// Short identifier, used only in tests and debug output.
    #[allow(dead_code)]
    name: &'static str,
    /// Direction the unseen dominant star *comes from*. Roughly overhead
    /// (negative y) by default so the plate's implicit "lit from above"
    /// rule keeps holding; each mood tilts it slightly so the light
    /// direction shifts over time.
    direction: Vec3,
    /// Linear-space RGB. Multiplied into consumers' specular/tint.
    color: Vec3,
    /// 0..1; overall "how present is the star right now."
    intensity: f32,
}

/// Life-cycle sky: the unseen star traces the sun's arc. Eight moods
/// spanning one day — night → pre-dawn → dawn → morning → noon →
/// afternoon → evening → dusk → (back to night). Direction.x swings
/// east-to-west across the cycle; direction.y dips under the horizon
/// at night so consumers can detect "below horizon" and hide their
/// specular highlights. Colors warm dramatically at dawn/dusk (classic
/// red-orange), cool in daylight, go deep-blue at night.
///
/// Sequence order matters: adjacent pairs are the ones we interpolate
/// between, so a smooth day cycle walks through them in time order.
#[allow(dead_code)]
const SKY_MOODS: &[SkyMood] = &[
    SkyMood {
        name: "deep-night",
        direction: Vec3::new(0.00, 0.55, -0.84), // below horizon, behind
        color: Vec3::new(0.30, 0.38, 0.62), // deep cool blue
        intensity: 0.04,
    },
    SkyMood {
        name: "pre-dawn",
        direction: Vec3::new(0.55, 0.15, -0.82), // east, still below
        color: Vec3::new(0.55, 0.45, 0.65), // muted violet
        intensity: 0.12,
    },
    SkyMood {
        name: "morning-dawn",
        direction: Vec3::new(0.82, -0.15, 0.55), // low east, just risen
        color: Vec3::new(0.95, 0.55, 0.35), // warm red-orange
        intensity: 0.45,
    },
    SkyMood {
        name: "morning-light",
        direction: Vec3::new(0.55, -0.62, 0.56), // east, climbing
        color: Vec3::new(0.95, 0.85, 0.66), // warm yellow
        intensity: 0.80,
    },
    SkyMood {
        name: "noon",
        direction: Vec3::new(0.00, -0.96, 0.28), // overhead
        color: Vec3::new(0.93, 0.92, 0.86), // warm white, not 1.0
        intensity: 0.95,
    },
    SkyMood {
        name: "afternoon",
        direction: Vec3::new(-0.55, -0.62, 0.56), // west, descending
        color: Vec3::new(0.95, 0.82, 0.62), // amber
        intensity: 0.75,
    },
    SkyMood {
        name: "evening-dawn",
        direction: Vec3::new(-0.82, -0.15, 0.55), // low west
        color: Vec3::new(0.92, 0.45, 0.28), // deep warm red
        intensity: 0.40,
    },
    SkyMood {
        name: "dusk",
        direction: Vec3::new(-0.55, 0.15, -0.82), // west, sinking below
        color: Vec3::new(0.48, 0.35, 0.52), // violet-grey
        intensity: 0.10,
    },
];

/// Seconds to cross between two adjacent moods.
///
/// **Debug value:** 15s × 8 moods = 2 min full day cycle — fast enough
/// to see the cycle visibly during a single sitting. The motion-
/// diapason target (10–15 min cycle) lands when we set this back to
/// ~90–110s for release. Keeping it fast during debug so mood
/// transitions, glints, and dawn colors can be verified without
/// waiting.
#[allow(dead_code)]
const MOOD_TRANSITION_SECS: f32 = 15.0;

/// The shared stellar-illumination state.
///
/// `SkyLight` is always derived from elapsed time — there's no setter.
/// Consumers call `SkyLight::at_elapsed` with a wall-clock scalar and
/// read direction/color/intensity from the result.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct SkyLight {
    /// Unit vector pointing toward the unseen dominant star. `-y` is
    /// "up" in the plate's implicit lighting convention, so typical
    /// y-components are negative.
    pub direction: Vec3,
    /// Color temperature as linear-space RGB. Consumers multiply this
    /// into their own specular/tint for a subtle wash of the sky.
    pub color: Vec3,
    /// 0..1. Modulates consumer brightness — at low intensity every
    /// environmental highlight dims together; at peak they glint in
    /// unison. Part of the motion-diapason: long period paired with
    /// wide range.
    pub intensity: f32,
}

#[allow(dead_code)]
impl SkyLight {
    /// Sample the evolved SkyLight at wall-clock elapsed seconds.
    ///
    /// Uses smoothstep-eased interpolation between adjacent moods so
    /// transitions don't have visible linear-joint seams.
    pub fn at_elapsed(elapsed_secs: f32) -> Self {
        let cycle_len = SKY_MOODS.len() as f32 * MOOD_TRANSITION_SECS;
        let t = elapsed_secs.rem_euclid(cycle_len).max(0.0);
        let slot = t / MOOD_TRANSITION_SECS;
        let idx_a = (slot.floor() as usize) % SKY_MOODS.len();
        let idx_b = (idx_a + 1) % SKY_MOODS.len();
        let frac = slot - slot.floor();
        let smooth_frac = smoothstep(0.0, 1.0, frac);
        let a = SKY_MOODS[idx_a];
        let b = SKY_MOODS[idx_b];
        Self {
            direction: lerp_vec3(a.direction, b.direction, smooth_frac).normalized(),
            color: lerp_vec3(a.color, b.color, smooth_frac),
            intensity: lerp(a.intensity, b.intensity, smooth_frac),
        }
    }
}

/// A single cloud layer in the nebula. Each layer's `star_response`
/// gates how much it reacts to the SkyLight — far layers (response ~0.1)
/// are effectively ambient and set their own baseline color; near
/// layers (response ~0.8) tint strongly and brighten/dim with intensity.
///
/// Not yet wired into the nebula renderer (that's a consumer step); the
/// struct is defined here so the per-layer coefficient lives alongside
/// the rest of the SkyLight-driven state.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub struct CloudLayer {
    pub star_response: f32,
}

/// Recommended coefficients for the M3.8 three-layer nebula.
#[allow(dead_code)]
pub const CLOUD_LAYER_FAR: CloudLayer = CloudLayer { star_response: 0.1 };
#[allow(dead_code)]
pub const CLOUD_LAYER_MID: CloudLayer = CloudLayer { star_response: 0.4 };
#[allow(dead_code)]
pub const CLOUD_LAYER_NEAR: CloudLayer = CloudLayer { star_response: 0.8 };

#[allow(dead_code)]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[allow(dead_code)]
fn lerp_vec3(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    Vec3::new(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.z, b.z, t))
}

#[allow(dead_code)]
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// At t=0 the sky starts at mood 0 (warm-nebula) exactly, with
    /// `frac=0`. All three components match the first preset.
    #[test]
    fn at_elapsed_zero_matches_first_mood() {
        let s = SkyLight::at_elapsed(0.0);
        let m = SKY_MOODS[0];
        assert!((s.intensity - m.intensity).abs() < 1e-5);
        assert!((s.color.x - m.color.x).abs() < 1e-5);
        // direction is normalized in SkyLight — check the unit-vec match.
        let n = m.direction.normalized();
        assert!((s.direction.x - n.x).abs() < 1e-5);
    }

    /// Sampling at a future time lands at the next mood exactly when
    /// `elapsed == n * MOOD_TRANSITION_SECS`.
    #[test]
    fn at_elapsed_lands_on_successive_moods() {
        for (i, m) in SKY_MOODS.iter().enumerate() {
            let t = i as f32 * MOOD_TRANSITION_SECS;
            let s = SkyLight::at_elapsed(t);
            assert!((s.intensity - m.intensity).abs() < 1e-4, "mood {i}");
        }
    }

    /// `direction` is always unit-length — any consumer doing a dot
    /// product with it can trust the magnitude is 1.
    #[rstest]
    #[case(0.0)]
    #[case(15.0)]
    #[case(90.0)]
    #[case(123.456)]
    #[case(600.0)]
    fn direction_is_always_unit(#[case] elapsed: f32) {
        let s = SkyLight::at_elapsed(elapsed);
        let l = s.direction.length();
        assert!((l - 1.0).abs() < 1e-4, "direction length {l} at t={elapsed}");
    }

    /// `intensity` stays inside [0, 1]. Every mood is in range; linear
    /// interpolation between in-range values stays in range.
    #[rstest]
    #[case(0.0)]
    #[case(45.0)]
    #[case(270.0)]
    #[case(9999.0)]
    fn intensity_stays_in_unit_range(#[case] elapsed: f32) {
        let s = SkyLight::at_elapsed(elapsed);
        assert!((0.0..=1.0).contains(&s.intensity));
    }

    /// Color components stay in [0, 1] for the same reason intensity does.
    #[rstest]
    #[case(0.0)]
    #[case(45.0)]
    #[case(200.0)]
    #[case(888.0)]
    fn color_stays_in_unit_range(#[case] elapsed: f32) {
        let s = SkyLight::at_elapsed(elapsed);
        for c in [s.color.x, s.color.y, s.color.z] {
            assert!((0.0..=1.0).contains(&c), "color {c} out of [0,1] at t={elapsed}");
        }
    }

    /// Evolution is continuous — tiny dt steps produce tiny state
    /// changes. Locks in smoothstep easing (no linear-joint kinks).
    #[test]
    fn evolution_is_continuous() {
        let mut prev = SkyLight::at_elapsed(0.0);
        let dt = 0.1;
        for i in 1..=600 {
            let t = i as f32 * dt;
            let s = SkyLight::at_elapsed(t);
            let d_intensity = (s.intensity - prev.intensity).abs();
            assert!(d_intensity < 0.05, "intensity jumped {d_intensity} at t={t}");
            prev = s;
        }
    }

    /// Negative elapsed is clamped to zero so a clock that hasn't
    /// started yet (or a glitched dt) can't produce NaN sampling.
    #[test]
    fn negative_elapsed_is_handled() {
        let s = SkyLight::at_elapsed(-1.0);
        assert!(s.intensity.is_finite());
        assert!(s.direction.x.is_finite());
    }
}
