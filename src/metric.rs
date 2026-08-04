use crate::run::RunOutput;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Unit {
    pub scale: f64,
    pub symbol: &'static str,
}

pub trait Metric: Copy {
    const LOWER: &'static str;
    const BASE_UNIT: &'static str;

    fn read(output: &RunOutput) -> Self;
    fn from_base(value: f64) -> Self;
    fn base(self) -> f64;
    fn display_unit(magnitude: Self) -> Unit;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Time(f64);

impl Metric for Time {
    const LOWER: &'static str = "faster";
    const BASE_UNIT: &'static str = "seconds";

    fn read(output: &RunOutput) -> Self {
        Self(output.elapsed().as_secs_f64())
    }

    fn from_base(value: f64) -> Self {
        Self(value)
    }

    fn base(self) -> f64 {
        self.0
    }

    fn display_unit(magnitude: Self) -> Unit {
        match magnitude.0.abs() {
            seconds if seconds >= 1.0 => Unit {
                scale: 1.0,
                symbol: "s",
            },
            seconds if seconds >= 1e-3 => Unit {
                scale: 1e3,
                symbol: "ms",
            },
            seconds if seconds >= 1e-6 => Unit {
                scale: 1e6,
                symbol: "µs",
            },
            _ => Unit {
                scale: 1e9,
                symbol: "ns",
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakMemory(f64);

impl Metric for PeakMemory {
    const LOWER: &'static str = "smaller";
    const BASE_UNIT: &'static str = "bytes";

    fn read(output: &RunOutput) -> Self {
        Self(output.peak_memory().get() as f64)
    }

    fn from_base(value: f64) -> Self {
        Self(value)
    }

    fn base(self) -> f64 {
        self.0
    }

    /// Decimal, matching the names: `KB` is 1000 bytes, not 1024.
    fn display_unit(magnitude: Self) -> Unit {
        match magnitude.0.abs() {
            bytes if bytes >= 1e9 => Unit {
                scale: 1e-9,
                symbol: "GB",
            },
            bytes if bytes >= 1e6 => Unit {
                scale: 1e-6,
                symbol: "MB",
            },
            bytes if bytes >= 1e3 => Unit {
                scale: 1e-3,
                symbol: "KB",
            },
            _ => Unit {
                scale: 1.0,
                symbol: "B",
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each arm, plus the exact boundary, where an off-by-one comparison hides.
    #[test]
    fn time_steps_down_through_si_prefixes() {
        for (seconds, scale, symbol) in [
            (12.0, 1.0, "s"),
            (1.0, 1.0, "s"),
            (0.999, 1e3, "ms"),
            (1e-3, 1e3, "ms"),
            (0.5e-3, 1e6, "µs"),
            (1e-6, 1e6, "µs"),
            (0.5e-6, 1e9, "ns"),
            (0.0, 1e9, "ns"),
        ] {
            let unit = Time::display_unit(Time::from_base(seconds));

            assert_eq!(unit, Unit { scale, symbol }, "{seconds}.");
        }
    }

    #[test]
    fn memory_steps_up_through_decimal_prefixes() {
        for (bytes, scale, symbol) in [
            (3e9, 1e-9, "GB"),
            (1e9, 1e-9, "GB"),
            (5e8, 1e-6, "MB"),
            (1e6, 1e-6, "MB"),
            (2e3, 1e-3, "KB"),
            (1e3, 1e-3, "KB"),
            (999.0, 1.0, "B"),
            (0.0, 1.0, "B"),
        ] {
            let unit = PeakMemory::display_unit(PeakMemory::from_base(bytes));

            assert_eq!(unit, Unit { scale, symbol }, "{bytes}.");
        }
    }

    #[test]
    fn a_fall_scales_like_a_rise() {
        let fall = Time::display_unit(Time::from_base(-2e-3));

        assert_eq!(fall, Time::display_unit(Time::from_base(2e-3)));
    }
}
