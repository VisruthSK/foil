use anyhow::Result;
use foil::{Change, ChangeBounds, Interval, Metric, PeakMemory, Range, Summary, Time};

fn defaults() -> [Interval; 3] {
    [0.5, 0.8, 0.9].map(|width| Interval::new(width).unwrap())
}

#[test]
fn time_report_matches_the_public_summary_contract() -> Result<()> {
    let [half, eighty, ninety] = defaults();
    let summary = Summary {
        baseline: Time::from_base(10.0),
        candidate: Time::from_base(10.065),
        change: Change {
            absolute_median: Time::from_base(0.065),
            relative_median: Some(0.65),
            intervals: vec![
                ChangeBounds {
                    interval: half,
                    absolute: Range {
                        lower: Time::from_base(0.035),
                        upper: Time::from_base(0.0875),
                    },
                    relative: Some(Range {
                        lower: 0.35,
                        upper: 0.875,
                    }),
                },
                ChangeBounds {
                    interval: eighty,
                    absolute: Range {
                        lower: Time::from_base(0.017),
                        upper: Time::from_base(0.101),
                    },
                    relative: Some(Range {
                        lower: 0.17,
                        upper: 1.01,
                    }),
                },
                ChangeBounds {
                    interval: ninety,
                    absolute: Range {
                        lower: Time::from_base(0.0035),
                        upper: Time::from_base(0.1055),
                    },
                    relative: Some(Range {
                        lower: 0.035,
                        upper: 1.055,
                    }),
                },
            ],
        },
        probability_candidate_lower: 0.1,
        draws: 10,
    };

    const EXPECTED: &str = concat!(
        "Baseline:  10.0s\n",
        "Candidate: 10.1s\n",
        "\n",
        "Change: +65.0ms (+0.65%)\n",
        "  50% CrI: [+35.0ms, +87.5ms] (+0.35%, +0.88%)\n",
        "  80% CrI: [+17.0ms, +101.0ms] (+0.17%, +1.01%)\n",
        "  90% CrI: [+3.5ms, +105.5ms] (+0.04%, +1.05%)\n",
        "\n",
        "P(candidate faster): 10.0% (1 of 10 draws)\n",
    );
    assert_eq!(summary.to_string(), EXPECTED);
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
