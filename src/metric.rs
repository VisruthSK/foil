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
