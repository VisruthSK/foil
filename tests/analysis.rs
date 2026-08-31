use anyhow::{Context, Result};
use foil::{Interval, Metric, Shrinkage, analyze_measurements};
use std::{fs, num::NonZeroUsize, path::Path};
use tempfile::tempdir;

const MEASUREMENTS: &str = concat!(
    "repetition,order,baseline_seconds,candidate_seconds\n",
    "1,candidate_first,1,1.04\n",
    "2,baseline_first,1.08,1.06\n",
    "3,baseline_first,1.13,1.19\n",
    "4,candidate_first,1.18,1.17\n",
    "5,candidate_first,1.27,1.31\n",
    "6,baseline_first,1.31,1.30\n",
    "7,candidate_first,1.39,1.46\n",
    "8,baseline_first,1.44,1.41\n",
    "9,baseline_first,1.53,1.58\n",
    "10,candidate_first,1.59,1.61\n",
);

fn fixture(contents: &str) -> Result<(tempfile::TempDir, std::path::PathBuf)> {
    let directory = tempdir()?;
    let path = directory.path().join("measurements.csv");
    fs::write(&path, contents)?;
    Ok((directory, path))
}

fn analyze(path: &Path, shrinkage: f64, draws: usize) -> Result<foil::Analysis> {
    analyze_measurements(
        path,
        0,
        NonZeroUsize::new(draws).unwrap(),
        Shrinkage::new(shrinkage)?,
        &[
            Interval::new(0.5)?,
            Interval::new(0.8)?,
            Interval::new(0.9)?,
        ],
    )
}

#[test]
fn fixed_measurements_produce_deterministic_unshrunk_and_shrunk_posteriors() -> Result<()> {
    let (_directory, path) = fixture(MEASUREMENTS)?;

    let unshrunk = analyze(&path, 0.0, 8)?;
    let unshrunk_again = analyze(&path, 0.0, 8)?;
    let shrunk = analyze(&path, 5.0, 8)?;

    #[rustfmt::skip]
    const UNSHRUNK: [(f64, f64); 8] = [
        (1.2897790127661646, 1.3140883568234745),
        (1.292883484982506,  1.3266471431710283),
        (1.2879932756550871, 1.2945836174112333),
        (1.2823193280893046, 1.280692753461745),
        (1.2916998674200741, 1.3035345997982273),
        (1.2928833190008737, 1.3205179916667944),
        (1.2911119206996142, 1.311784563265395),
        (1.2897803634958591, 1.311705417840889),
    ];
    #[rustfmt::skip]
    const SHRUNK: [(f64, f64); 8] = [
        (1.2923191661142508, 1.3115482034753883),
        (1.3004709405338217, 1.323672830397041),
        (1.291195784730286,  1.3034427996514897),
        (1.299741227321653,  1.3092432191986378),
        (1.2981568918715902, 1.3104455855880441),
        (1.2945052725350779, 1.3083912114299314),
        (1.296645936301807,  1.309693976413047),
        (1.293723742091815,  1.303450869465768),
    ];
    let values = |analysis: &foil::Analysis| {
        analysis
            .posterior
            .draws()
            .iter()
            .map(|draw| (draw.baseline.base(), draw.candidate.base()))
            .collect::<Vec<_>>()
    };

    assert_eq!(unshrunk.posterior.draws(), unshrunk_again.posterior.draws());
    assert_eq!(values(&unshrunk), UNSHRUNK);
    assert_eq!(values(&shrunk), SHRUNK);
    assert_eq!(unshrunk.summary.change.intervals.len(), 3);
    assert_eq!(shrunk.summary.change.intervals.len(), 3);
    Ok(())
}

#[test]
fn more_shrinkage_reduces_the_typical_adjusted_difference() -> Result<()> {
    let (_directory, path) = fixture(MEASUREMENTS)?;

    let median_difference = |shrinkage| -> Result<f64> {
        let mut differences: Vec<_> = analyze(&path, shrinkage, 4_000)?
            .posterior
            .draws()
            .iter()
            .map(|draw| draw.absolute().base().abs())
            .collect();
        differences.sort_by(f64::total_cmp);
        Ok(differences[differences.len() / 2])
    };

    let none = median_difference(0.0)?;
    let some = median_difference(5.0)?;
    let lots = median_difference(50.0)?;
    assert!(lots < some && some < none, "{lots} < {some} < {none}");
    Ok(())
}

#[test]
fn an_unrepresentable_duration_is_rejected() -> Result<()> {
    let invalid = MEASUREMENTS.replacen(",1,1.04", ",1e300,1.04", 1);
    let (_directory, path) = fixture(&invalid)?;

    let error = analyze(&path, 0.0, 1_000)
        .err()
        .context("an unrepresentable duration should fail")?;
    assert!(error.to_string().contains("too large"), "{error:#}");
    Ok(())
}

#[test]
fn an_empty_interval_set_is_rejected() -> Result<()> {
    let (_directory, path) = fixture(MEASUREMENTS)?;

    let error = analyze_measurements(
        &path,
        0,
        NonZeroUsize::new(1_000).unwrap(),
        Shrinkage::NONE,
        &[],
    )
    .err()
    .context("an empty interval set should fail")?;
    assert!(error.to_string().contains("interval"), "{error}");
    Ok(())
}
