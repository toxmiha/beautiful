use serde::{Deserialize, Serialize};

/// stabilizer presets: `0`–`15` (subtle) and `S1`–`S6` (slow / heavy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StabilizerPreset {
    /// No smoothing.
    Off,
    /// Levels 1..=15 — light lag, good for sketching/inking.
    Level(u8),
    /// Slow modes S1..=S6 — stronger delay, cleaner curves.
    Slow(u8),
}

impl Default for StabilizerPreset {
    fn default() -> Self {
        // Stabilizer starts at 0 — raw mouse, zero lag.
        Self::Off
    }
}

impl StabilizerPreset {
    pub fn level(n: u8) -> Self {
        match n {
            0 => Self::Off,
            1..=15 => Self::Level(n),
            _ => Self::Level(15),
        }
    }

    pub fn slow(n: u8) -> Self {
        Self::Slow(n.clamp(1, 6))
    }

    pub fn label(self) -> String {
        match self {
            Self::Off => "0".into(),
            Self::Level(n) => n.to_string(),
            Self::Slow(n) => format!("S{n}"),
        }
    }

    pub fn all() -> [Self; 22] {
        [
            Self::Off,
            Self::Level(1),
            Self::Level(2),
            Self::Level(3),
            Self::Level(4),
            Self::Level(5),
            Self::Level(6),
            Self::Level(7),
            Self::Level(8),
            Self::Level(9),
            Self::Level(10),
            Self::Level(11),
            Self::Level(12),
            Self::Level(13),
            Self::Level(14),
            Self::Level(15),
            Self::Slow(1),
            Self::Slow(2),
            Self::Slow(3),
            Self::Slow(4),
            Self::Slow(5),
            Self::Slow(6),
        ]
    }

    /// Normalized 0..=1 strength used by the smoother.
    /// 0–15 stay milder; S1–S6 continue past 15 with heavier lag.
    pub fn strength(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Level(n) => {
                let t = (n as f32 / 15.0).clamp(0.0, 1.0);
                // Cap normal modes ~0.62 so they feel lighter than S-modes.
                t * 0.62
            }
            Self::Slow(n) => {
                let t = ((n as f32 - 1.0) / 5.0).clamp(0.0, 1.0);
                // S1 ≈ 0.68 … S6 ≈ 0.96
                0.68 + t * 0.28
            }
        }
    }

    /// Catch-up exponent: S-modes pull slower toward the cursor.
    pub fn catch_up_power(self) -> f32 {
        match self {
            Self::Off | Self::Level(_) => 1.6,
            Self::Slow(_) => 2.4,
        }
    }

    pub fn is_slow(self) -> bool {
        matches!(self, Self::Slow(_))
    }
}

/// Line stabilizer (lazy mouse / delay smoothing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stabilizer {
    #[serde(default)]
    pub preset: StabilizerPreset,
    /// Kept for compatibility / fine-tune display; driven by `preset`.
    #[serde(default)]
    pub strength: f32,
    #[serde(skip)]
    smoothed: Option<(f32, f32)>,
}

impl Default for Stabilizer {
    fn default() -> Self {
        Self::from_preset(StabilizerPreset::default())
    }
}

impl Stabilizer {
    pub fn new(strength: f32) -> Self {
        let strength = strength.clamp(0.0, 1.0);
        // Map continuous strength onto nearest preset for legacy callers/tests.
        let preset = if strength <= 0.001 {
            StabilizerPreset::Off
        } else if strength <= 0.62 {
            let n = ((strength / 0.62) * 15.0).round() as u8;
            StabilizerPreset::level(n.max(1))
        } else {
            let t = ((strength - 0.68) / 0.28).clamp(0.0, 1.0);
            let n = (1.0 + t * 5.0).round() as u8;
            StabilizerPreset::slow(n.clamp(1, 6))
        };
        Self::from_preset(preset)
    }

    pub fn from_preset(preset: StabilizerPreset) -> Self {
        Self {
            preset,
            strength: preset.strength(),
            smoothed: None,
        }
    }

    pub fn set_preset(&mut self, preset: StabilizerPreset) {
        self.preset = preset;
        self.strength = preset.strength();
        // Keep smoothed point — switching mid-stroke is fine; reset happens on pen up.
    }

    pub fn process(&mut self, x: f32, y: f32) -> (f32, f32) {
        // Stabilizer 0: identity — no lerp, EMA, springs, or leftover state.
        if matches!(self.preset, StabilizerPreset::Off) || self.preset.strength() <= 0.001 {
            self.smoothed = None;
            return (x, y);
        }

        let strength = self.preset.strength();
        let power = self.preset.catch_up_power();
        let catch_up = (1.0 - strength).powf(power).clamp(0.02, 1.0);

        match self.smoothed {
            None => {
                self.smoothed = Some((x, y));
                (x, y)
            }
            Some((sx, sy)) => {
                let nx = sx + (x - sx) * catch_up;
                let ny = sy + (y - sy) * catch_up;
                self.smoothed = Some((nx, ny));
                (nx, ny)
            }
        }
    }

    #[inline]
    pub fn is_off(&self) -> bool {
        matches!(self.preset, StabilizerPreset::Off) || self.preset.strength() <= 0.001
    }

    pub fn reset(&mut self) {
        self.smoothed = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_strength_passes_through() {
        let mut s = Stabilizer::from_preset(StabilizerPreset::Off);
        assert_eq!(s.process(10.0, 20.0), (10.0, 20.0));
        assert_eq!(s.process(50.0, 60.0), (50.0, 60.0));
    }

    #[test]
    fn high_strength_lags_behind() {
        let mut s = Stabilizer::from_preset(StabilizerPreset::Slow(6));
        s.process(0.0, 0.0);
        let (x, _) = s.process(100.0, 0.0);
        assert!(x < 100.0);
    }

    #[test]
    fn off_clears_smoothed_state() {
        let mut s = Stabilizer::from_preset(StabilizerPreset::Level(10));
        s.process(0.0, 0.0);
        s.process(100.0, 0.0);
        s.set_preset(StabilizerPreset::Off);
        assert_eq!(s.process(50.0, 50.0), (50.0, 50.0));
        assert_eq!(s.process(1.0, 2.0), (1.0, 2.0));
    }
}
