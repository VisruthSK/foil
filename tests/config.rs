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
fn configuration_overrides_builtin_defaults() -> Result<()> {
    let project = project(&[("b3.toml", CONFIG)])?;
    let help = help(&project, &[])?;

    for default in [
        "[default: baseline-branch]",
        "[default: candidate-branch]",
        "[default: 2.5]",
        "[default: configured]",
        "[default: 12]",
        "[default: 2000]",
        "[default: 0.5 0.9]",
        "[default: 7]",
        "[default: Rscript benchmark.R]",
    ] {
        assert!(help.contains(default), "{default} is missing from\n{help}");
    }

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
fn an_explicit_configuration_replaces_the_default_file() -> Result<()> {
    let project = project(&[("b3.toml", CONFIG), ("other.toml", "draws = 2500\n")])?;
    let help = help(&project, &["--config", "other.toml"])?;

    assert!(help.contains("[default: 2500]"), "{help}");
    assert!(!help.contains("[default: baseline-branch]"), "{help}");
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
fn a_benchmark_overrides_the_configuration() -> Result<()> {
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

    assert!(help.contains("[default: 15]"), "{help}");
    assert!(help.contains("[default: git --version]"), "{help}");

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
            "has no benchmark named `parse`",
        ),
        (
            "[benchmarks]\nparse = 1",
            &["--benchmark", "parse"][..],
            "has no benchmark named `parse`",
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
fn an_unselected_benchmarks_table_is_not_validated() -> Result<()> {
    let project = project(&[("b3.toml", "output-dir = 'bench'\nbenchmarks = 1\n")])?;
    let error = failure(&project, &["--repetitions", "5"])?;

    assert!(
        error.contains("At least 10 repetitions are required."),
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

    let log = fs::read_to_string(project.path().join("bench").join("benchmark.log"))?;
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

    let log = fs::read_to_string(project.path().join("bench").join("benchmark.log"))?;
    assert!(log.contains("benchmark-env"), "{log}");

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
