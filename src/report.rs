use crate::metric::{Metric, Unit};
use crate::summary::{Range, Summary};

use std::fmt;

fn shared<M: Metric>(a: M, b: M) -> Unit {
    M::display_unit(if a.base().abs() >= b.base().abs() {
        a
    } else {
        b
    })
}

fn signed<M: Metric>(value: M) -> String {
    let Unit { scale, symbol } = M::display_unit(value);

    format!("{:+.1}{symbol}", value.base() * scale)
}

fn bracket<M: Metric>(range: Range<M>) -> String {
    let Unit { scale, symbol } = shared(range.lower, range.upper);

    format!(
        "[{:+.1}, {:+.1}]{symbol}",
        range.lower.base() * scale,
        range.upper.base() * scale
    )
}

fn magnitude<M: Metric>(value: M) -> String {
    let Unit { scale, symbol } = M::display_unit(value);

    format!("{:.1}{symbol}", value.base() * scale)
}

impl<M: Metric> Summary<M> {
    /// A one-line summary for a report spanning several benchmarks, e.g. `1.2s -> 554.0ms [-52.41%, -51.31%]`.
    pub fn compact(&self) -> String {
        let bounds = self
            .change
            .intervals
            .iter()
            .max_by(|a, b| a.interval.percent().total_cmp(&b.interval.percent()))
            .expect("At least one interval is always requested.");

        let change = match bounds.relative {
            Some(relative) => format!("[{:+.2}%, {:+.2}%]", relative.lower, relative.upper),
            None => bracket(bounds.absolute),
        };

        format!(
            "{} -> {} {change}",
            magnitude(self.baseline),
            magnitude(self.candidate)
        )
    }
}

impl<M: Metric> fmt::Display for Summary<M> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Unit { scale, symbol } = shared(self.baseline, self.candidate);

        writeln!(
            formatter,
            "Baseline:  {:.1}{symbol}",
            self.baseline.base() * scale
        )?;
        writeln!(
            formatter,
            "Candidate: {:.1}{symbol}",
            self.candidate.base() * scale
        )?;
        writeln!(formatter)?;

        write!(formatter, "Change: {}", signed(self.change.absolute_median))?;

        if let Some(median) = self.change.relative_median {
            write!(formatter, " ({median:+.2}%)")?;
        }

        writeln!(formatter)?;

        for bounds in &self.change.intervals {
            write!(
                formatter,
                "  {:.0}% CrI: {}",
                bounds.interval.percent(),
                bracket(bounds.absolute),
            )?;

            if let Some(relative) = bounds.relative {
                write!(
                    formatter,
                    " ({:+.2}%, {:+.2}%)",
                    relative.lower, relative.upper
                )?;
            }

            writeln!(formatter)?;
        }

        writeln!(formatter)?;
        writeln!(
            formatter,
            "P(candidate {}): {:.1}%",
            M::LOWER,
            100.0 * self.probability_candidate_lower
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::{PeakMemory, Time};
    use crate::posterior::{Posterior, Shrinkage};
    use crate::repetition::{Pair, Repetition, Repetitions, RunOrder};
    use crate::run::{Bytes, RunOutput};
    use crate::summary::Interval;
    use anyhow::Result;
    use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
    use std::num::NonZeroUsize;
    use std::process::ExitStatus;
    use std::time::Duration;

    /// Ten seconds a side, the candidate about 20ms slower, drifting upward, with
    /// enough scatter for the credible intervals to have width.
    fn tiny_change_on_large_totals<M: Metric>(draws: usize) -> Posterior<M> {
        let baseline = [
            10.000, 10.012, 10.019, 10.031, 10.038, 10.052, 10.058, 10.071, 10.079, 10.090,
        ];
        let candidate = [
            10.023, 10.028, 10.036, 10.054, 10.057, 10.076, 10.074, 10.092, 10.097, 10.112,
        ];

        let second = |seconds: f64| {
            RunOutput::new(
                ExitStatus::default(),
                Duration::from_secs_f64(seconds),
                Some(Bytes::ZERO),
            )
        };

        let repetitions = (0..baseline.len())
            .map(|position| Repetition {
                outputs: Pair {
                    baseline: second(baseline[position]),
                    candidate: second(candidate[position]),
                },
                order: if position % 2 == 0 {
                    RunOrder::BaselineFirst
                } else {
                    RunOrder::CandidateFirst
                },
            })
            .collect::<Vec<_>>();

        let repetitions =
            Repetitions::try_from(repetitions).expect("Ten repetitions, both orders.");
        let draws = NonZeroUsize::new(draws).expect("Test draw counts are positive.");

        Posterior::bootstrap(
            &repetitions,
            draws,
            Shrinkage::NONE,
            &mut Xoshiro256PlusPlus::seed_from_u64(0),
        )
        .expect("The design is well conditioned.")
    }

    /// The CLI's default widths, so the golden below is what a user sees.
    fn default_intervals() -> [Interval; 3] {
        [0.5, 0.8, 0.98].map(|width| Interval::new(width).expect("These are valid widths."))
    }

    /// Pins everything the report prints: the units, the sign and precision of every
    /// column, the blank lines, and one CrI line per requested width. The levels read
    /// in seconds while the change reads in milliseconds.
    ///
    /// As with the posterior goldens, these numbers came from the code they check, so
    /// they catch drift rather than error.
    #[test]
    fn report_matches_golden() -> Result<()> {
        const EXPECTED: &str = concat!(
            "Baseline:  10.0s\n",
            "Candidate: 10.1s\n",
            "\n",
            "Change: +20.0ms (+0.20%)\n",
            "  50% CrI: [+19.4, +20.4]ms (+0.19%, +0.20%)\n",
            "  80% CrI: [+19.0, +20.9]ms (+0.19%, +0.21%)\n",
            "  98% CrI: [+18.1, +21.4]ms (+0.18%, +0.21%)\n",
            "\n",
            "P(candidate faster): 0.0%\n",
        );

        let summary = tiny_change_on_large_totals::<Time>(500).summarize(&default_intervals())?;

        assert_eq!(summary.to_string(), EXPECTED);

        Ok(())
    }

    /// The compact line picks the widest requested interval, here 98%, regardless of
    /// the order `--interval` was given in.
    #[test]
    fn compact_matches_golden() -> Result<()> {
        let intervals = [0.98, 0.5, 0.8].map(|width| Interval::new(width).expect("Valid."));
        let summary = tiny_change_on_large_totals::<Time>(500).summarize(&intervals)?;

        assert_eq!(summary.compact(), "10.0s -> 10.1s [+0.18%, +0.21%]");

        Ok(())
    }

    #[test]
    fn a_zero_baseline_reports_without_percentages() -> Result<()> {
        const EXPECTED: &str = concat!(
            "Baseline:  0.0B\n",
            "Candidate: 0.0B\n",
            "\n",
            "Change: +0.0B\n",
            "  50% CrI: [+0.0, +0.0]B\n",
            "\n",
            "P(candidate smaller): 0.0%\n",
        );

        let half = [Interval::new(0.5)?];
        let summary = tiny_change_on_large_totals::<PeakMemory>(500).summarize(&half)?;

        assert!(summary.change.relative_median.is_none());
        assert_eq!(summary.to_string(), EXPECTED);

        Ok(())
    }

    #[test]
    fn compact_falls_back_to_absolute_bounds_with_a_zero_baseline() -> Result<()> {
        let half = [Interval::new(0.5)?];
        let summary = tiny_change_on_large_totals::<PeakMemory>(500).summarize(&half)?;

        assert_eq!(summary.compact(), "0.0B -> 0.0B [+0.0, +0.0]B");

        Ok(())
    }
}
