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
