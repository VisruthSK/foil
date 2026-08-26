use anyhow::{Result, ensure};
use foil::{Interval, Metric, Shrinkage, analyze_measurements};
use std::{fs, num::NonZeroUsize, process::Command};
use tempfile::{TempDir, tempdir};

const REQUIRED: &str =
    "output-dir = 'bench'\nrepetitions = 12\ninterval = [0.5, 0.8]\ncommand = ['benchmark']\n";

const PREAMBLE: &str = "baseline = 'HEAD'\n\
    candidate = 'HEAD'\n\
    output-dir = 'bench'\n\
    repetitions = 10\n\
    draws = 1000\n\
    interval = [0.5, 0.8]\n\
    seed = 0\n";

const CONFIG: &str = "\
    baseline = 'baseline-branch'\n\
    candidate = 'candidate-branch'\n\
    shrinkage = 2.5\n\
    output-dir = 'configured'\n\
    repetitions = 12\n\
    draws = 2000\n\
    interval = [0.5, 0.8]\n\
    seed = 0\n\
    command = ['Rscript', 'benchmark.R']\n";

const BUILTIN_USAGE: &str = "--output-dir <DIR> --repetitions <REPETITIONS> -- <COMMAND>...";

#[test]
fn package_name_is_foil_bench() {
    assert_eq!(env!("CARGO_PKG_NAME"), "foil-bench");
}

fn project(files: &[(&str, &str)]) -> Result<TempDir> {
    let directory = tempdir()?;

    for (name, contents) in files {
        fs::write(directory.path().join(name), contents)?;
    }

    Ok(directory)
}

fn run(project: &TempDir, arguments: &[&str]) -> Result<(bool, String, String)> {
    let output = Command::new(env!("CARGO_BIN_EXE_foil"))
        .args(arguments)
        .current_dir(project.path())
        .output()?;

    Ok((
        output.status.success(),
        String::from_utf8(output.stdout)?,
        String::from_utf8(output.stderr)?,
    ))
}

fn help(project: &TempDir, arguments: &[&str]) -> Result<String> {
    let (succeeded, stdout, stderr) = run(project, &[arguments, &["--help"]].concat())?;
    ensure!(succeeded, "foil --help failed with {stderr}");

    Ok(stdout)
}

fn failure(project: &TempDir, arguments: &[&str]) -> Result<String> {
    let (succeeded, stdout, stderr) = run(project, arguments)?;
    ensure!(!succeeded, "foil unexpectedly succeeded with {stdout}");

    Ok(stderr)
}

fn repository(config: &str) -> Result<TempDir> {
    let project = project(&[("foil.toml", config)])?;

    git(&project, &["init", "--quiet"])?;
    git(
        &project,
        &["commit", "--quiet", "--allow-empty", "--message", "root"],
    )?;

    Ok(project)
}

fn git(project: &TempDir, arguments: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=foil",
            "-c",
            "user.email=foil@example.invalid",
        ])
        .args(arguments)
        .current_dir(project.path())
        .status()?;
    ensure!(status.success(), "git {arguments:?} failed with {status}");

    Ok(())
}

#[test]
fn a_complete_configuration_runs_without_any_arguments() -> Result<()> {
    let project = repository(
        "\
        baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        repetitions = 10\n\
        draws = 1000\n\
        interval = [0.5, 0.8]\n\
        seed = 0\n\
        output-dir = 'benchmark'\n\
        command = ['git', '--version']\n",
    )?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;

    ensure!(succeeded, "foil failed with {stderr}");
    assert!(stdout.contains("10 paired repetitions"), "{stdout}");
    assert!(stdout.contains("1000 Bayesian bootstrap draws"), "{stdout}");

    for artifact in [
        "config.json",
        "benchmark.log",
        "measurements.csv",
        "posterior.csv",
        "report.txt",
    ] {
        let path = project.path().join("benchmark").join(artifact);
        assert!(path.is_file(), "{} is missing.", path.display());
    }

    let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        project.path().join("benchmark/config.json"),
    )?)?;
    assert_eq!(config["seed"], 0);
    assert_eq!(config["foil_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(config["command"], serde_json::json!(["git", "--version"]));

    Ok(())
}

#[test]
fn saved_measurements_reproduce_the_full_analysis() -> Result<()> {
    let project = repository(&format!("{PREAMBLE}command = ['git', '--version']\n"))?;
    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    let output = project.path().join("bench");
    let config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(output.join("config.json"))?)?;
    assert_eq!(config["foil_version"], env!("CARGO_PKG_VERSION"));
    let seed = config["seed"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("The recorded seed is not an integer."))?;
    let intervals = [
        Interval::new(0.5)?,
        Interval::new(0.8)?,
        Interval::new(0.98)?,
    ];
    let analysis = analyze_measurements(
        &output.join("measurements.csv"),
        seed,
        NonZeroUsize::new(1_000).unwrap(),
        Shrinkage::NONE,
        &intervals,
    )?;
    let expected: Vec<(f64, f64)> = fs::read_to_string(output.join("posterior.csv"))?
        .lines()
        .skip(1)
        .map(|line| {
            let (baseline, candidate) = line
                .split_once(',')
                .ok_or_else(|| anyhow::anyhow!("Invalid posterior row."))?;
            Ok((baseline.parse()?, candidate.parse()?))
        })
        .collect::<Result<_>>()?;
    let actual: Vec<_> = analysis
        .posterior
        .draws()
        .iter()
        .map(|draw| (draw.baseline.base(), draw.candidate.base()))
        .collect();

    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn builtin_defaults_apply_without_a_configuration_file() -> Result<()> {
    let project = project(&[])?;
    let help = help(&project, &[])?;

    for default in [
        "[default: main]",
        "[default: HEAD]",
        "[default: 0]",
        "[default: 4]",
        "[default: 10000]",
        "[default: 0.5 0.8 0.90]",
    ] {
        assert!(help.contains(default), "{default} is missing from\n{help}");
    }
    assert!(help.contains(BUILTIN_USAGE), "{help}");

    Ok(())
}

#[test]
fn help_ignores_configuration_defaults() -> Result<()> {
    let project = project(&[("foil.toml", CONFIG)])?;
    let help = help(&project, &[])?;

    for default in [
        "[default: main]",
        "[default: HEAD]",
        "[default: 0]",
        "[default: 10000]",
        "[default: 0.5 0.8 0.90]",
    ] {
        assert!(help.contains(default), "{default} is missing from\n{help}");
    }
    assert!(!help.contains("baseline-branch"), "{help}");
    assert!(!help.contains("[default: Rscript"), "{help}");

    Ok(())
}

#[test]
fn configuration_satisfies_required_options() -> Result<()> {
    let project = project(&[("foil.toml", "repetitions = 12\n")])?;
    let error = failure(&project, &[])?;

    assert!(error.contains("--output-dir <DIR>"), "{error}");
    assert!(error.contains("<COMMAND>..."), "{error}");
    assert!(!error.contains("--repetitions"), "{error}");

    Ok(())
}

#[test]
fn arguments_override_the_configuration() -> Result<()> {
    let project = project(&[("foil.toml", CONFIG)])?;
    let error = failure(&project, &["--repetitions", "5"])?;

    assert!(
        error.contains("At least 10 repetitions are required."),
        "{error}"
    );

    Ok(())
}

/// An interval whose tails are narrower than one repetition's share is refused
/// before any benchmark runs, not printed after them.
#[test]
fn an_interval_wider_than_the_repetitions_support_is_rejected_at_startup() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
         candidate = 'HEAD'\n\
         output-dir = 'bench'\n\
         repetitions = 10\n\
         interval = [0.8, 0.90]\n\
         command = ['git', '--version']\n",
    )?;
    let error = failure(&project, &[])?;

    assert!(error.contains("widest supported interval"), "{error}");
    assert!(error.contains("80%"), "{error}");

    Ok(())
}

#[test]
fn an_unnamed_command_override_keeps_configured_options() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}command = ['git', 'cat-file', '-p', 'absent-object']\n"
    ))?;

    let (succeeded, _, stderr) = run(&project, &["--", "git", "--version"])?;
    ensure!(succeeded, "foil failed with {stderr}");

    let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        project.path().join("bench/config.json"),
    )?)?;
    assert_eq!(config["command"], serde_json::json!(["git", "--version"]));
    assert_eq!(config["repetitions"], 10);
    Ok(())
}

#[test]
fn configured_values_go_unused_when_an_argument_supplies_them() -> Result<()> {
    for (setting, argument, value, unused) in [
        ("draws = 5", "--draws", "3000", "draws"),
        ("interval = [1.5]", "--interval", "0.25", "Interval width"),
        ("shrinkage = -1", "--shrinkage", "1", "Shrinkage"),
    ] {
        let project = project(&[("foil.toml", &format!("{REQUIRED}{setting}\n"))])?;
        let error = failure(&project, &[argument, value])?;

        assert!(!error.contains(unused), "{setting} gave {error}");
    }

    Ok(())
}

#[test]
fn help_does_not_read_an_explicit_configuration() -> Result<()> {
    let project = project(&[("foil.toml", "not valid TOML")])?;
    let help = help(&project, &["--config", "absent.toml"])?;

    assert!(help.contains("[default: 10000]"), "{help}");
    assert!(help.contains(BUILTIN_USAGE), "{help}");

    Ok(())
}

#[test]
fn configured_values_meet_the_same_bounds_as_arguments() -> Result<()> {
    for (setting, expected) in [
        ("draws = 5", "At least 1000 draws are required."),
        ("repetitions = 2", "At least 10 repetitions are required."),
        ("interval = 1.5", "Interval width must be between 0 and 1"),
        ("shrinkage = -1", "Shrinkage must be finite and nonnegative"),
    ] {
        let project = project(&[("foil.toml", setting)])?;
        let error = failure(&project, &[])?;

        assert!(error.contains(expected), "{setting} gave {error}");
    }

    Ok(())
}

#[test]
fn unusable_configurations_are_reported() -> Result<()> {
    for (contents, expected) in [
        (
            "output_dir = 'bench'",
            "sets `output_dir`, which is not an option",
        ),
        (
            "intervals = [0.5]",
            "sets `intervals`, which is not an option",
        ),
        ("config = 'other.toml'", "cannot set `config`"),
        ("interval = [[0.5, 0.8]]", "must set `interval` to a string"),
        ("seed = { value = 7 }", "must set `seed` to a string"),
        ("seed = 1979-05-27", "must set `seed` to a string"),
        ("interval = []", "sets `interval` to an empty list"),
        ("command = []", "sets `command` to an empty list"),
        (
            "baseline = ['a', 'b']",
            "sets `baseline` to 2 values, but it takes only one",
        ),
        ("draws =", "Failed to parse"),
    ] {
        let project = project(&[("foil.toml", contents)])?;
        let error = failure(&project, &[])?;

        assert!(error.contains(expected), "{contents} gave {error}");
    }

    Ok(())
}

#[test]
fn a_missing_explicit_configuration_is_an_error() -> Result<()> {
    let project = project(&[])?;
    let error = failure(&project, &["--config", "absent.toml"])?;

    assert!(error.contains("Failed to read absent.toml."), "{error}");

    Ok(())
}

#[test]
fn selectors_are_found_after_run_options() -> Result<()> {
    let project = repository("not valid TOML")?;
    fs::write(
        project.path().join("other.toml"),
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        repetitions = 10\n\
        draws = 1000\n\
        interval = [0.5, 0.8]\n\
        [benchmarks.first]\n\
        command = ['git', '--version']\n\
        [benchmarks.second]\n\
        command = ['git', '--version']\n",
    )?;
    let (succeeded, stdout, stderr) = run(
        &project,
        &[
            "--output-dir",
            "bench",
            "--config",
            "other.toml",
            "--draws",
            "1000",
            "--benchmark",
            "second",
        ],
    )?;

    ensure!(succeeded, "foil failed with {stderr}");
    assert!(project.path().join("bench/second/report.txt").is_file());
    assert!(!project.path().join("bench/first").exists());
    assert!(!stdout.contains("first: Comparing"), "{stdout}");

    Ok(())
}

#[test]
fn a_benchmark_supplies_its_own_command() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}\n\
        [benchmarks.parse]\n\
        command = ['git', '--version']\n"
    ))?;
    let (succeeded, stdout, stderr) = run(&project, &["--benchmark", "parse"])?;

    ensure!(succeeded, "foil failed with {stderr}");
    assert!(stdout.contains("10 paired repetitions"), "{stdout}");

    Ok(())
}

#[test]
fn help_does_not_resolve_a_selected_benchmark() -> Result<()> {
    let project = project(&[(
        "foil.toml",
        "output-dir = 'bench'\n\
        repetitions = 10\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', '--version']\n\
        repetitions = 15\n",
    )])?;
    let help = help(&project, &["--benchmark", "parse"])?;

    assert!(help.contains("[default: 10000]"), "{help}");
    assert!(!help.contains("[default: 15]"), "{help}");
    assert!(!help.contains("git --version"), "{help}");

    Ok(())
}

#[test]
fn an_argument_overrides_a_selected_benchmark() -> Result<()> {
    let project = project(&[(
        "foil.toml",
        "output-dir = 'bench'\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', '--version']\n\
        repetitions = 15\n",
    )])?;
    let error = failure(&project, &["--benchmark", "parse", "--repetitions", "5"])?;

    assert!(
        error.contains("At least 10 repetitions are required."),
        "{error}"
    );

    Ok(())
}

#[test]
fn a_benchmarks_own_options_cannot_be_passed_as_arguments() -> Result<()> {
    let project = project(&[(
        "foil.toml",
        "output-dir = 'bench'\n\
        repetitions = 10\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', '--version']\n",
    )])?;

    for (arguments, key) in [
        (&["--working-directory", "sub"][..], "working-directory"),
        (&["--env", "KEY=VALUE"][..], "env"),
        (&["--", "git", "--version"][..], "command"),
    ] {
        let error = failure(
            &project,
            &[&["--benchmark", "parse"][..], arguments].concat(),
        )?;

        assert!(
            error.contains(&format!(
                "`{key}` cannot be passed on the command line; set it in [benchmarks.parse] instead."
            )),
            "{arguments:?} gave {error}"
        );
    }

    Ok(())
}

#[test]
fn an_unknown_benchmark_is_reported() -> Result<()> {
    let project = project(&[("foil.toml", "output-dir = 'bench'\n")])?;
    let error = failure(&project, &["--benchmark", "nope"])?;

    assert!(error.contains("has no benchmark named `nope`"), "{error}");

    Ok(())
}

#[test]
fn benchmark_definitions_are_not_exposed_as_options() -> Result<()> {
    let project = project(&[(
        "foil.toml",
        "[benchmarks.parse]\ncommand = ['git', '--version']\n",
    )])?;
    let help = help(&project, &[])?;

    assert!(!help.contains("parse"), "{help}");

    Ok(())
}

#[test]
fn unusable_benchmarks_are_reported() -> Result<()> {
    for (contents, arguments, expected) in [
        ("benchmark = 'parse'", &[][..], "cannot set `benchmark`"),
        (
            "benchmarks = 1",
            &["--benchmark", "parse"][..],
            "must set `benchmarks` to a table",
        ),
        (
            "[benchmarks]\nparse = 1",
            &["--benchmark", "parse"][..],
            "must set benchmark `parse` to a table",
        ),
        (
            "[benchmarks.parse]\ncommand = ['echo']\nenv = 1",
            &["--benchmark", "parse"][..],
            "is not `KEY=VALUE`",
        ),
        (
            "[benchmarks.parse]\ncommand = ['echo']\n[benchmarks.parse.env]\nVAR = 1",
            &["--benchmark", "parse"][..],
            "must set `env` to a string, number, boolean, list of those, or table of strings",
        ),
        (
            "[benchmarks.parse]\ncommand = ['echo']\nbogus = 1",
            &["--benchmark", "parse"][..],
            "sets `bogus`, which is not an option",
        ),
    ] {
        let project = project(&[("foil.toml", contents)])?;
        let error = failure(&project, arguments)?;

        assert!(error.contains(expected), "{contents} gave {error}");
    }

    Ok(())
}

#[test]
fn a_malformed_benchmarks_table_is_always_rejected() -> Result<()> {
    let project = project(&[("foil.toml", "output-dir = 'bench'\nbenchmarks = 1\n")])?;
    let error = failure(&project, &["--repetitions", "5"])?;

    assert!(
        error.contains("must set `benchmarks` to a table"),
        "{error}"
    );

    Ok(())
}

#[test]
fn a_benchmark_can_set_its_working_directory() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}\n\
        [benchmarks.parse]\n\
        command = ['git', 'ls-files', '--error-unmatch', '.gitkeep']\n\
        working-directory = 'sub'\n"
    ))?;
    fs::create_dir(project.path().join("sub"))?;
    fs::write(project.path().join("sub").join(".gitkeep"), "")?;
    git(&project, &["add", "sub"])?;
    git(&project, &["commit", "--quiet", "--message", "add sub"])?;

    let (succeeded, _, stderr) = run(&project, &["--benchmark", "parse"])?;
    ensure!(succeeded, "foil failed with {stderr}");

    Ok(())
}

#[test]
fn a_benchmark_can_set_environment_variables() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}\n\
        [benchmarks.parse]\n\
        command = ['git', 'config', '--get-regexp', '^user.name$', '^benchmark-env$']\n\
        \n\
        [benchmarks.parse.env]\n\
        GIT_CONFIG_COUNT = '1'\n\
        GIT_CONFIG_KEY_0 = 'user.name'\n\
        GIT_CONFIG_VALUE_0 = 'benchmark-env'\n"
    ))?;

    let (succeeded, _, stderr) = run(&project, &["--benchmark", "parse"])?;
    ensure!(succeeded, "foil failed with {stderr}");

    Ok(())
}

#[test]
fn a_benchmarks_env_merges_with_the_top_level_env() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}command = ['git', 'config', 'user.name']\n\
        \n\
        [env]\n\
        GIT_CONFIG_COUNT = '1'\n\
        GIT_CONFIG_KEY_0 = 'user.name'\n\
        GIT_CONFIG_VALUE_0 = 'top-level-env'\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', 'config', '--get-regexp', '^user.name$', '^benchmark-env$']\n\
        \n\
        [benchmarks.parse.env]\n\
        GIT_CONFIG_VALUE_0 = 'benchmark-env'\n"
    ))?;

    let (succeeded, _, stderr) = run(&project, &["--benchmark", "parse"])?;
    ensure!(succeeded, "foil failed with {stderr}");

    Ok(())
}

#[test]
fn working_directory_and_env_are_ordinary_options() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}working-directory = 'sub'\n\
        command = ['git', 'config', '--get-regexp', '^user.name$', '^top-level-env$']\n\
        \n\
        [env]\n\
        GIT_CONFIG_COUNT = '1'\n\
        GIT_CONFIG_KEY_0 = 'user.name'\n\
        GIT_CONFIG_VALUE_0 = 'top-level-env'\n"
    ))?;
    fs::create_dir(project.path().join("sub"))?;
    fs::write(project.path().join("sub").join(".gitkeep"), "")?;
    git(&project, &["add", "sub"])?;
    git(&project, &["commit", "--quiet", "--message", "add sub"])?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    Ok(())
}

#[test]
fn an_explicit_env_argument_overrides_the_configuration() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}command = ['git', 'config', '--get-regexp', '^user.name$', '^argument-env$']\n\
        \n\
        [env]\n\
        GIT_CONFIG_COUNT = '1'\n\
        GIT_CONFIG_KEY_0 = 'user.name'\n\
        GIT_CONFIG_VALUE_0 = 'configured-env'\n"
    ))?;

    let (succeeded, _, stderr) = run(
        &project,
        &[
            "--env",
            "GIT_CONFIG_COUNT=1",
            "--env",
            "GIT_CONFIG_KEY_0=user.name",
            "--env",
            "GIT_CONFIG_VALUE_0=argument-env",
        ],
    )?;
    ensure!(succeeded, "foil failed with {stderr}");

    Ok(())
}

#[test]
fn benchmark_startup_runs_in_each_worktree_before_the_measured_runs() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}[benchmarks.test]\n\
        startup = ['git', 'config', '--file', 'marker.txt', 'startup.ran', 'yes']\n\
        command = ['git', 'config', '--file', 'marker.txt', '--get-regexp', '^startup.ran$', '^yes$']\n"
    ))?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    Ok(())
}

#[test]
fn a_failing_benchmark_startup_stops_before_the_measured_runs() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}[benchmarks.test]\n\
        startup = ['git', 'cat-file', '-p', 'absent-object']\n\
        teardown = ['git', 'tag', '--force', 'startup-cleaned-up']\n\
        command = ['git', '--version']\n"
    ))?;

    let error = failure(&project, &[])?;

    assert!(error.contains("The baseline startup failed."), "{error}");

    let bench = project.path().join("bench/test");
    assert!(bench.join("config.json").is_file());
    assert!(!bench.join("benchmark.log").exists());
    git(
        &project,
        &["rev-parse", "--verify", "refs/tags/startup-cleaned-up"],
    )?;

    Ok(())
}

#[test]
fn stale_outputs_are_removed_for_every_selected_benchmark() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}[benchmarks.first]\n\
        startup = ['git', 'cat-file', '-p', 'absent-object']\n\
        command = ['git', '--version']\n\
        [benchmarks.second]\n\
        command = ['git', '--version']\n"
    ))?;
    let bench = project.path().join("bench");
    for name in ["first", "second"] {
        let output = bench.join(name);
        fs::create_dir_all(&output)?;
        for artifact in [
            "config.json",
            "benchmark.log",
            "measurements.csv",
            "posterior.csv",
            "report.txt",
        ] {
            fs::write(output.join(artifact), "stale")?;
        }
    }
    fs::write(bench.join("report_short.txt"), "stale")?;

    let error = failure(&project, &[])?;
    assert!(error.contains("The baseline startup failed."), "{error}");

    for name in ["first", "second"] {
        let output = bench.join(name);
        assert_ne!(fs::read_to_string(output.join("config.json"))?, "stale");
        for artifact in [
            "benchmark.log",
            "measurements.csv",
            "posterior.csv",
            "report.txt",
        ] {
            assert!(
                !output.join(artifact).exists(),
                "{name}/{artifact} survived"
            );
        }
    }
    assert!(!bench.join("report_short.txt").exists());

    Ok(())
}

#[test]
fn a_generated_output_that_cannot_be_removed_is_fatal() -> Result<()> {
    let project = repository(&format!("{PREAMBLE}command = ['git', '--version']\n"))?;
    fs::create_dir_all(project.path().join("bench/posterior.csv"))?;
    fs::write(project.path().join("bench/report.txt"), "stale")?;

    let error = failure(&project, &[])?;
    assert!(error.contains("Failed to remove"), "{error}");
    assert!(!project.path().join("bench/report.txt").exists());

    Ok(())
}

#[test]
fn teardown_runs_after_the_measured_runs() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}[benchmarks.test]\n\
        teardown = ['git', 'cat-file', '-p', 'absent-object']\n\
        command = ['git', '--version']\n"
    ))?;

    let error = failure(&project, &[])?;

    assert!(error.contains("The baseline teardown failed."), "{error}");

    let bench = project.path().join("bench/test");
    let log = fs::read_to_string(bench.join("benchmark.log"))?;
    assert_eq!(log.lines().count(), 20, "{log}");

    let csv = fs::read_to_string(bench.join("measurements.csv"))?;
    assert_eq!(csv.lines().count(), 11, "{csv}");

    Ok(())
}

#[test]
fn candidate_teardown_runs_after_baseline_teardown_fails() -> Result<()> {
    let project = repository("")?;
    let baseline = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project.path())
        .output()?;
    let baseline = String::from_utf8(baseline.stdout)?.trim().to_owned();
    git(
        &project,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "--message",
            "candidate",
        ],
    )?;
    let candidate = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project.path())
        .output()?;
    let candidate = String::from_utf8(candidate.stdout)?.trim().to_owned();
    git(
        &project,
        &["update-ref", "refs/tags/teardown-state", &candidate],
    )?;
    fs::write(
        project.path().join("foil.toml"),
        format!(
            "baseline = '{baseline}'\n\
             candidate = '{candidate}'\n\
             output-dir = 'bench'\n\
             repetitions = 10\n\
             draws = 1000\n\
             interval = [0.5, 0.8]\n\
             [benchmarks.test]\n\
             teardown = ['git', 'update-ref', 'refs/tags/teardown-state', '{baseline}', 'HEAD']\n\
             command = ['git', '--version']\n"
        ),
    )?;

    let error = failure(&project, &[])?;
    assert!(error.contains("The baseline teardown failed."), "{error}");

    let state = Command::new("git")
        .args(["rev-parse", "refs/tags/teardown-state"])
        .current_dir(project.path())
        .output()?;
    assert!(state.status.success());
    assert_eq!(String::from_utf8(state.stdout)?.trim(), baseline);

    Ok(())
}

#[test]
fn successful_lifecycle_output_is_suppressed() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}startup = ['git', '--version']\n\
         teardown = ['git', '--version']\n\
         [benchmarks.test]\n\
         startup = ['git', '--version']\n\
         teardown = ['git', '--version']\n\
         command = ['git', 'rev-parse', '--is-inside-work-tree']\n"
    ))?;

    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");
    assert!(!stdout.contains("git version"), "{stdout}");
    assert!(!stderr.contains("git version"), "{stderr}");

    Ok(())
}

#[test]
fn suite_and_benchmark_each_run_startups_compose() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}startup-each-run = ['git', 'config', '--file', 'marker.txt', 'suite.ran', 'yes']\n\
         [benchmarks.test]\n\
         startup-each-run = ['git', 'config', '--file', 'marker.txt', '--rename-section', 'suite', 'benchmark']\n\
         command = ['git', 'config', '--file', 'marker.txt', '--get-regexp', '^benchmark.ran$', '^yes$']\n"
    ))?;

    let (succeeded, _stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");
    Ok(())
}

#[test]
fn each_run_teardowns_run_after_a_failed_benchmark() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}teardown-each-run = ['git', 'tag', '--force', 'suite-each-run-torn-down']\n\
         [benchmarks.test]\n\
         teardown-each-run = ['git', 'tag', '--force', 'benchmark-each-run-torn-down']\n\
         command = ['git', 'cat-file', '-p', 'absent-object']\n"
    ))?;

    let (succeeded, stdout, stderr) = run(&project, &[])?;
    assert!(!succeeded, "foil unexpectedly succeeded with {stdout}");
    assert!(stderr.contains("benchmark failed"), "{stderr}");
    for tag in [
        "refs/tags/suite-each-run-torn-down",
        "refs/tags/benchmark-each-run-torn-down",
    ] {
        git(&project, &["rev-parse", "--verify", tag])?;
    }

    Ok(())
}

#[test]
fn removed_lifecycle_names_are_rejected() -> Result<()> {
    for key in ["setup", "prepare"] {
        let project = project(&[("foil.toml", &format!("{REQUIRED}{key} = ['git']\n"))])?;
        let error = failure(&project, &[])?;
        assert!(
            error.contains(&format!("sets `{key}`, which is not an option")),
            "{error}"
        );
    }

    Ok(())
}

#[test]
fn a_failing_benchmark_still_leaves_a_measurements_file() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}command = ['git', 'cat-file', '-p', 'absent-object']\n"
    ))?;

    let error = failure(&project, &[])?;

    assert!(error.contains("benchmark failed"), "{error}");
    assert_eq!(
        fs::read_to_string(project.path().join("bench").join("measurements.csv"))?,
        "repetition,order,baseline_seconds,candidate_seconds\n"
    );

    Ok(())
}

#[test]
fn a_timed_out_benchmark_is_logged() -> Result<()> {
    let command = if cfg!(windows) {
        "['ping', '-n', '31', '127.0.0.1']"
    } else {
        "['sleep', '30']"
    };
    let project = repository(&format!("{PREAMBLE}timeout = 1\ncommand = {command}\n"))?;

    let error = failure(&project, &[])?;
    assert!(error.contains("timed out"), "{error}");

    let log = fs::read_to_string(project.path().join("bench/benchmark.log"))?;
    let entry: serde_json::Value = serde_json::from_str(
        log.lines()
            .next()
            .ok_or_else(|| anyhow::anyhow!("The timed-out run was not logged."))?,
    )?;
    assert_eq!(entry["timed_out"], true, "{entry}");

    Ok(())
}

#[test]
fn redirected_stderr_reports_each_benchmark_run() -> Result<()> {
    let project = repository(&format!("{PREAMBLE}command = ['git', '--version']\n"))?;

    let (succeeded, _, stderr) = run(&project, &[])?;

    ensure!(succeeded, "foil failed with {stderr}");
    let progress: Vec<_> = stderr
        .lines()
        .filter(|line| line.ends_with(" benchmark"))
        .collect();
    assert_eq!(progress.len(), 20, "{stderr}");
    assert!(progress[0].contains(" 1/20 benchmark"), "{stderr}");
    Ok(())
}

#[test]
fn modified_tracked_files_print_a_warning() -> Result<()> {
    let project = repository(&format!("{PREAMBLE}command = ['git', '--version']\n"))?;
    fs::write(project.path().join("tracked.txt"), "before")?;
    git(&project, &["add", "tracked.txt"])?;
    git(&project, &["commit", "--quiet", "--message", "tracked"])?;
    fs::write(project.path().join("tracked.txt"), "after")?;

    let (succeeded, _, stderr) = run(&project, &[])?;

    ensure!(succeeded, "foil failed with {stderr}");
    assert!(stderr.contains("modified tracked files"), "{stderr}");

    Ok(())
}

#[test]
fn a_clean_working_tree_prints_no_warning() -> Result<()> {
    let project = repository(&format!("{PREAMBLE}command = ['git', '--version']\n"))?;

    let (succeeded, _, stderr) = run(&project, &[])?;

    ensure!(succeeded, "foil failed with {stderr}");
    assert!(!stderr.contains("modified tracked files"), "{stderr}");

    Ok(())
}

#[test]
fn teardown_still_runs_when_a_benchmark_fails() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}[benchmarks.test]\n\
        teardown = ['git', 'tag', '--force', 'teardown-ran']\n\
        command = ['git', 'cat-file', '-p', 'absent-object']\n"
    ))?;

    let error = failure(&project, &[])?;

    assert!(error.contains("benchmark failed"), "{error}");
    git(
        &project,
        &["rev-parse", "--verify", "refs/tags/teardown-ran"],
    )?;

    Ok(())
}

#[test]
fn a_teardown_failure_is_reported_alongside_a_benchmark_failure() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}[benchmarks.test]\n\
        teardown = ['git', 'cat-file', '-p', 'absent-object']\n\
        command = ['git', 'cat-file', '-p', 'absent-object']\n"
    ))?;

    let error = failure(&project, &[])?;

    assert!(error.contains("benchmark failed"), "{error}");
    assert!(error.contains("The baseline teardown failed."), "{error}");

    Ok(())
}

#[test]
fn a_benchmark_inherits_the_top_level_command() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}command = ['git', '--version']\n\
        \n\
        [benchmarks.inherits]\n\
        draws = 2000\n"
    ))?;
    let (succeeded, stdout, stderr) = run(&project, &["--benchmark", "inherits"])?;

    ensure!(succeeded, "foil failed with {stderr}");
    assert!(stdout.contains("2000 Bayesian bootstrap draws"), "{stdout}");

    Ok(())
}

#[test]
fn suite_and_benchmark_lifecycles_remain_distinct() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}startup = ['git', '--version']\n\
        teardown = ['git', '--version']\n\
        \n\
        [benchmarks.parse]\n\
        startup = ['git', 'config', '--file', 'marker.txt', 'startup.ran', 'benchmark-startup']\n\
        teardown = ['git', 'cat-file', '-p', 'absent-object']\n\
        command = ['git', 'config', '--file', 'marker.txt', '--get-regexp', '^startup.ran$', '^benchmark-startup$']\n"
    ))?;

    let error = failure(&project, &[])?;

    assert!(error.contains("The baseline teardown failed."), "{error}");

    Ok(())
}

#[test]
fn an_empty_benchmark_lifecycle_does_not_clear_the_suite_lifecycle() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}startup = ['git', 'cat-file', '-p', 'absent-object']\n\
        teardown = ['git', 'tag', 'suite-cleaned-up']\n\
        \n\
        [benchmarks.standalone]\n\
        startup = []\n\
        teardown = []\n\
        command = ['git', '--version']\n"
    ))?;

    let error = failure(&project, &[])?;
    assert!(error.contains("The suite startup failed."), "{error}");
    assert!(
        project
            .path()
            .join("bench/standalone/config.json")
            .is_file()
    );
    git(
        &project,
        &["rev-parse", "--verify", "refs/tags/suite-cleaned-up"],
    )?;

    Ok(())
}

const SUITE: &str = "baseline = 'HEAD'\n\
    candidate = 'HEAD'\n\
    output-dir = 'bench'\n\
    repetitions = 10\n\
    draws = 1000\n\
    interval = [0.5, 0.8]\n\
    seed = 0\n\
    \n\
    [benchmarks.first]\n\
    command = ['git', '--version']\n\
    \n\
    [benchmarks.second]\n\
    command = ['git', '--version']\n\
    \n\
    [benchmarks.third]\n\
    command = ['git', '--version']\n";

#[test]
fn every_named_benchmark_runs_by_default() -> Result<()> {
    let project = repository(SUITE)?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    for name in ["first", "second", "third"] {
        assert!(
            stdout.contains(&format!("{name}: Comparing candidate")),
            "{stdout}"
        );
        assert!(
            project
                .path()
                .join("bench")
                .join(name)
                .join("report.txt")
                .is_file(),
            "{name}/report.txt is missing"
        );
    }

    let short = fs::read_to_string(project.path().join("bench").join("report_short.txt"))?;
    for name in ["first", "second", "third"] {
        assert!(short.contains(&format!("{name}: ")), "{short}");
    }
    assert!(short.contains("->"), "{short}");

    Ok(())
}

#[test]
fn a_single_benchmark_argument_selects_a_subset() -> Result<()> {
    let project = repository(SUITE)?;
    let (succeeded, _, stderr) = run(&project, &["--benchmark", "first", "third"])?;
    ensure!(succeeded, "foil failed with {stderr}");

    let bench = project.path().join("bench");
    assert!(bench.join("first").join("report.txt").is_file());
    assert!(bench.join("third").join("report.txt").is_file());
    assert!(!bench.join("second").exists());

    let short = fs::read_to_string(bench.join("report_short.txt"))?;
    assert!(short.contains("first:"), "{short}");
    assert!(short.contains("third:"), "{short}");
    assert!(!short.contains("second:"), "{short}");

    Ok(())
}

#[test]
fn a_lone_benchmark_skips_the_short_report() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}\n\
        [benchmarks.parse]\n\
        command = ['git', '--version']\n"
    ))?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    assert!(!stdout.contains("parse: Comparing candidate"), "{stdout}");

    let bench = project.path().join("bench");
    assert!(bench.join("parse").join("report.txt").is_file());
    assert!(!bench.join("report_short.txt").exists());

    Ok(())
}

#[test]
fn report_short_uses_the_top_level_output_directory() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}\n\
        [benchmarks.a]\n\
        command = ['git', '--version']\n\
        output-dir = 'special'\n\
        \n\
        [benchmarks.b]\n\
        command = ['git', '--version']\n"
    ))?;
    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    assert!(
        project
            .path()
            .join("bench")
            .join("report_short.txt")
            .is_file()
    );
    assert!(
        project
            .path()
            .join("special")
            .join("a")
            .join("report.txt")
            .is_file()
    );

    Ok(())
}

#[test]
fn an_output_dir_argument_relocates_the_short_report_too() -> Result<()> {
    let project = repository(SUITE)?;
    let (succeeded, _, stderr) = run(&project, &["--output-dir", "elsewhere"])?;
    ensure!(succeeded, "foil failed with {stderr}");

    assert!(!project.path().join("bench").exists());

    let elsewhere = project.path().join("elsewhere");
    assert!(elsewhere.join("report_short.txt").is_file());
    for name in ["first", "second", "third"] {
        assert!(elsewhere.join(name).join("report.txt").is_file());
    }

    Ok(())
}

#[test]
fn benchmarks_run_in_declaration_order() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        interval = [0.5, 0.8]\n\
        \n\
        [benchmarks.zebra]\n\
        command = ['git', '--version']\n\
        \n\
        [benchmarks.apple]\n\
        command = ['git', '--version']\n",
    )?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    assert!(
        stdout.find("zebra: Comparing") < stdout.find("apple: Comparing"),
        "{stdout}"
    );

    Ok(())
}

#[test]
fn suite_settings_cannot_be_overridden_by_a_benchmark() -> Result<()> {
    for key in ["baseline", "candidate", "seed"] {
        let value = if key == "seed" { "1" } else { "'HEAD'" };
        let project = project(&[(
            "foil.toml",
            &format!("[benchmarks.parse]\ncommand = ['git', '--version']\n{key} = {value}\n"),
        )])?;
        let error = failure(&project, &[])?;

        assert!(
            error.contains(&format!("cannot set suite-level `{key}`")),
            "{error}"
        );
    }

    Ok(())
}

#[test]
fn an_environment_variable_name_cannot_be_empty() -> Result<()> {
    let project = project(&[("foil.toml", REQUIRED)])?;
    let error = failure(&project, &["--env", "=value"])?;

    assert!(
        error.contains("Environment variable name cannot be empty"),
        "{error}"
    );

    Ok(())
}

#[test]
fn working_directory_cannot_escape_the_worktree() -> Result<()> {
    for directory in ["/etc", "../outside"] {
        let project = project(&[("foil.toml", REQUIRED)])?;
        let error = failure(&project, &["--working-directory", directory])?;

        assert!(
            error.contains("must be relative to the worktree root"),
            "{error}"
        );
    }

    Ok(())
}

#[test]
fn every_benchmark_uses_the_suite_seed() -> Result<()> {
    let project = repository(SUITE)?;
    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    let seeds = ["first", "second", "third"].map(|name| -> Result<u64> {
        let text = fs::read_to_string(project.path().join("bench").join(name).join("config.json"))?;
        Ok(serde_json::from_str::<serde_json::Value>(&text)?["seed"]
            .as_u64()
            .expect("seed is an integer"))
    });
    let seeds = seeds.into_iter().collect::<Result<Vec<_>>>()?;

    assert!(seeds.windows(2).all(|pair| pair[0] == pair[1]), "{seeds:?}");

    Ok(())
}

#[test]
fn worktrees_are_shared_unless_isolation_is_requested() -> Result<()> {
    const WORKTREES: &str = "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        interval = [0.5, 0.8]\n\
        [benchmarks.first]\n\
        command = ['git', 'config', '--file', 'shared.marker', 'first.ran', 'yes']\n\
        [benchmarks.second]\n\
        command = ['git', 'config', '--file', 'shared.marker', '--get-regexp', '^first.ran$', '^yes$']\n";

    let shared = repository(WORKTREES)?;
    let (succeeded, _, stderr) = run(&shared, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    let isolated = repository(WORKTREES)?;
    let error = failure(&isolated, &["--isolate"])?;
    assert!(error.contains("benchmark failed"), "{error}");

    let selective = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        interval = [0.5, 0.8]\n\
        [benchmarks.isolated]\n\
        isolate = true\n\
        command = ['git', 'update-index', '--force-remove', 'sentinel']\n\
        [benchmarks.shared_one]\n\
        startup = ['git', 'config', '--file', 'shared.marker', 'shared.ran', 'yes']\n\
        command = ['git', 'ls-files', '--error-unmatch', 'sentinel']\n\
        [benchmarks.shared_two]\n\
        command = ['git', 'config', '--file', 'shared.marker', '--get-regexp', '^shared.ran$', '^yes$']\n",
    )?;
    fs::write(selective.path().join("sentinel"), "tracked")?;
    git(&selective, &["add", "sentinel"])?;
    git(
        &selective,
        &["commit", "--quiet", "--message", "add sentinel"],
    )?;
    let (succeeded, _, stderr) = run(&selective, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");
    Ok(())
}
