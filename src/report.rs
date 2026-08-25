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
        "[{:+.1}{symbol}, {:+.1}{symbol}]",
        range.lower.base() * scale,
        range.upper.base() * scale
    )
}

fn magnitude<M: Metric>(value: M) -> String {
    let Unit { scale, symbol } = M::display_unit(value);

    format!("{:.1}{symbol}", value.base() * scale)
}

fn thousands(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

impl<M: Metric> Summary<M> {
    /// The line under each report's intervals. The draw counts are always shown,
    /// because a fraction of draws is only known to within one draw either way:
    /// an empty side reads as bounded by that resolution rather than exactly zero,
    /// and a full side likewise.
    fn probability(&self) -> String {
        let total = self.draws;
        let below = (self.probability_candidate_lower * total as f64).round() as usize;

        format!(
            "P(candidate {}): {:.1}% ({} of {} draws)",
            M::LOWER,
            100.0 * self.probability_candidate_lower,
            thousands(below),
            thousands(total),
        )
    }
}

impl<M: Metric> Summary<M> {
    /// A one-line summary for a report spanning several benchmarks, e.g. `1.2s -> 554.0ms [-52.41%, -51.31%]`.
    pub(crate) fn compact(&self) -> String {
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
        writeln!(formatter, "{}", self.probability())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::{MeasuredMetric, PeakMemory, Time};
    use crate::posterior::{Draw, Posterior, Shrinkage};
    use crate::repetition::{Pair, Repetition, Repetitions, RunOrder};
    use crate::run::{Bytes, RunOutput};
    use crate::summary::{Interval, Quantiles, Summary};
    use anyhow::Result;
    use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
    use std::num::NonZeroUsize;
    use std::process::ExitStatus;
    use std::time::Duration;

    /// Ten seconds a side, the candidate about 20ms slower, drifting upward, with
    /// enough scatter for the credible intervals to have width.
    fn tiny_change_on_large_totals<M: Metric + MeasuredMetric>(draws: usize) -> Posterior<M> {
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
        [0.5, 0.8, 0.90].map(|width| Interval::new(width).expect("These are valid widths."))
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
            "  50% CrI: [+19.4ms, +20.4ms] (+0.19%, +0.20%)\n",
            "  80% CrI: [+19.0ms, +20.9ms] (+0.19%, +0.21%)\n",
            "  90% CrI: [+18.6ms, +21.1ms] (+0.19%, +0.21%)\n",
            "\n",
            "P(candidate faster): 0.0% (0 of 500 draws)\n",
        );

        let summary = tiny_change_on_large_totals::<Time>(500).summarize(&default_intervals())?;

        assert_eq!(summary.to_string(), EXPECTED);

        Ok(())
    }

    /// The compact line picks the widest requested interval, here 90%, regardless of
    /// the order `--interval` was given in.
    #[test]
    fn compact_matches_golden() -> Result<()> {
        let intervals = [0.90, 0.5, 0.8].map(|width| Interval::new(width).expect("Valid."));
        let summary = tiny_change_on_large_totals::<Time>(500).summarize(&intervals)?;

        assert_eq!(summary.compact(), "10.0s -> 10.1s [+0.19%, +0.21%]");

        Ok(())
    }

    #[test]
    fn a_zero_baseline_reports_without_percentages() -> Result<()> {
        const EXPECTED: &str = concat!(
            "Baseline:  0.0B\n",
            "Candidate: 0.0B\n",
            "\n",
            "Change: +0.0B\n",
            "  50% CrI: [+0.0B, +0.0B]\n",
            "\n",
            "P(candidate smaller): 0.0% (0 of 500 draws)\n",
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

        assert_eq!(summary.compact(), "0.0B -> 0.0B [+0.0B, +0.0B]");

        Ok(())
    }

    fn fixed_draws() -> Vec<Draw<Time>> {
        [
            (10.0, 10.03),
            (10.0, 10.02),
            (10.0, 10.05),
            (10.0, 9.99),
            (10.0, 10.07),
            (10.0, 10.06),
            (10.0, 10.09),
            (10.0, 10.08),
            (10.0, 10.11),
            (10.0, 10.10),
        ]
        .into_iter()
        .map(|(baseline, candidate)| Draw {
            baseline: Time::from_base(baseline),
            candidate: Time::from_base(candidate),
        })
        .collect()
    }

    fn fixed_summary(intervals: &[Interval]) -> Result<Summary<Time>> {
        Summary::from_draws(&fixed_draws(), intervals)
    }

    #[test]
    fn fixed_posterior_report_matches_expected() -> Result<()> {
        let summary = fixed_summary(&default_intervals())?;

        assert_eq!(summary.baseline, Time::from_base(10.0));
        assert!((summary.candidate.base() - 10.065).abs() < 1e-9);
        assert!((summary.change.absolute_median.base() - 0.065).abs() < 1e-9);
        assert!((summary.change.relative_median.unwrap() - 0.65).abs() < 1e-9);
        assert!((summary.probability_candidate_lower - 0.1).abs() < 1e-12);

        let cri_50 = &summary.change.intervals[0];
        assert_eq!(cri_50.interval.percent(), 50.0);
        assert!((cri_50.absolute.lower.base() - 0.035).abs() < 1e-9);
        assert!((cri_50.absolute.upper.base() - 0.0875).abs() < 1e-9);
        assert!((cri_50.relative.unwrap().lower - 0.35).abs() < 1e-9);
        assert!((cri_50.relative.unwrap().upper - 0.875).abs() < 1e-9);

        let cri_80 = &summary.change.intervals[1];
        assert_eq!(cri_80.interval.percent(), 80.0);
        assert!((cri_80.absolute.lower.base() - 0.017).abs() < 1e-9);
        assert!((cri_80.absolute.upper.base() - 0.101).abs() < 1e-9);
        assert!((cri_80.relative.unwrap().lower - 0.17).abs() < 1e-9);
        assert!((cri_80.relative.unwrap().upper - 1.01).abs() < 1e-9);

        let cri_90 = &summary.change.intervals[2];
        assert_eq!(cri_90.interval.percent(), 90.0);
        assert!((cri_90.absolute.lower.base() - 0.0035).abs() < 1e-9);
        assert!((cri_90.absolute.upper.base() - 0.1055).abs() < 1e-9);
        assert!((cri_90.relative.unwrap().lower - 0.035).abs() < 1e-9);
        assert!((cri_90.relative.unwrap().upper - 1.055).abs() < 1e-9);

        const EXPECTED: &str = concat!(
            "Baseline:  10.0s\n",
            "Candidate: 10.1s\n",
            "\n",
            "Change: +65.0ms (+0.65%)\n",
            "  50% CrI: [+35.0ms, +87.5ms] (+0.35%, +0.87%)\n",
            "  80% CrI: [+17.0ms, +101.0ms] (+0.17%, +1.01%)\n",
            "  90% CrI: [+3.5ms, +105.5ms] (+0.04%, +1.05%)\n",
            "\n",
            "P(candidate faster): 10.0% (1 of 10 draws)\n",
        );

        assert_eq!(summary.to_string(), EXPECTED);

        Ok(())
    }

    #[test]
    fn fixed_posterior_compact_matches_expected() -> Result<()> {
        let summary = fixed_summary(&default_intervals())?;

        assert_eq!(summary.compact(), "10.0s -> 10.1s [+0.04%, +1.05%]");

        Ok(())
    }

    /// Ten thousand draws all on one side: the count and percentage are reported
    /// as-is, not bounded by the draw resolution.
    #[test]
    fn a_one_sided_probability_reports_the_count_and_percentage() -> Result<()> {
        let draws: Vec<Draw<Time>> = (0..10_000)
            .map(|_| Draw {
                baseline: Time::from_base(10.0),
                candidate: Time::from_base(11.0),
            })
            .collect();
        let slower = Summary::from_draws(&draws, &default_intervals())?;
        assert_eq!(
            slower.probability(),
            "P(candidate faster): 0.0% (0 of 10,000 draws)"
        );

        let mirrored: Vec<Draw<Time>> = draws
            .iter()
            .map(|_| Draw {
                baseline: Time::from_base(11.0),
                candidate: Time::from_base(10.0),
            })
            .collect();
        let faster = Summary::from_draws(&mirrored, &default_intervals())?;
        assert_eq!(
            faster.probability(),
            "P(candidate faster): 100.0% (10,000 of 10,000 draws)"
        );

        Ok(())
    }

    #[test]
    fn quantile_at_zero_returns_the_minimum() -> Result<()> {
        let values = vec![3.0, 1.0, 2.0];
        let q = Quantiles::new(values)?;

        assert_eq!(q.at(0.0), 1.0);

        Ok(())
    }

    #[test]
    fn quantile_at_one_returns_the_maximum() -> Result<()> {
        let values = vec![3.0, 1.0, 2.0];
        let q = Quantiles::new(values)?;

        assert_eq!(q.at(1.0), 3.0);

        Ok(())
    }

    #[test]
    fn quantile_rejects_empty_input() -> Result<()> {
        assert!(Quantiles::new(Vec::new()).is_err());

        Ok(())
    }
}
