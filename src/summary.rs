use crate::metric::Metric;
use crate::posterior::Draw;

use anyhow::{Context, Result, ensure};
use std::str::FromStr;

/// Width of a central credible interval, strictly between 0 and 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interval(f64);

impl Interval {
    pub fn new(width: f64) -> Result<Self> {
        ensure!(
            0.0 < width && width < 1.0,
            "Interval width must be between 0 and 1, got {width}."
        );

        Ok(Self(width))
    }

    pub fn tails(self) -> (f64, f64) {
        let tail = (1.0 - self.0) / 2.0;
        (tail, 1.0 - tail)
    }

    /// Rejects widths whose tails are narrower than one repetition's share of the
    /// posterior, i.e. anything above `1 - 2/pairs`. Ten pairs cap the width at 80%.
    ///
    /// A bootstrap quantile beyond the first order statistic's weight describes the
    /// resampling noise rather than the data, so such an interval is refused up front
    /// instead of printed after the benchmarks have run.
    pub fn validate_for_pairs(self, pairs: usize) -> Result<Self> {
        let widest = 100.0 * (pairs - 2) as f64 / pairs as f64;

        ensure!(
            self.percent() <= widest + 1e-6,
            "A {:.0}% interval needs each tail to span at least one of the {} paired \
             repetitions; the widest supported interval at {} repetitions is {:.0}%.",
            self.percent(),
            pairs,
            pairs,
            widest
        );

        Ok(self)
    }

    pub fn percent(self) -> f64 {
        100.0 * self.0
    }
}

impl FromStr for Interval {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        Self::new(
            text.parse()
                .with_context(|| format!("`{text}` is not a number."))?,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Range<T> {
    pub lower: T,
    pub upper: T,
}

impl<T> Range<T> {
    pub fn map<U>(self, convert: impl Fn(T) -> U) -> Range<U> {
        Range {
            lower: convert(self.lower),
            upper: convert(self.upper),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChangeBounds<M> {
    pub interval: Interval,
    pub absolute: Range<M>,
    pub relative: Option<Range<f64>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Change<M> {
    pub absolute_median: M,
    pub relative_median: Option<f64>,
    pub intervals: Vec<ChangeBounds<M>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Summary<M> {
    pub baseline: M,
    pub candidate: M,
    pub change: Change<M>,
    pub probability_candidate_lower: f64,
    pub draws: usize,
}

impl<M: Metric> Summary<M> {
    pub(crate) fn from_draws(draws: &[Draw<M>], intervals: &[Interval]) -> Result<Self> {
        let baseline = Quantiles::new(draws.iter().map(|draw| draw.baseline.base()).collect())?;
        let candidate = Quantiles::new(draws.iter().map(|draw| draw.candidate.base()).collect())?;
        let absolute = Quantiles::new(draws.iter().map(|draw| draw.absolute().base()).collect())?;

        let relative = draws
            .iter()
            .map(|draw| draw.relative())
            .collect::<Option<Vec<f64>>>()
            .map(Quantiles::new)
            .transpose()?;

        Ok(Self {
            baseline: M::from_base(baseline.median()),
            candidate: M::from_base(candidate.median()),
            probability_candidate_lower: absolute.fraction_below(0.0),
            draws: draws.len(),
            change: Change {
                absolute_median: M::from_base(absolute.median()),
                relative_median: relative.as_ref().map(Quantiles::median),
                intervals: intervals
                    .iter()
                    .map(|&interval| ChangeBounds {
                        interval,
                        absolute: absolute.range(interval).map(M::from_base),
                        relative: relative.as_ref().map(|it| it.range(interval)),
                    })
                    .collect(),
            },
        })
    }
}

/// One posterior series, sorted so a quantile is a lookup.
///
/// Built only from a [`Posterior`](crate::posterior::Posterior), which is never empty,
/// so `len() - 1` cannot underflow and every index is in range. NaN is rejected rather
/// than sorted, because it would sit at one end under `total_cmp` while failing every
/// comparison [`Self::fraction_below`] partitions on.
pub(crate) struct Quantiles(Vec<f64>);

impl Quantiles {
    pub(crate) fn new(mut values: Vec<f64>) -> Result<Self> {
        ensure!(!values.is_empty(), "Posterior is empty.");
        ensure!(
            !values.iter().any(|value| value.is_nan()),
            "Posterior contains NaN."
        );

        values.sort_by(f64::total_cmp);

        Ok(Self(values))
    }

    pub(crate) fn at(&self, probability: f64) -> f64 {
        debug_assert!((0.0..=1.0).contains(&probability));
        let values = &self.0;
        let h = (values.len() - 1) as f64 * probability;
        let lo = h.floor() as usize;
        let hi = h.ceil() as usize;
        if lo == hi {
            return values[lo];
        }
        let t = h - lo as f64;
        values[lo] + t * (values[hi] - values[lo])
    }

    fn median(&self) -> f64 {
        self.at(0.5)
    }

    fn range(&self, interval: Interval) -> Range<f64> {
        let (lower, upper) = interval.tails();

        Range {
            lower: self.at(lower),
            upper: self.at(upper),
        }
    }

    fn fraction_below(&self, threshold: f64) -> f64 {
        self.0.partition_point(|&value| value < threshold) as f64 / self.0.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interval_at_the_repetition_boundary_is_accepted() -> Result<()> {
        // Ten pairs put one repetition in each 10% tail, so exactly 80% is the widest.
        Interval::new(0.8)?.validate_for_pairs(10)?;

        Ok(())
    }

    #[test]
    fn an_interval_wider_than_the_repetitions_support_is_rejected() -> Result<()> {
        let error = Interval::new(0.98)?
            .validate_for_pairs(10)
            .expect_err("98% exceeds what ten pairs support");

        assert!(error.to_string().contains("80%"), "{error}");

        Ok(())
    }
}
