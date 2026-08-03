//! Rendering a posterior to human- and machine-readable output.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

pub fn report_posterior(posterior: &[(f64, f64)], intervals: &[f64]) -> String {
    use std::fmt::Write as _;
    let (mut baseline, mut candidate): (Vec<f64>, Vec<f64>) = posterior.iter().copied().unzip();
    let mut absolute = Vec::with_capacity(posterior.len());
    let mut relative = Vec::with_capacity(posterior.len());

    for &(baseline, candidate) in posterior {
        absolute.push(candidate - baseline);
        relative.push(100.0 * (candidate / baseline - 1.0));
    }

    baseline.sort_by(f64::total_cmp);
    candidate.sort_by(f64::total_cmp);
    absolute.sort_by(f64::total_cmp);
    relative.sort_by(f64::total_cmp);

    let quantile =
        |posterior: &[f64], p: f64| posterior[((posterior.len() - 1) as f64 * p).round() as usize];
    // TODO: move to custom duration derived type?
    let (scale, unit) = match quantile(&baseline, 0.5).max(quantile(&candidate, 0.5)) {
        x if x >= 1.0 => (1.0, "s"),
        x if x >= 1e-3 => (1e3, "ms"),
        x if x >= 1e-6 => (1e6, "µs"),
        _ => (1e9, "ns"),
    };
    let mut report = String::new();

    writeln!(
        report,
        "Baseline:  {:.1}{unit}",
        scale * quantile(&baseline, 0.5)
    )
    .unwrap();

    writeln!(
        report,
        "Candidate: {:.1}{unit}",
        scale * quantile(&candidate, 0.5)
    )
    .unwrap();

    writeln!(report).unwrap();

    writeln!(
        report,
        "Change: {:+.1}{unit} ({:+.2}%)",
        scale * quantile(&absolute, 0.5),
        quantile(&relative, 0.5),
    )
    .unwrap();

    for &width in intervals {
        let tail = (1.0 - width) / 2.0;

        writeln!(
            report,
            "  {:.0}% CrI: [{:+.1}, {:+.1}]{unit} ({:+.2}%, {:+.2}%)",
            100.0 * width,
            scale * quantile(&absolute, tail),
            scale * quantile(&absolute, 1.0 - tail),
            quantile(&relative, tail),
            quantile(&relative, 1.0 - tail),
        )
        .unwrap();
    }

    let probability_faster =
        absolute.partition_point(|&change| change < 0.0) as f64 / absolute.len() as f64;

    writeln!(report).unwrap();

    writeln!(
        report,
        "P(candidate faster): {:.1}%",
        100.0 * probability_faster
    )
    .unwrap();

    report
}

// NOTE: could swap to CSV crate if this gets annoying
pub fn write_posterior_csv(path: &Path, posterior: &[(f64, f64)]) -> Result<()> {
    let file =
        File::create(path).with_context(|| format!("Failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);

    writeln!(writer, "baseline,candidate")?;

    for &(baseline, candidate) in posterior {
        writeln!(writer, "{baseline},{candidate}")?;
    }

    writer.flush()?;
    Ok(())
}
