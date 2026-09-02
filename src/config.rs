use crate::{Interval, Repetitions, Revision, Shrinkage};

use anyhow::{Context, Result, bail, ensure};
use clap::{
    Arg, ArgAction, Args, Command, CommandFactory, FromArgMatches, Parser, builder::Str,
    parser::ValueSource,
};
use std::{
    env,
    ffi::OsString,
    fs,
    io::ErrorKind,
    num::{NonZeroU64, NonZeroUsize},
    path::{Component, Path, PathBuf},
};
use toml::{Table, Value};

const MIN_DRAWS: usize = 1_000;
const DEFAULT_CONFIG: &str = "foil.toml";
const BENCHMARKS: &str = "benchmarks";
const ENV: &str = "env";
const ENV_ID: &str = "envs";
const SUITE_STARTUP: &str = "suite-startup";
const SUITE_TEARDOWN: &str = "suite-teardown";
const WORKTREE_STARTUP: &str = "worktree-startup";
const WORKTREE_TEARDOWN: &str = "worktree-teardown";
const STARTUP: &str = "startup";
const STARTUP_EACH_RUN: &str = "startup-each-run";
const TEARDOWN_EACH_RUN: &str = "teardown-each-run";
const TEARDOWN: &str = "teardown";
const COMMAND: &str = "command";
const WORKING_DIRECTORY: &str = "working-directory";
const WORKING_DIRECTORY_ID: &str = "working_directory";
const PER_BENCHMARK: [(&str, &str); 3] = [
    (COMMAND, COMMAND),
    (WORKING_DIRECTORY_ID, WORKING_DIRECTORY),
    (ENV_ID, ENV),
];

#[derive(Default)]
pub(crate) struct SuiteLifecycle {
    pub(crate) startup: Vec<OsString>,
    pub(crate) startup_each_run: Vec<OsString>,
    pub(crate) teardown_each_run: Vec<OsString>,
    pub(crate) teardown: Vec<OsString>,
}

#[derive(Default)]
pub(crate) struct WorktreeLifecycle {
    pub(crate) startup: Vec<OsString>,
    pub(crate) teardown: Vec<OsString>,
}

#[derive(Default)]
pub(crate) struct BenchmarkLifecycle {
    pub(crate) startup: Vec<OsString>,
    pub(crate) startup_each_run: Vec<OsString>,
    pub(crate) teardown_each_run: Vec<OsString>,
    pub(crate) teardown: Vec<OsString>,
}

#[derive(Parser)]
#[command(name = "foil")]
#[command(version)]
#[command(about = "Paired Git revision benchmarking", long_about = None)]
pub(crate) struct Cli {
    #[command(flatten)]
    suite: SuiteConfig,

    #[command(flatten)]
    run: RunConfig,

    #[command(flatten)]
    selectors: Selectors,
}

#[derive(Args)]
struct Selectors {
    /// TOML configuration file. May also define lifecycle hooks and named benchmarks.
    ///
    /// Defaults to `foil.toml`, which is read when present.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Names of benchmarks from the configuration file's `[benchmarks]` table, e.g. `--benchmark a b c`.
    ///
    /// A benchmark's table may override run options, including `command`. With none
    /// named, every benchmark in the table runs; with no `[benchmarks]` table,
    /// `command` and the options above are used as given.
    #[arg(long = "benchmark", value_name = "NAME", num_args = 1..)]
    benchmarks: Vec<String>,
}

#[derive(Parser)]
#[command(disable_help_flag = true, disable_version_flag = true)]
struct SelectorCli {
    #[command(flatten)]
    selectors: Selectors,
}

#[derive(Args)]
struct SuiteConfig {
    /// Git revision used as the baseline.
    #[arg(short, long, default_value = "main")]
    baseline: String,

    /// Git revision containing the candidate changes.
    #[arg(short, long, default_value = "HEAD")]
    candidate: String,

    /// Set a seed for reproducible benchmarking.
    #[arg(long)]
    seed: Option<u64>,
}

#[derive(Args)]
pub(crate) struct RunConfig {
    /// Give this benchmark its own pair of worktrees.
    #[arg(long)]
    pub(crate) isolate: bool,

    /// Control shrinkage of the adjusted mean runtime difference toward 0 by specifying a prior number of no-change pseudo-observations.
    #[arg(long, default_value = "0")]
    pub(crate) shrinkage: Shrinkage,

    /// Directory where generated output files are written.
    #[arg(long, value_name = "DIR", required = true)]
    pub(crate) output_dir: PathBuf,

    /// Number of benchmark runs per branch.
    ///
    /// Each repetition runs both branches, for `repetitions * 2` total runs.
    #[arg(short, long, required = true, value_parser = parse_repetitions)]
    pub(crate) repetitions: NonZeroUsize,

    /// Pairs per randomized run-order block.
    #[arg(long, default_value = "4")]
    pub(crate) block_size: NonZeroUsize,

    /// Number of Bayesian bootstrap draws.
    #[arg(long, value_parser = parse_draws, default_value = "10000")]
    pub(crate) draws: NonZeroUsize,

    /// Fail when a single benchmark run takes longer than this many seconds.
    ///
    /// Applies to each measured run separately; lifecycle commands are never limited.
    #[arg(long, value_name = "SECONDS")]
    pub(crate) timeout: Option<NonZeroU64>,

    /// Central credible interval widths.
    #[arg(
        long = "interval",
        num_args = 1..,
        default_values = ["0.5", "0.8", "0.9"]
    )]
    pub(crate) intervals: Vec<Interval>,

    /// Working directory for the benchmark and lifecycle commands, relative to the worktree root.
    #[arg(long, value_name = "DIR", value_parser = parse_working_directory)]
    pub(crate) working_directory: Option<PathBuf>,

    /// Environment variable for the benchmark and lifecycle commands, as `KEY=VALUE`.
    ///
    /// May be repeated. In a configuration file, may instead be given as a table.
    #[arg(long = "env", value_name = "KEY=VALUE", value_parser = parse_env)]
    pub(crate) envs: Vec<(String, String)>,

    /// Benchmark program and arguments.
    ///
    /// Place the command after `--`, for example: `foil --output-dir benchmark/ --repetitions 10 -- Rscript benchmark.R`.
    #[arg(last = true, required = true, num_args = 1..)]
    pub(crate) command: Vec<OsString>,
}

struct Configuration {
    path: PathBuf,
    suite_lifecycle: SuiteLifecycle,
    worktree_lifecycle: WorktreeLifecycle,
    top: Table,
    benchmarks: Table,
}

pub(crate) struct Suite {
    pub(crate) config: ResolvedSuiteConfig,
    pub(crate) lifecycle: SuiteLifecycle,
    pub(crate) worktree_lifecycle: WorktreeLifecycle,
    pub(crate) runs: Vec<(Option<String>, Benchmark)>,
}

pub(crate) struct Benchmark {
    pub(crate) config: RunConfig,
    pub(crate) lifecycle: BenchmarkLifecycle,
}

pub(crate) struct ResolvedSuiteConfig {
    pub(crate) baseline: Revision,
    pub(crate) candidate: Revision,
    pub(crate) seed: u64,
}

struct ResolvedCli {
    suite: SuiteConfig,
    run: RunConfig,
    benchmark_lifecycle: BenchmarkLifecycle,
}

impl Cli {
    pub(crate) fn suite() -> Result<Suite> {
        let arguments: Vec<OsString> = env::args_os().collect();
        if let Some(action) = arguments
            .iter()
            .skip(1)
            .take_while(|argument| *argument != "--")
            .find(|argument| {
                matches!(
                    argument.to_str(),
                    Some("-h" | "--help" | "-V" | "--version")
                )
            })
        {
            Self::command().get_matches_from([arguments[0].clone(), action.clone()]);
            unreachable!("Clap exits after displaying help or version.");
        }

        let selectors = SelectorCli::parse_from(selector_arguments(&arguments)).selectors;
        let configuration = read_config(selectors.config)?;
        validate_config(&configuration)?;

        let names = if selectors.benchmarks.is_empty() {
            configuration.benchmarks.keys().cloned().collect()
        } else {
            selectors.benchmarks
        };

        let runs = if names.is_empty() {
            vec![(None, resolve(&configuration, None, &arguments)?)]
        } else {
            names
                .into_iter()
                .map(|name| {
                    let cli = resolve(&configuration, Some(&name), &arguments)?;
                    Ok((Some(name), cli))
                })
                .collect::<Result<_>>()?
        };
        let (_, first) = runs.first().expect("At least one run is always produced.");
        let baseline = Revision::resolve(first.suite.baseline.clone())?;
        let candidate = Revision::resolve(first.suite.candidate.clone())?;
        let seed = first.suite.seed.unwrap_or_else(rand::random);
        let runs = runs
            .into_iter()
            .map(|(name, resolved)| {
                (
                    name,
                    Benchmark {
                        config: resolved.run,
                        lifecycle: resolved.benchmark_lifecycle,
                    },
                )
            })
            .collect();

        Ok(Suite {
            config: ResolvedSuiteConfig {
                baseline,
                candidate,
                seed,
            },
            lifecycle: configuration.suite_lifecycle,
            worktree_lifecycle: configuration.worktree_lifecycle,
            runs,
        })
    }
}

fn selector_arguments(arguments: &[OsString]) -> Vec<OsString> {
    let command = SelectorCli::command();
    let selectors: Vec<_> = command
        .get_arguments()
        .filter_map(|argument| {
            argument
                .get_long()
                .map(|long| (format!("--{long}"), argument.get_action().clone()))
        })
        .collect();
    let mut selected = vec![arguments[0].clone()];
    let mut position = 1;

    while position < arguments.len() {
        let Some(text) = arguments[position].to_str() else {
            position += 1;
            continue;
        };
        if text == "--" {
            break;
        }

        let Some((_, action)) = selectors
            .iter()
            .find(|(option, _)| text == option || text.starts_with(&format!("{option}=")))
        else {
            position += 1;
            continue;
        };
        selected.push(arguments[position].clone());

        if !action.takes_values() {
            position += 1;
            continue;
        }

        position += 1;
        if text.contains('=') && !matches!(action, ArgAction::Append) {
            continue;
        }
        while position < arguments.len() {
            let value = &arguments[position];
            if value.to_str().is_some_and(|value| value.starts_with('-')) {
                break;
            }
            selected.push(value.clone());
            position += 1;
            if !matches!(action, ArgAction::Append) {
                break;
            }
        }
    }

    selected
}

fn read_config(path: Option<PathBuf>) -> Result<Configuration> {
    let (path, optional) = path.map_or_else(|| (DEFAULT_CONFIG.into(), true), |path| (path, false));
    let mut top: Table = match fs::read_to_string(&path) {
        Err(error) if optional && error.kind() == ErrorKind::NotFound => Table::new(),
        result => result
            .with_context(|| format!("Failed to read {}.", path.display()))?
            .parse()
            .with_context(|| format!("Failed to parse {}.", path.display()))?,
    };

    let benchmarks = match top.remove(BENCHMARKS) {
        None => Table::new(),
        Some(Value::Table(benchmarks)) => benchmarks,
        Some(_) => bail!("{} must set `{BENCHMARKS}` to a table.", path.display()),
    };
    let suite_lifecycle = SuiteLifecycle {
        startup: take_command(&mut top, SUITE_STARTUP)?,
        startup_each_run: take_command(&mut top, STARTUP_EACH_RUN)?,
        teardown_each_run: take_command(&mut top, TEARDOWN_EACH_RUN)?,
        teardown: take_command(&mut top, SUITE_TEARDOWN)?,
    };
    let worktree_lifecycle = WorktreeLifecycle {
        startup: take_command(&mut top, WORKTREE_STARTUP)?,
        teardown: take_command(&mut top, WORKTREE_TEARDOWN)?,
    };

    Ok(Configuration {
        path,
        suite_lifecycle,
        worktree_lifecycle,
        top,
        benchmarks,
    })
}

fn validate_config(config: &Configuration) -> Result<()> {
    configure(Cli::command(), &config.path, &config.top)?;

    for (name, value) in &config.benchmarks {
        let mut table = value.as_table().cloned().with_context(|| {
            format!(
                "{} must set benchmark `{name}` to a table.",
                config.path.display()
            )
        })?;
        if let Some(key) = table
            .keys()
            .find(|key| option_in::<SuiteConfig>(key) || is_top_lifecycle_key(key))
        {
            bail!(
                "{} benchmark `{name}` cannot set suite-level `{key}`.",
                config.path.display()
            );
        }
        take_benchmark_lifecycle(&mut table)?;
        configure(Cli::command(), &config.path, &table)?;
    }

    Ok(())
}

fn is_top_lifecycle_key(key: &str) -> bool {
    matches!(
        key,
        SUITE_STARTUP | SUITE_TEARDOWN | WORKTREE_STARTUP | WORKTREE_TEARDOWN
    )
}

fn option_in<A: Args>(key: &str) -> bool {
    A::augment_args(Command::new("options"))
        .get_arguments()
        .any(|argument| configuration_key(argument) == Some(key))
}

fn resolve(
    config: &Configuration,
    benchmark: Option<&str>,
    arguments: &[OsString],
) -> Result<ResolvedCli> {
    let mut values = config.top.clone();
    let mut benchmark_lifecycle = BenchmarkLifecycle::default();
    if let Some(name) = benchmark {
        let mut overrides = config
            .benchmarks
            .get(name)
            .and_then(Value::as_table)
            .cloned()
            .with_context(|| {
                format!("{} has no benchmark named `{name}`.", config.path.display())
            })?;
        benchmark_lifecycle = take_benchmark_lifecycle(&mut overrides)?;
        if let (Some(Value::Table(base)), Some(Value::Table(over))) =
            (values.get(ENV), overrides.get(ENV))
        {
            let mut env = base.clone();
            env.extend(over.clone());
            overrides.insert(ENV.to_owned(), Value::Table(env));
        }
        values.extend(overrides);
    }

    let command = configure(Cli::command(), &config.path, &values)?;
    let mut matches = command.get_matches_from(arguments);
    if let Some(name) = benchmark {
        for (id, key) in PER_BENCHMARK {
            ensure!(
                matches.value_source(id) != Some(ValueSource::CommandLine),
                "`{key}` cannot be passed on the command line; set it in [benchmarks.{name}] instead."
            );
        }
    }
    let cli = Cli::from_arg_matches_mut(&mut matches).map_err(|error| error.exit())?;
    let Cli {
        suite,
        run,
        selectors: _,
    } = cli;

    Ok(ResolvedCli {
        suite,
        run,
        benchmark_lifecycle,
    })
}

fn take_benchmark_lifecycle(table: &mut Table) -> Result<BenchmarkLifecycle> {
    Ok(BenchmarkLifecycle {
        startup: take_command(table, STARTUP)?,
        startup_each_run: take_command(table, STARTUP_EACH_RUN)?,
        teardown_each_run: take_command(table, TEARDOWN_EACH_RUN)?,
        teardown: take_command(table, TEARDOWN)?,
    })
}

fn take_command(table: &mut Table, key: &str) -> Result<Vec<OsString>> {
    let Some(value) = table.remove(key) else {
        return Ok(Vec::new());
    };
    let values = defaults(&value).with_context(|| format!("`{key}` is not a command list."))?;
    Ok(values
        .into_iter()
        .map(|value| value.to_string().into())
        .collect())
}

fn configure(mut command: Command, path: &Path, config: &Table) -> Result<Command> {
    for (key, value) in config {
        if option_in::<Selectors>(key) {
            bail!(
                "{} cannot set `{key}`; `--{key}` is command-line-only.",
                path.display()
            );
        }

        command = configure_value(command, path, key, value)?;
    }
    Ok(command)
}

fn configure_value(command: Command, path: &Path, key: &str, value: &Value) -> Result<Command> {
    let argument = command
        .get_arguments()
        .find(|argument| configuration_key(argument) == Some(key))
        .with_context(|| format!("{} sets `{key}`, which is not an option.", path.display()))?;
    let id = argument.get_id().to_string();
    let repeatable = matches!(argument.get_action(), ArgAction::Append);

    let defaults = defaults(value).with_context(|| {
        format!(
            "{} must set `{key}` to a string, number, boolean, list of those, or table of strings.",
            path.display()
        )
    })?;
    ensure!(
        !defaults.is_empty(),
        "{} sets `{key}` to an empty list.",
        path.display()
    );
    ensure!(
        repeatable || defaults.len() == 1,
        "{} sets `{key}` to {} values, but it takes only one.",
        path.display(),
        defaults.len()
    );

    Ok(command.mut_arg(id, |argument| {
        argument.required(false).default_values(defaults)
    }))
}

fn configuration_key(argument: &Arg) -> Option<&str> {
    argument
        .get_long()
        .or_else(|| argument.is_last_set().then(|| argument.get_id().as_str()))
}

fn defaults(value: &Value) -> Option<Vec<Str>> {
    match value {
        Value::Array(array) => array.iter().map(scalar).collect(),
        Value::Table(table) => table
            .iter()
            .map(|(key, value)| Some(format!("{key}={}", value.as_str()?).into()))
            .collect(),
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

fn parse_env(text: &str) -> Result<(String, String)> {
    let (key, value) = text
        .split_once('=')
        .with_context(|| format!("`{text}` is not `KEY=VALUE`."))?;
    ensure!(
        !key.is_empty(),
        "Environment variable name cannot be empty."
    );

    Ok((key.to_owned(), value.to_owned()))
}

fn parse_working_directory(text: &str) -> Result<PathBuf> {
    let path = PathBuf::from(text);
    ensure!(
        !path.has_root()
            && path
                .components()
                .all(|component| matches!(component, Component::CurDir | Component::Normal(_))),
        "Working directory must be relative to the worktree root and cannot contain `..`."
    );

    Ok(path)
}

fn parse_repetitions(text: &str) -> Result<NonZeroUsize> {
    let repetitions: NonZeroUsize = text
        .parse()
        .with_context(|| format!("`{text}` is not a positive integer."))?;

    ensure!(
        repetitions.get() >= Repetitions::<()>::MINIMUM,
        "At least {} repetitions are required.",
        Repetitions::<()>::MINIMUM
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
