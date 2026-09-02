pub mod common;
use anyhow::{Context, Result, ensure};
use common::*;
use foil::{Interval, Metric, Shrinkage, analyze_measurements};
use std::{fs, num::NonZeroUsize};

#[test]
fn package_name_is_foil_bench() {
    assert_eq!(env!("CARGO_PKG_NAME"), "foil-bench");
}

#[test]
fn a_complete_configuration_runs_without_any_arguments() -> Result<()> {
    let project = repository(
        "\
        baseline = 'HEAD'\n\
        candidate = 'HEAD'\n\
        repetitions = 10\n\
        draws = 1000\n\
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
    assert_eq!(config["intervals"], serde_json::json!([0.5, 0.8, 0.9]));
    assert_eq!(
        config["suite_lifecycle"],
        serde_json::json!({
            "startup": [],
            "startup_each_run": [],
            "teardown_each_run": [],
            "teardown": []
        })
    );
    assert_eq!(
        config["worktree_lifecycle"],
        serde_json::json!({"startup": [], "teardown": []})
    );
    assert_eq!(
        config["benchmark_lifecycle"],
        serde_json::json!({
            "startup": [],
            "startup_each_run": [],
            "teardown_each_run": [],
            "teardown": []
        })
    );

    let report = fs::read_to_string(project.path().join("benchmark/report.txt"))?;
    for interval in ["50% CrI", "80% CrI", "90% CrI"] {
        assert!(
            report.contains(interval),
            "{interval} is missing from\n{report}"
        );
    }

    let log: Vec<serde_json::Value> =
        fs::read_to_string(project.path().join("benchmark/benchmark.log"))?
            .lines()
            .map(serde_json::from_str)
            .collect::<serde_json::Result<_>>()?;
    assert_eq!(log.len(), 20);
    for (index, entry) in log.iter().enumerate() {
        assert_eq!(entry["run"], index + 1);
        assert!(matches!(
            entry["side"].as_str(),
            Some("baseline" | "candidate")
        ));
        assert_eq!(entry["exit_code"], 0);
        assert_eq!(entry["peak_memory_bytes"], serde_json::Value::Null);
        assert_eq!(entry["timed_out"], false);
        assert_eq!(entry["interrupted"], false);
    }

    let measurements = fs::read_to_string(project.path().join("benchmark/measurements.csv"))?;
    let rows: Vec<_> = measurements.lines().collect();
    assert_eq!(
        rows[0],
        "repetition,order,baseline_seconds,candidate_seconds"
    );
    assert_eq!(rows.len(), 11);
    for (index, row) in rows[1..].iter().enumerate() {
        let fields: Vec<_> = row.split(',').collect();
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0], (index + 1).to_string());
        assert!(matches!(fields[1], "baseline_first" | "candidate_first"));
        fields[2].parse::<f64>()?;
        fields[3].parse::<f64>()?;
    }

    let posterior = fs::read_to_string(project.path().join("benchmark/posterior.csv"))?;
    let rows: Vec<_> = posterior.lines().collect();
    assert_eq!(rows[0], "baseline_seconds,candidate_seconds");
    assert_eq!(rows.len(), 1_001);
    for row in &rows[1..] {
        let (baseline, candidate) = row.split_once(',').context("invalid posterior row")?;
        baseline.parse::<f64>()?;
        candidate.parse::<f64>()?;
    }

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
        Interval::new(0.9)?,
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
        "[default: 0.5 0.8 0.9]",
    ] {
        assert!(help.contains(default), "{default} is missing from\n{help}");
    }
    assert!(help.contains(BUILTIN_USAGE), "{help}");
    for lifecycle in [
        "--suite-startup",
        "--suite-teardown",
        "--worktree-startup",
        "--worktree-teardown",
    ] {
        assert!(
            !help.contains(lifecycle),
            "{lifecycle} is present in\n{help}"
        );
    }
    for lifecycle in [
        "--startup <",
        "--startup-each-run",
        "--teardown-each-run",
        "--teardown <",
    ] {
        assert!(
            !help.contains(lifecycle),
            "{lifecycle} is present in\n{help}"
        );
    }

    Ok(())
}

#[test]
fn lifecycle_hooks_are_toml_only_and_preserve_command_arguments() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}suite-startup = []\n\
         suite-teardown = []\n\
         worktree-startup = ['git', '--version']\n\
         worktree-teardown = []\n\
         startup-each-run = []\n\
         teardown-each-run = []\n\
         [benchmarks.test]\n\
         startup = []\n\
         startup-each-run = []\n\
         teardown-each-run = []\n\
         teardown = []\n\
         command = ['git', '--version']\n"
    ))?;
    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        project.path().join("bench/test/config.json"),
    )?)?;
    assert_eq!(
        config["worktree_lifecycle"]["startup"],
        serde_json::json!(["git", "--version"])
    );
    assert_eq!(config["suite_lifecycle"]["startup"], serde_json::json!([]));
    assert_eq!(
        config["suite_lifecycle"]["startup_each_run"],
        serde_json::json!([])
    );
    assert_eq!(
        config["benchmark_lifecycle"]["startup_each_run"],
        serde_json::json!([])
    );

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
        "[default: 0.5 0.8 0.9]",
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

#[test]
fn adjacent_interval_values_are_one_cli_argument_group() -> Result<()> {
    let project = repository(&format!("{PREAMBLE}command = ['git', '--version']\n"))?;
    let (succeeded, _, stderr) = run(
        &project,
        &["--interval", "0.5", "0.8", "0.9", "--", "git", "--version"],
    )?;
    ensure!(succeeded, "foil failed with {stderr}");

    let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        project.path().join("bench/config.json"),
    )?)?;
    assert_eq!(config["intervals"], serde_json::json!([0.5, 0.8, 0.9]));
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
