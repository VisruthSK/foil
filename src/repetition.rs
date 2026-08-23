use crate::run::RunOutput;

use anyhow::{Result, ensure};
use rand::{Rng, RngExt, seq::SliceRandom};
use std::{fmt, num::NonZeroUsize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
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
pub(crate) struct Pair<T> {
    pub(crate) baseline: T,
    pub(crate) candidate: T,
}

impl<T> Pair<T> {
    pub(crate) const fn get(&self, side: Side) -> &T {
        match side {
            Side::Baseline => &self.baseline,
            Side::Candidate => &self.candidate,
        }
    }

    pub(crate) fn from_execution_order([first, second]: [T; 2], order: RunOrder) -> Self {
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
pub(crate) enum RunOrder {
    CandidateFirst,
    BaselineFirst,
}

impl RunOrder {
    /// Effect coding of the order contrast used by the regressions.
    pub(crate) const fn effect_code(self) -> f64 {
        match self {
            Self::CandidateFirst => -1.0,
            Self::BaselineFirst => 1.0,
        }
    }

    pub(crate) const fn sides(self) -> [Side; 2] {
        match self {
            Self::BaselineFirst => [Side::Baseline, Side::Candidate],
            Self::CandidateFirst => [Side::Candidate, Side::Baseline],
        }
    }

    pub(crate) fn schedule(
        repetitions: usize,
        block_size: NonZeroUsize,
        rng: &mut impl Rng,
    ) -> Vec<Self> {
        let block_size = block_size.get();
        let full_blocks = repetitions / block_size;
        let remainder = repetitions % block_size;
        let odd_blocks = full_blocks * (block_size % 2) + remainder % 2;
        let mut surpluses = [Self::BaselineFirst, Self::CandidateFirst].repeat(odd_blocks / 2);
        if odd_blocks % 2 == 1 {
            surpluses.push(if rng.random() {
                Self::BaselineFirst
            } else {
                Self::CandidateFirst
            });
        }
        surpluses.shuffle(rng);
        let mut surpluses = surpluses.into_iter();
        let mut schedule = Vec::with_capacity(repetitions);

        while schedule.len() < repetitions {
            let size = block_size.min(repetitions - schedule.len());
            let mut block = [Self::BaselineFirst, Self::CandidateFirst].repeat(size / 2);
            if size % 2 == 1 {
                block.push(surpluses.next().expect("Every odd block has a surplus."));
            }
            block.shuffle(rng);
            schedule.extend(block);
        }

        schedule
    }
}

/// Baseline and candidate measured back to back, in a known order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Repetition {
    pub(crate) outputs: Pair<RunOutput>,
    pub(crate) order: RunOrder,
}

/// A validated set of paired repetitions.
pub(crate) struct Repetitions(Vec<Repetition>);

impl Repetitions {
    pub(crate) const MINIMUM: usize = 10;

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

    fn schedule(repetitions: usize, block_size: usize, seed: u64) -> Vec<RunOrder> {
        RunOrder::schedule(
            repetitions,
            NonZeroUsize::new(block_size).unwrap(),
            &mut Xoshiro256PlusPlus::seed_from_u64(seed),
        )
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
            assert_eq!(Pair::from_execution_order(order.sides(), order), labelled);
        }
    }

    #[test]
    fn schedules_are_balanced() {
        for repetitions in [Repetitions::MINIMUM, Repetitions::MINIMUM + 1] {
            let orders = schedule(repetitions, 4, 0);
            let first = baseline_firsts(&orders);
            assert_eq!(orders.len(), repetitions);
            assert!(first.abs_diff(repetitions - first) <= 1);
        }
    }

    #[test]
    fn full_blocks_are_balanced() {
        for block in schedule(14, 4, 0)[..12].chunks_exact(4) {
            assert_eq!(baseline_firsts(block), 2);
        }
    }

    #[test]
    fn odd_blocks_balance_their_surplus_before_shuffling() {
        for block_size in [1, 3, 5] {
            for seed in 0..64 {
                let orders = schedule(60, block_size, seed);
                assert_eq!(baseline_firsts(&orders), orders.len() / 2);
                assert!(orders.chunks(block_size).all(|block| {
                    baseline_firsts(block).abs_diff(block.len() - baseline_firsts(block)) <= 1
                }));
            }
        }
    }

    #[test]
    fn the_odd_repetition_falls_on_either_side() {
        let counts: Vec<_> = (0..16)
            .map(|seed| baseline_firsts(&schedule(Repetitions::MINIMUM + 1, 4, seed)))
            .collect();
        assert!(counts.contains(&5) && counts.contains(&6));
    }
}
