use crate::run::RunOutput;

use anyhow::{Result, ensure};
use rand::{Rng, RngExt, seq::SliceRandom};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Baseline,
    Candidate,
}

impl fmt::Display for Side {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pair<T> {
    pub baseline: T,
    pub candidate: T,
}

impl<T> Pair<T> {
    pub const fn get(&self, side: Side) -> &T {
        match side {
            Side::Baseline => &self.baseline,
            Side::Candidate => &self.candidate,
        }
    }

    pub fn from_execution_order([first, second]: [T; 2], order: RunOrder) -> Self {
        match order {
            RunOrder::BaselineFirst => Self {
                baseline: first,
                candidate: second,
            },
            RunOrder::CandidateFirst => Self {
                baseline: second,
                candidate: first,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOrder {
    CandidateFirst,
    BaselineFirst,
}

impl RunOrder {
    /// Effect coding of the order contrast used by the regressions.
    pub const fn effect_code(self) -> f64 {
        match self {
            Self::CandidateFirst => -1.0,
            Self::BaselineFirst => 1.0,
        }
    }

    pub const fn sides(self) -> [Side; 2] {
        match self {
            Self::BaselineFirst => [Side::Baseline, Side::Candidate],
            Self::CandidateFirst => [Side::Candidate, Side::Baseline],
        }
    }

    // TODO: Swap to Latin square?
    pub fn schedule(repetitions: usize, rng: &mut impl Rng) -> Vec<Self> {
        let mut orders = [Self::BaselineFirst, Self::CandidateFirst].repeat(repetitions / 2);

        if repetitions % 2 == 1 {
            orders.push(if rng.random() {
                Self::BaselineFirst
            } else {
                Self::CandidateFirst
            });
        }

        orders.shuffle(rng);

        orders
    }
}

/// Baseline and candidate measured back to back, in a known order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Repetition {
    pub outputs: Pair<RunOutput>,
    pub order: RunOrder,
}

/// A validated set of paired repetitions.
pub struct Repetitions(Vec<Repetition>);

impl Repetitions {
    pub const MINIMUM: usize = 10;

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Repetition> {
        self.0.iter()
    }

    /// Mean run position, the point the model's drift term is centered on.
    pub(crate) fn center(&self) -> f64 {
        (self.0.len() - 1) as f64 / 2.0
    }
}

impl TryFrom<Vec<Repetition>> for Repetitions {
    type Error = anyhow::Error;

    fn try_from(repetitions: Vec<Repetition>) -> Result<Self> {
        ensure!(
            repetitions.len() >= Self::MINIMUM,
            "At least {} repetitions are required, got {}.",
            Self::MINIMUM,
            repetitions.len()
        );

        let seen = |wanted| repetitions.iter().any(|it| it.order == wanted);
        ensure!(
            seen(RunOrder::BaselineFirst) && seen(RunOrder::CandidateFirst),
            "Both run orders are required."
        );

        Ok(Self(repetitions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};

    fn schedule(repetitions: usize, seed: u64) -> Vec<RunOrder> {
        RunOrder::schedule(repetitions, &mut Xoshiro256PlusPlus::seed_from_u64(seed))
    }

    fn baseline_firsts(orders: &[RunOrder]) -> usize {
        orders
            .iter()
            .filter(|&&order| order == RunOrder::BaselineFirst)
            .count()
    }

    #[test]
    fn sides_and_from_execution_order_agree() {
        let labelled = Pair {
            baseline: Side::Baseline,
            candidate: Side::Candidate,
        };

        for order in [RunOrder::BaselineFirst, RunOrder::CandidateFirst] {
            assert_eq!(
                Pair::from_execution_order(order.sides(), order),
                labelled,
                "{order:?}."
            );
        }
    }

    /// Balance implies the two orders [`Repetitions`] requires. An odd count is where
    /// one of them could go missing.
    #[test]
    fn schedules_are_balanced() {
        for repetitions in [Repetitions::MINIMUM, Repetitions::MINIMUM + 1] {
            let orders = schedule(repetitions, 0);
            let first = baseline_firsts(&orders);

            assert_eq!(orders.len(), repetitions);
            assert!(
                first.abs_diff(repetitions - first) <= 1,
                "{repetitions} repetitions split {first}/{}.",
                repetitions - first
            );
        }
    }

    /// A fixed side would still pass the balance check above while tilting every
    /// odd-length run the same way.
    #[test]
    fn the_odd_repetition_falls_on_either_side() {
        let counts: Vec<usize> = (0..16)
            .map(|seed| baseline_firsts(&schedule(Repetitions::MINIMUM + 1, seed)))
            .collect();

        assert!(counts.contains(&5) && counts.contains(&6), "{counts:?}");
    }
}
