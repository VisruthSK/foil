pub mod common;

use anyhow::Result;
use common::MEASUREMENTS;
use foil::{
    Change, ChangeBounds, Interval, Metric, PeakMemory, Range, Shrinkage, Summary, Time,
    analyze_measurements,
};
use std::{fs, num::NonZeroUsize};
use tempfile::tempdir;

#[test]
fn interpolated_posterior_quantiles_are_rendered() -> Result<()> {
    let directory = tempdir()?;
    let path = directory.path().join("measurements.csv");
    fs::write(&path, MEASUREMENTS)?;
    let report = analyze_measurements(
        &path,
        0,
        NonZeroUsize::new(8).unwrap(),
        Shrinkage::NONE,
        &[Interval::new(0.8)?],
    )?
    .summary
    .to_string();

    const EXPECTED: &str = concat!(
        "Baseline:  1.3s\n",
        "Candidate: 1.3s\n",
        "\n",
        "Change: +21.3ms (+1.65%)\n",
        "  80% CrI: [+4.1ms, +29.5ms] (+0.32%, +2.28%)\n",
        "\n",
        "P(candidate faster): 12.5% (1 of 8 draws)\n",
    );
    assert_eq!(report, EXPECTED);
    Ok(())
}

#[test]
fn zero_memory_baseline_omits_relative_changes() -> Result<()> {
    let zero = PeakMemory::from_base(0.0);
    let summary = Summary {
        baseline: zero,
        candidate: zero,
        change: Change {
            absolute_median: zero,
            relative_median: None,
            intervals: vec![ChangeBounds {
                interval: Interval::new(0.5)?,
                absolute: Range {
                    lower: zero,
                    upper: zero,
                },
                relative: None,
            }],
        },
        probability_candidate_lower: 0.0,
        draws: 500,
    };

    const EXPECTED: &str = concat!(
        "Baseline:  0.0B\n",
        "Candidate: 0.0B\n",
        "\n",
        "Change: +0.0B\n",
        "  50% CrI: [+0.0B, +0.0B]\n",
        "\n",
        "P(candidate smaller): 0.0% (0 of 500 draws)\n",
    );
    assert_eq!(summary.to_string(), EXPECTED);
    Ok(())
}

#[test]
fn probability_counts_use_thousands_separators() -> Result<()> {
    let value = Time::from_base(1.0);
    let report = |probability| Summary {
        baseline: value,
        candidate: value,
        change: Change {
            absolute_median: Time::from_base(0.0),
            relative_median: Some(0.0),
            intervals: Vec::new(),
        },
        probability_candidate_lower: probability,
        draws: 10_000,
    };

    assert!(
        report(0.0)
            .to_string()
            .contains("P(candidate faster): 0.0% (0 of 10,000 draws)")
    );
    assert!(
        report(1.0)
            .to_string()
            .contains("P(candidate faster): 100.0% (10,000 of 10,000 draws)")
    );
    Ok(())
}
