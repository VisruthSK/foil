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
        let mut schedule = Vec::with_capacity(repetitions);
        let mut baseline_first = 0;

        while schedule.len() < repetitions {
            let size = block_size.get().min(repetitions - schedule.len());
            let mut baseline = size / 2;
            if size % 2 == 1 && (baseline_first * 2 < schedule.len() || rng.random::<bool>()) {
                baseline += 1;
            }
            let mut block = vec![Self::BaselineFirst; baseline];
            block.resize(size, Self::CandidateFirst);
            block.shuffle(rng);
            baseline_first += baseline;
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
