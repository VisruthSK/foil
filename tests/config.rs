use anyhow::{Result, ensure};
use std::{fs, process::Command};
use tempfile::{TempDir, tempdir};

const REQUIRED: &str = "output-dir = 'bench'\nrepetitions = 12\ncommand = ['benchmark']\n";

const CONFIG: &str = "\
    baseline = 'baseline-branch'\n\
    candidate = 'candidate-branch'\n\
    shrinkage = 2.5\n\
    output-dir = 'configured'\n\
    repetitions = 12\n\
    draws = 2000\n\
    interval = [0.5, 0.9]\n\
    seed = 7\n\
    command = ['Rscript', 'benchmark.R']\n";

const BUILTIN_USAGE: &str = "--output-dir <DIR> --repetitions <REPETITIONS> -- <COMMAND>...";

fn project(files: &[(&str, &str)]) -> Result<TempDir> {
    let directory = tempdir()?;

    for (name, contents) in files {
        fs::write(directory.path().join(name), contents)?;
    }

    Ok(directory)
}

fn run(project: &TempDir, arguments: &[&str]) -> Result<(bool, String, String)> {
    let output = Command::new(env!("CARGO_BIN_EXE_b3"))
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
    ensure!(succeeded, "b3 --help failed with {stderr}");

    Ok(stdout)
}

fn failure(project: &TempDir, arguments: &[&str]) -> Result<String> {
    let (succeeded, stdout, stderr) = run(project, arguments)?;
    ensure!(!succeeded, "b3 unexpectedly succeeded with {stdout}");

    Ok(stderr)
}

fn repository(config: &str) -> Result<TempDir> {
    let project = project(&[("b3.toml", config)])?;

    git(&project, &["init", "--quiet"])?;
    git(
        &project,
        &["commit", "--quiet", "--allow-empty", "--message", "root"],
    )?;

    Ok(project)
}

fn git(project: &TempDir, arguments: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(["-c", "user.name=b3", "-c", "user.email=b3@example.invalid"])
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
        seed = 1\n\
        output-dir = 'benchmark'\n\
        command = ['git', '--version']\n",
    )?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;

    ensure!(succeeded, "b3 failed with {stderr}");
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
        "[default: 10000]",
        "[default: 0.5 0.8 0.98]",
    ] {
        assert!(help.contains(default), "{default} is missing from\n{help}");
    }
    assert!(help.contains(BUILTIN_USAGE), "{help}");

    Ok(())
}

#[test]
fn help_ignores_configuration_defaults() -> Result<()> {
    let project = project(&[("b3.toml", CONFIG)])?;
    let help = help(&project, &[])?;

    for default in [
        "[default: main]",
        "[default: HEAD]",
        "[default: 0]",
        "[default: 10000]",
        "[default: 0.5 0.8 0.98]",
    ] {
        assert!(help.contains(default), "{default} is missing from\n{help}");
    }
    assert!(!help.contains("baseline-branch"), "{help}");
    assert!(!help.contains("[default: Rscript"), "{help}");

    Ok(())
}

#[test]
fn configuration_satisfies_required_options() -> Result<()> {
    let project = project(&[("b3.toml", "repetitions = 12\n")])?;
    let error = failure(&project, &[])?;

    assert!(error.contains("--output-dir <DIR>"), "{error}");
    assert!(error.contains("<COMMAND>..."), "{error}");
    assert!(!error.contains("--repetitions"), "{error}");

    Ok(())
}

#[test]
fn arguments_override_the_configuration() -> Result<()> {
    let project = project(&[("b3.toml", CONFIG)])?;
    let error = failure(&project, &["--repetitions", "5"])?;

    assert!(
        error.contains("At least 10 repetitions are required."),
        "{error}"
    );

    Ok(())
}

#[test]
fn configured_values_go_unused_when_an_argument_supplies_them() -> Result<()> {
    for (setting, argument, value, unused) in [
        ("draws = 5", "--draws", "3000", "draws"),
        ("interval = [1.5]", "--interval", "0.25", "Interval width"),
        ("shrinkage = -1", "--shrinkage", "1", "Shrinkage"),
    ] {
        let project = project(&[("b3.toml", &format!("{REQUIRED}{setting}\n"))])?;
        let error = failure(&project, &[argument, value])?;

        assert!(!error.contains(unused), "{setting} gave {error}");
    }

    Ok(())
}

#[test]
fn help_does_not_read_an_explicit_configuration() -> Result<()> {
    let project = project(&[("b3.toml", "not valid TOML")])?;
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
        let project = project(&[("b3.toml", setting)])?;
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
        let project = project(&[("b3.toml", contents)])?;
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

    ensure!(succeeded, "b3 failed with {stderr}");
    assert!(project.path().join("bench/second/report.txt").is_file());
    assert!(!project.path().join("bench/first").exists());
    assert!(!stdout.contains("first: Comparing"), "{stdout}");

    Ok(())
}

#[test]
fn a_benchmark_supplies_its_own_command() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', '--version']\n",
    )?;
    let (succeeded, stdout, stderr) = run(&project, &["--benchmark", "parse"])?;

    ensure!(succeeded, "b3 failed with {stderr}");
    assert!(stdout.contains("10 paired repetitions"), "{stdout}");

    Ok(())
}

#[test]
fn help_does_not_resolve_a_selected_benchmark() -> Result<()> {
    let project = project(&[(
        "b3.toml",
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
        "b3.toml",
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
fn an_unknown_benchmark_is_reported() -> Result<()> {
    let project = project(&[("b3.toml", "output-dir = 'bench'\n")])?;
    let error = failure(&project, &["--benchmark", "nope"])?;

    assert!(error.contains("has no benchmark named `nope`"), "{error}");

    Ok(())
}

#[test]
fn benchmark_definitions_are_not_exposed_as_options() -> Result<()> {
    let project = project(&[(
        "b3.toml",
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
        let project = project(&[("b3.toml", contents)])?;
        let error = failure(&project, arguments)?;

        assert!(error.contains(expected), "{contents} gave {error}");
    }

    Ok(())
}

#[test]
fn a_malformed_benchmarks_table_is_always_rejected() -> Result<()> {
    let project = project(&[("b3.toml", "output-dir = 'bench'\nbenchmarks = 1\n")])?;
    let error = failure(&project, &["--repetitions", "5"])?;

    assert!(
        error.contains("must set `benchmarks` to a table"),
        "{error}"
    );

    Ok(())
}

#[test]
fn a_benchmark_can_set_its_working_directory() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', 'rev-parse', '--show-prefix']\n\
        working-directory = 'sub'\n",
    )?;
    fs::create_dir(project.path().join("sub"))?;
    fs::write(project.path().join("sub").join(".gitkeep"), "")?;
    git(&project, &["add", "sub"])?;
    git(&project, &["commit", "--quiet", "--message", "add sub"])?;

    let (succeeded, _, stderr) = run(&project, &["--benchmark", "parse"])?;
    ensure!(succeeded, "b3 failed with {stderr}");

    let log = fs::read_to_string(
        project
            .path()
            .join("bench")
            .join("parse")
            .join("benchmark.log"),
    )?;
    assert!(log.contains("sub/"), "{log}");

    Ok(())
}

#[test]
fn a_benchmark_can_set_environment_variables() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', 'config', 'user.name']\n\
        \n\
        [benchmarks.parse.env]\n\
        GIT_CONFIG_COUNT = '1'\n\
        GIT_CONFIG_KEY_0 = 'user.name'\n\
        GIT_CONFIG_VALUE_0 = 'benchmark-env'\n",
    )?;

    let (succeeded, _, stderr) = run(&project, &["--benchmark", "parse"])?;
    ensure!(succeeded, "b3 failed with {stderr}");

    let log = fs::read_to_string(
        project
            .path()
            .join("bench")
            .join("parse")
            .join("benchmark.log"),
    )?;
    assert!(log.contains("benchmark-env"), "{log}");

    Ok(())
}

#[test]
fn a_benchmarks_env_merges_with_the_top_level_env() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        command = ['git', 'config', 'user.name']\n\
        \n\
        [env]\n\
        GIT_CONFIG_COUNT = '1'\n\
        GIT_CONFIG_KEY_0 = 'user.name'\n\
        GIT_CONFIG_VALUE_0 = 'top-level-env'\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', 'config', 'user.name']\n\
        \n\
        [benchmarks.parse.env]\n\
        GIT_CONFIG_VALUE_0 = 'benchmark-env'\n",
    )?;

    let (succeeded, _, stderr) = run(&project, &["--benchmark", "parse"])?;
    ensure!(succeeded, "b3 failed with {stderr}");

    let log = fs::read_to_string(
        project
            .path()
            .join("bench")
            .join("parse")
            .join("benchmark.log"),
    )?;
    assert!(log.contains("benchmark-env"), "{log}");
    assert!(!log.contains("top-level-env"), "{log}");

    Ok(())
}

#[test]
fn working_directory_and_env_are_ordinary_options() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        working-directory = 'sub'\n\
        command = ['git', 'config', 'user.name']\n\
        \n\
        [env]\n\
        GIT_CONFIG_COUNT = '1'\n\
        GIT_CONFIG_KEY_0 = 'user.name'\n\
        GIT_CONFIG_VALUE_0 = 'top-level-env'\n",
    )?;
    fs::create_dir(project.path().join("sub"))?;
    fs::write(project.path().join("sub").join(".gitkeep"), "")?;
    git(&project, &["add", "sub"])?;
    git(&project, &["commit", "--quiet", "--message", "add sub"])?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "b3 failed with {stderr}");

    let log = fs::read_to_string(project.path().join("bench").join("benchmark.log"))?;
    assert!(log.contains("top-level-env"), "{log}");

    Ok(())
}

#[test]
fn an_explicit_env_argument_overrides_the_configuration() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        command = ['git', 'config', 'user.name']\n\
        \n\
        [env]\n\
        GIT_CONFIG_COUNT = '1'\n\
        GIT_CONFIG_KEY_0 = 'user.name'\n\
        GIT_CONFIG_VALUE_0 = 'configured-env'\n",
    )?;

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
    ensure!(succeeded, "b3 failed with {stderr}");

    let log = fs::read_to_string(project.path().join("bench").join("benchmark.log"))?;
    assert!(log.contains("argument-env"), "{log}");
    assert!(!log.contains("configured-env"), "{log}");

    Ok(())
}

#[test]
fn setup_runs_in_each_worktree_before_the_measured_runs() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        setup = ['git', 'config', '--file', 'marker.txt', 'setup.ran', 'yes']\n\
        command = ['git', 'config', '--file', 'marker.txt', 'setup.ran']\n",
    )?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "b3 failed with {stderr}");

    let log = fs::read_to_string(project.path().join("bench").join("benchmark.log"))?;
    assert_eq!(log.matches("yes").count(), 20, "{log}");

    Ok(())
}

#[test]
fn a_failing_setup_stops_before_the_measured_runs() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        setup = ['git', 'cat-file', '-p', 'absent-object']\n\
        command = ['git', '--version']\n",
    )?;

    let error = failure(&project, &[])?;

    assert!(error.contains("The baseline setup failed."), "{error}");
    assert!(
        error.contains("Not a valid object name absent-object"),
        "{error}"
    );

    let bench = project.path().join("bench");
    assert!(bench.join("config.json").is_file());
    assert!(!bench.join("benchmark.log").exists());

    Ok(())
}

#[test]
fn teardown_runs_after_the_measured_runs() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        teardown = ['git', 'cat-file', '-p', 'absent-object']\n\
        command = ['git', '--version']\n",
    )?;

    let error = failure(&project, &[])?;

    assert!(error.contains("The baseline teardown failed."), "{error}");

    let bench = project.path().join("bench");
    let log = fs::read_to_string(bench.join("benchmark.log"))?;
    assert_eq!(log.lines().count(), 20, "{log}");

    let csv = fs::read_to_string(bench.join("measurements.csv"))?;
    assert_eq!(csv.lines().count(), 11, "{csv}");

    Ok(())
}

#[test]
fn a_failing_benchmark_leaves_the_measurements_recorded_so_far() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        command = ['git', 'cat-file', '-p', 'absent-object']\n",
    )?;

    let error = failure(&project, &[])?;

    assert!(error.contains("benchmark failed"), "{error}");
    assert_eq!(
        fs::read_to_string(project.path().join("bench").join("measurements.csv"))?,
        "repetition,order,baseline_seconds,candidate_seconds\n"
    );

    Ok(())
}

#[test]
fn teardown_still_runs_when_a_benchmark_fails() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        teardown = ['git', 'tag', '--force', 'teardown-ran']\n\
        command = ['git', 'cat-file', '-p', 'absent-object']\n",
    )?;

    let error = failure(&project, &[])?;

    assert!(error.contains("benchmark failed"), "{error}");
    git(&project, &["rev-parse", "--verify", "refs/tags/teardown-ran"])?;

    Ok(())
}

#[test]
fn a_benchmark_can_set_its_own_setup_and_teardown() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        setup = ['git', 'config', '--file', 'marker.txt', 'setup.ran', 'top-level-setup']\n\
        teardown = ['git', '--version']\n\
        \n\
        [benchmarks.parse]\n\
        setup = ['git', 'config', '--file', 'marker.txt', 'setup.ran', 'benchmark-setup']\n\
        teardown = ['git', 'cat-file', '-p', 'absent-object']\n\
        command = ['git', 'config', '--file', 'marker.txt', 'setup.ran']\n",
    )?;

    let error = failure(&project, &[])?;

    assert!(error.contains("The baseline teardown failed."), "{error}");

    let log = fs::read_to_string(
        project
            .path()
            .join("bench")
            .join("parse")
            .join("benchmark.log"),
    )?;
    assert_eq!(log.matches("benchmark-setup").count(), 20, "{log}");
    assert!(!log.contains("top-level-setup"), "{log}");

    Ok(())
}

#[test]
fn a_benchmark_clears_an_inherited_setup_and_teardown_with_an_empty_list() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        setup = ['git', 'cat-file', '-p', 'absent-object']\n\
        teardown = ['git', 'cat-file', '-p', 'absent-object']\n\
        \n\
        [benchmarks.standalone]\n\
        setup = []\n\
        teardown = []\n\
        command = ['git', '--version']\n",
    )?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "b3 failed with {stderr}");

    Ok(())
}

const SUITE: &str = "baseline = 'HEAD'\n\
    candidate = 'HEAD'\n\
    output-dir = 'bench'\n\
    repetitions = 10\n\
    draws = 1000\n\
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
    ensure!(succeeded, "b3 failed with {stderr}");

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
    ensure!(succeeded, "b3 failed with {stderr}");

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
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        \n\
        [benchmarks.parse]\n\
        command = ['git', '--version']\n",
    )?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "b3 failed with {stderr}");

    assert!(!stdout.contains("parse: Comparing candidate"), "{stdout}");

    let bench = project.path().join("bench");
    assert!(bench.join("parse").join("report.txt").is_file());
    assert!(!bench.join("report_short.txt").exists());

    Ok(())
}

#[test]
fn report_short_uses_the_top_level_output_directory() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        \n\
        [benchmarks.a]\n\
        command = ['git', '--version']\n\
        output-dir = 'special'\n\
        \n\
        [benchmarks.b]\n\
        command = ['git', '--version']\n",
    )?;
    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "b3 failed with {stderr}");

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
    ensure!(succeeded, "b3 failed with {stderr}");

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
        \n\
        [benchmarks.zebra]\n\
        command = ['git', '--version']\n\
        \n\
        [benchmarks.apple]\n\
        command = ['git', '--version']\n",
    )?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "b3 failed with {stderr}");

    assert!(
        stdout.find("zebra: Comparing") < stdout.find("apple: Comparing"),
        "{stdout}"
    );

    Ok(())
}

#[test]
fn every_benchmark_must_define_its_own_command() -> Result<()> {
    let project = project(&[(
        "b3.toml",
        "command = ['git', '--version']\n[benchmarks.parse]\nrepetitions = 10\n",
    )])?;
    let error = failure(&project, &[])?;

    assert!(
        error.contains("benchmark `parse` must set `command`"),
        "{error}"
    );

    Ok(())
}

#[test]
fn suite_settings_cannot_be_overridden_by_a_benchmark() -> Result<()> {
    for key in ["baseline", "candidate", "seed"] {
        let value = if key == "seed" { "1" } else { "'HEAD'" };
        let project = project(&[(
            "b3.toml",
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
    let project = project(&[("b3.toml", REQUIRED)])?;
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
        let project = project(&[("b3.toml", REQUIRED)])?;
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
    ensure!(succeeded, "b3 failed with {stderr}");

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

fn logged_worktree(project: &TempDir, benchmark: &str) -> Result<String> {
    let text = fs::read_to_string(
        project
            .path()
            .join("bench")
            .join(benchmark)
            .join("benchmark.log"),
    )?;
    let entry: serde_json::Value =
        serde_json::from_str(text.lines().next().expect("a benchmark log has entries"))?;
    Ok(entry["stdout"]
        .as_str()
        .expect("stdout is a string")
        .trim()
        .to_owned())
}

#[test]
fn worktrees_are_shared_unless_isolation_is_requested() -> Result<()> {
    const WORKTREES: &str = "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        [benchmarks.first]\n\
        command = ['git', 'rev-parse', '--show-toplevel']\n\
        [benchmarks.second]\n\
        command = ['git', 'rev-parse', '--show-toplevel']\n";

    let shared = repository(WORKTREES)?;
    let (succeeded, _, stderr) = run(&shared, &[])?;
    ensure!(succeeded, "b3 failed with {stderr}");
    assert_eq!(
        logged_worktree(&shared, "first")?,
        logged_worktree(&shared, "second")?
    );

    let isolated = repository(WORKTREES)?;
    let (succeeded, _, stderr) = run(&isolated, &["--isolate"])?;
    ensure!(succeeded, "b3 failed with {stderr}");
    assert_ne!(
        logged_worktree(&isolated, "first")?,
        logged_worktree(&isolated, "second")?
    );

    let selective = repository(
        "baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        output-dir = 'bench'\n\
        repetitions = 10\n\
        draws = 1000\n\
        [benchmarks.isolated]\n\
        isolate = true\n\
        command = ['git', 'rev-parse', '--show-toplevel']\n\
        [benchmarks.shared_one]\n\
        command = ['git', 'rev-parse', '--show-toplevel']\n\
        [benchmarks.shared_two]\n\
        command = ['git', 'rev-parse', '--show-toplevel']\n",
    )?;
    let (succeeded, _, stderr) = run(&selective, &[])?;
    ensure!(succeeded, "b3 failed with {stderr}");
    assert_ne!(
        logged_worktree(&selective, "isolated")?,
        logged_worktree(&selective, "shared_one")?
    );
    assert_eq!(
        logged_worktree(&selective, "shared_one")?,
        logged_worktree(&selective, "shared_two")?
    );

    Ok(())
}
