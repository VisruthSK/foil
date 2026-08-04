use b3::{
    BenchmarkLog, Config, Interval, Pair, Posterior, Repetition, Repetitions, Revision, RunCommand,
    RunOrder, Shrinkage, Time, Worktree, write_config_json, write_measurements_csv,
    write_posterior_csv,
};

use anyhow::{Context, Result, ensure};
use clap::{Arg, Command, CommandFactory, FromArgMatches, Parser, builder::Str};
use rand::{SeedableRng, rngs::Xoshiro256PlusPlus};
use std::{ffi::OsString, fs, io::ErrorKind, num::NonZeroUsize, path::PathBuf};
use tempfile::tempdir;
use toml::{Table, Value};

const MIN_DRAWS: usize = 1_000;
const CONFIG: &str = "config";
const DEFAULT_CONFIG: &str = "b3.toml";

#[derive(Parser)]
#[command(name = "b3")]
#[command(version)]
#[command(about = "Bayesian Branch Benchmarking", long_about = None)]
struct Cli {
    // TODO: Reorder args into cogent order.
    /// Git revision used as the baseline.
    #[arg(short, long, default_value = "main")]
    baseline: String,

    /// Git revision containing the candidate changes.
    #[arg(short, long, default_value = "HEAD")]
    candidate: String,

    /// Control shrinkage of the adjusted mean runtime difference toward 0 by specifying a prior number of no-change pseudo-observations.
    #[arg(long, default_value = "0")]
    shrinkage: Shrinkage,

    /// Directory where generated output files are written.
    #[arg(long, value_name = "DIR", required = true)]
    output_dir: PathBuf,

    /// Number of benchmark runs per branch.
    ///
    /// Each repetition runs both branches, for `repetitions * 2` total runs.
    #[arg(short, long, required = true, value_parser = parse_repetitions)]
    repetitions: NonZeroUsize,

    /// Number of Bayesian bootstrap draws.
    #[arg(long, value_parser = parse_draws, default_value = "10000")]
    draws: NonZeroUsize,

    /// Central credible interval widths.
    #[arg(long = "interval", default_values = ["0.5", "0.8", "0.98"])]
    intervals: Vec<Interval>,

    /// Set a seed for reproducible benchmarking.
    #[arg(long)]
    seed: Option<u64>,

    /// TOML file whose keys are the long names of the options above, or `command`.
    ///
    /// Defaults to `b3.toml`, which is read when present.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Benchmark program and arguments.
    ///
    /// Place the command after `--`, for example: `b3 --output-dir benchmark/ --repetitions 10 -- Rscript benchmark.R`.
    #[arg(last = true, required = true, num_args = 1..)]
    command: Vec<OsString>,
}

impl Cli {
    fn layered() -> Result<Self> {
        let path = Self::command()
            .ignore_errors(true)
            .disable_help_flag(true)
            .disable_version_flag(true)
            .get_matches()
            .remove_one::<PathBuf>(CONFIG);
        let mut matches = configure(Self::command(), path)?.get_matches();

        Self::from_arg_matches_mut(&mut matches).map_err(|error| error.exit())
    }
}

fn configure(command: Command, path: Option<PathBuf>) -> Result<Command> {
    let (path, optional) = path.map_or((DEFAULT_CONFIG.into(), true), |path| (path, false));
    let config: Table = match fs::read_to_string(&path) {
        Err(error) if optional && error.kind() == ErrorKind::NotFound => return Ok(command),
        result => result
            .with_context(|| format!("Failed to read {}.", path.display()))?
            .parse()
            .with_context(|| format!("Failed to parse {}.", path.display()))?,
    };

    config
        .into_iter()
        .try_fold(command, |command, (key, value)| {
            ensure!(
                key != CONFIG,
                "{} cannot set `{key}`; pass --{CONFIG} instead.",
                path.display()
            );

            let id = command
                .get_arguments()
                .find(|argument| configuration_key(argument) == Some(key.as_str()))
                .with_context(|| {
                    format!("{} sets `{key}`, which is not an option.", path.display())
                })?
                .get_id()
                .to_string();
            let defaults = defaults(&value).with_context(|| {
                format!(
                    "{} must set `{key}` to a string, number, boolean, or list of those.",
                    path.display()
                )
            })?;
            ensure!(
                !defaults.is_empty(),
                "{} sets `{key}` to an empty list.",
                path.display()
            );

            Ok(command.mut_arg(id, |argument| {
                argument.required(false).default_values(defaults)
            }))
        })
}

fn configuration_key(argument: &Arg) -> Option<&str> {
    argument
        .get_long()
        .or_else(|| argument.is_last_set().then(|| argument.get_id().as_str()))
}

fn defaults(value: &Value) -> Option<Vec<Str>> {
    match value {
        Value::Array(array) => array.iter().map(scalar).collect(),
        value => Some(vec![scalar(value)?]),
    }
}

fn scalar(value: &Value) -> Option<Str> {
    match value {
        Value::String(text) => Some(text.clone().into()),
        Value::Integer(_) | Value::Float(_) | Value::Boolean(_) => Some(value.to_string().into()),
        _ => None,
    }
}

fn parse_repetitions(text: &str) -> Result<NonZeroUsize> {
    let repetitions: NonZeroUsize = text
        .parse()
        .with_context(|| format!("`{text}` is not a positive integer."))?;

    ensure!(
        repetitions.get() >= Repetitions::MINIMUM,
        "At least {} repetitions are required.",
        Repetitions::MINIMUM
    );

    Ok(repetitions)
}

fn parse_draws(text: &str) -> Result<NonZeroUsize> {
    let draws: NonZeroUsize = text
        .parse()
        .with_context(|| format!("`{text}` is not a positive integer."))?;

    ensure!(
        draws.get() >= MIN_DRAWS,
        "At least {MIN_DRAWS} draws are required."
    );

    Ok(draws)
}

fn main() -> Result<()> {
    let Cli {
        baseline,
        candidate,
        shrinkage,
        output_dir,
        repetitions: repetition_count,
        draws,
        intervals,
        seed,
        config: _,
        command,
    } = Cli::layered()?;

    let worktree_dir = tempdir().context("Failed to create temporary directory.")?;
    fs::create_dir_all(&output_dir).with_context(|| {
        format!(
            "Failed to create output directory {}.",
            output_dir.display()
        )
    })?;
    let seed = seed.unwrap_or_else(rand::random);
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);

    let config_command = command.clone();
    let mut command = command.into_iter();
    let program = command
        .next()
        .expect("Clap requires at least one command argument.");
    let benchmark = RunCommand::new(program, command.collect());

    let worktrees = Pair {
        baseline: Worktree::create(
            worktree_dir.path().join("baseline"),
            Revision::resolve(baseline)?,
        )?,
        candidate: Worktree::create(
            worktree_dir.path().join("candidate"),
            Revision::resolve(candidate)?,
        )?,
    };
    let repetition_count = repetition_count.get();

    let config_path = output_dir.join("config.json");
    write_config_json(
        &config_path,
        &Config {
            seed,
            repetitions: repetition_count,
            draws: draws.get(),
            shrinkage,
            baseline: worktrees.baseline.revision(),
            candidate: worktrees.candidate.revision(),
            command: &config_command,
        },
    )
    .with_context(|| format!("Failed to write {}.", config_path.display()))?;

    let mut measured_repetitions = Vec::with_capacity(repetition_count);

    let log_path = output_dir.join("benchmark.log");
    let mut log = BenchmarkLog::new(
        fs::File::create(&log_path)
            .with_context(|| format!("Failed to create {}.", log_path.display()))?,
        repetition_count * 2,
    );

    for order in RunOrder::schedule(repetition_count, &mut rng) {
        let [first, second] = order.sides();

        // TODO: Better handling of failing runs to find systematic errors. Should record and write out?
        let first_output = log.measure(&benchmark, first, worktrees.get(first))?;
        ensure!(
            first_output.exit_status().success(),
            "The {first} benchmark failed with {}.",
            first_output.exit_status()
        );

        let second_output = log.measure(&benchmark, second, worktrees.get(second))?;
        ensure!(
            second_output.exit_status().success(),
            "The {second} benchmark failed with {}.",
            second_output.exit_status()
        );

        let outputs = Pair::from_execution_order([first_output, second_output], order);
        measured_repetitions.push(Repetition { outputs, order });
    }

    // Clears the progress line before the report starts printing.
    drop(log);

    let repetitions = Repetitions::try_from(measured_repetitions)?;

    let measurements_path = output_dir.join("measurements.csv");
    write_measurements_csv(&measurements_path, &repetitions)
        .with_context(|| format!("Failed to write {}.", measurements_path.display()))?;

    // TODO: Add memory report. Needs one output path per metric.
    let posterior = Posterior::<Time>::bootstrap(&repetitions, draws, shrinkage, &mut rng)?;

    let posterior_path = output_dir.join("posterior.csv");
    write_posterior_csv(&posterior_path, &posterior)
        .with_context(|| format!("Failed to write {}.", posterior_path.display()))?;

    let report = format!(
        "Comparing candidate ({}) to baseline ({}) with {repetition_count} paired repetitions and {} Bayesian bootstrap draws.\n\n{}",
        worktrees.candidate.revision().name(),
        worktrees.baseline.revision().name(),
        draws.get(),
        posterior.summarize(&intervals)?,
    );
    print!("{report}");

    let report_path = output_dir.join("report.txt");
    fs::write(&report_path, &report)
        .with_context(|| format!("Failed to write {}.", report_path.display()))?;

    Ok(())
}
