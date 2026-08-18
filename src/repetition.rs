use crate::run::RunOutput;

use anyhow::{Result, ensure};
use rand::{Rng, RngExt, seq::SliceRandom};
use std::{fmt, num::NonZeroUsize};

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

    pub fn schedule(repetitions: usize, block_size: NonZeroUsize, rng: &mut impl Rng) -> Vec<Self> {
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
