use super::RunCommand;
use crate::{RunOrder, Side};
use anyhow::{Context, Result, ensure};
use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use std::{
    env,
    ffi::OsString,
    num::NonZeroUsize,
    process::Command,
    thread,
    time::{Duration, Instant},
};
use tempfile::tempdir;

const WORK_NS: &str = "B3_OVERHEAD_WORK_NS";

fn setting(name: &str, default: usize) -> Result<usize> {
    env::var(name).map_or(Ok(default), |value| {
        let value = value.parse()?;
        ensure!(value > 0, "{name} must be positive.");
        Ok(value)
    })
}

fn arguments() -> Vec<OsString> {
    [
        "--exact",
        "run::overhead::overhead_child",
        "--ignored",
        "--nocapture",
    ]
    .map(OsString::from)
    .to_vec()
}

fn internal_ns(stdout: Vec<u8>) -> Result<i128> {
    Ok(String::from_utf8(stdout)?
        .lines()
        .find_map(|line| line.strip_prefix("B3_INTERNAL_NS="))
        .context("The overhead child did not report its elapsed time.")?
        .parse()?)
}

fn quantile(sorted: &[f64], probability: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * probability).round() as usize]
}

#[test]
#[ignore = "release-only process-runner overhead diagnostic"]
fn process_runner_overhead() -> Result<()> {
    ensure!(
        !cfg!(debug_assertions),
        "Overhead diagnostics are release-only; run with `cargo test --release`."
    );
    let runs = setting("B3_OVERHEAD_RUNS", 200)?;
    let directory = tempdir()?;
    let executable = env::current_exe()?;
    eprintln!("mode,work_ms,runs,mean_us,p50_us,p90_us,p99_us,min_us,max_us,negative");

    for (mode, timeout) in [
        ("default", None),
        ("timeout", Some(Duration::from_secs(60))),
    ] {
        for work_ms in [0_u64, 1, 10, 100] {
            let mut overhead = Vec::with_capacity(runs);
            let work_ns = (work_ms * 1_000_000).to_string();
            let schedule = RunOrder::schedule(
                runs,
                NonZeroUsize::new(4).unwrap(),
                &mut Xoshiro256PlusPlus::seed_from_u64(0),
            );

            for order in schedule {
                let mut raw = None;
                let mut foil = None;
                for side in order.sides() {
                    match side {
                        Side::Baseline => {
                            let started = Instant::now();
                            let output = Command::new(&executable)
                                .args(arguments())
                                .current_dir(directory.path())
                                .env(WORK_NS, &work_ns)
                                .output()?;
                            ensure!(
                                output.status.success(),
                                "Raw child failed with {}.",
                                output.status
                            );
                            raw = Some((
                                started.elapsed().as_nanos() as i128,
                                internal_ns(output.stdout)?,
                            ));
                        }
                        Side::Candidate => {
                            let command = RunCommand::new(
                                executable.clone().into_os_string(),
                                arguments(),
                                None,
                                vec![(WORK_NS.to_owned(), work_ns.clone())],
                            )
                            .with_timeout(timeout);
                            let run = command.run_in(directory.path())?;
                            foil = Some((
                                run.output.elapsed().as_nanos() as i128,
                                internal_ns(run.stdout)?,
                            ));
                        }
                    }
                }
                let (raw_external, raw_internal) = raw.expect("The raw child ran.");
                let (foil_external, foil_internal) = foil.expect("The Foil child ran.");
                overhead.push(
                    ((foil_external - foil_internal) - (raw_external - raw_internal)) as f64
                        / 1_000.0,
                );
            }

            overhead.sort_by(f64::total_cmp);
            let mean = overhead.iter().sum::<f64>() / overhead.len() as f64;
            let negative = overhead.iter().filter(|&&value| value < 0.0).count();
            eprintln!(
                "{mode},{work_ms},{runs},{mean:.3},{:.3},{:.3},{:.3},{:.3},{:.3},{negative}",
                quantile(&overhead, 0.5),
                quantile(&overhead, 0.9),
                quantile(&overhead, 0.99),
                overhead[0],
                overhead[overhead.len() - 1],
            );
        }
    }
    Ok(())
}

#[test]
#[ignore]
fn overhead_child() {
    let work = Duration::from_nanos(
        env::var(WORK_NS)
            .expect("The parent supplies work duration.")
            .parse()
            .expect("Work duration is an integer."),
    );
    let started = Instant::now();
    thread::sleep(work);
    println!("B3_INTERNAL_NS={}", started.elapsed().as_nanos());
}
