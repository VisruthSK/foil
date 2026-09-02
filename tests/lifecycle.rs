pub mod common;
use anyhow::{Result, ensure};
use common::*;
use std::{env, fs, fs::OpenOptions, io::Write, path::Path, process::Command};

fn lifecycle_command(path: &Path, phase: &str) -> String {
    [
        env::current_exe().unwrap().to_string_lossy().into_owned(),
        "--exact".to_owned(),
        "lifecycle_marker".to_owned(),
        "--ignored".to_owned(),
        "--".to_owned(),
        path.to_string_lossy().into_owned(),
        phase.to_owned(),
    ]
    .map(|part| toml::Value::String(part).to_string())
    .join(", ")
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
fn candidate_startup_does_not_run_when_baseline_startup_fails() -> Result<()> {
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
        &["update-ref", "refs/tags/startup-state", &candidate],
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
             startup = ['git', 'update-ref', 'refs/tags/startup-state', '{baseline}', 'HEAD']\n\
             command = ['git', '--version']\n"
        ),
    )?;

    let error = failure(&project, &[])?;
    assert!(error.contains("The baseline startup failed."), "{error}");

    let state = Command::new("git")
        .args(["rev-parse", "refs/tags/startup-state"])
        .current_dir(project.path())
        .output()?;
    assert!(state.status.success());
    assert_eq!(
        String::from_utf8(state.stdout)?.trim(),
        candidate,
        "candidate startup ran despite baseline startup failure"
    );

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
        "{PREAMBLE}suite-startup = ['git', '--version']\n\
         suite-teardown = ['git', '--version']\n\
         worktree-startup = ['git', '--version']\n\
         worktree-teardown = ['git', '--version']\n\
         startup-each-run = ['git', '--version']\n\
         teardown-each-run = ['git', '--version']\n\
         [benchmarks.test]\n\
         startup = ['git', '--version']\n\
         teardown = ['git', '--version']\n\
         command = ['git', 'rev-parse', '--is-inside-work-tree']\n"
    ))?;

    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");
    assert!(!stdout.contains("git version"), "{stdout}");
    assert!(!stderr.contains("git version"), "{stderr}");

    let config: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        project.path().join("bench/test/config.json"),
    )?)?;
    assert_eq!(config["suite_lifecycle"]["startup"][0], "git");
    assert_eq!(config["suite_lifecycle"]["startup_each_run"][0], "git");
    assert_eq!(config["suite_lifecycle"]["teardown_each_run"][0], "git");
    assert_eq!(config["suite_lifecycle"]["teardown"][0], "git");
    assert_eq!(config["worktree_lifecycle"]["startup"][0], "git");
    assert_eq!(config["worktree_lifecycle"]["teardown"][0], "git");
    assert_eq!(config["benchmark_lifecycle"]["startup"][0], "git");
    assert_eq!(config["benchmark_lifecycle"]["teardown"][0], "git");

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
    git(
        &project,
        &[
            "rev-parse",
            "--verify",
            "refs/tags/benchmark-each-run-torn-down",
        ],
    )?;
    git(
        &project,
        &[
            "rev-parse",
            "--verify",
            "refs/tags/suite-each-run-torn-down",
        ],
    )?;

    Ok(())
}

#[test]
fn removed_lifecycle_names_are_rejected() -> Result<()> {
    for key in [
        "setup",
        "prepare",
        "startup",
        "teardown",
    ] {
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
        "{PREAMBLE}suite-startup = ['git', '--version']\n\
        suite-teardown = ['git', '--version']\n\
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
        "{PREAMBLE}suite-startup = ['git', 'cat-file', '-p', 'absent-object']\n\
        suite-teardown = ['git', 'tag', 'suite-cleaned-up']\n\
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

#[test]
fn lifecycle_scopes_run_in_ownership_order() -> Result<()> {
    let project = repository("")?;
    let marker = project.path().join("lifecycle-order");
    let command = |phase| lifecycle_command(&marker, phase);
    fs::write(
        project.path().join("foil.toml"),
        format!(
            "{PREAMBLE}\
             suite-startup = [{}]\n\
             suite-teardown = [{}]\n\
             worktree-startup = [{}]\n\
             worktree-teardown = [{}]\n\
             [benchmarks.test]\n\
             startup = [{}]\n\
             teardown = [{}]\n\
             command = [{}]\n",
            command("suite-startup"),
            command("suite-teardown"),
            command("worktree-startup"),
            command("worktree-teardown"),
            command("benchmark-startup"),
            command("benchmark-teardown"),
            command("benchmark-work"),
        ),
    )?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    let mut expected = vec!["suite-startup"];
    expected.extend(["worktree-startup"; 2]);
    expected.extend(["benchmark-startup"; 2]);
    expected.extend(["benchmark-work"; 20]);
    expected.extend(["benchmark-teardown"; 2]);
    expected.extend(["worktree-teardown"; 2]);
    expected.push("suite-teardown");
    assert_eq!(
        fs::read_to_string(marker)?.lines().collect::<Vec<_>>(),
        expected
    );
    Ok(())
}

#[test]
fn worktree_hooks_follow_shared_and_isolated_ownership() -> Result<()> {
    for (isolated, expected) in [(false, 2), (true, 4)] {
        let project = repository("")?;
        let marker = project.path().join("worktree-hooks");
        let startup = lifecycle_command(&marker, "startup");
        let teardown = lifecycle_command(&marker, "teardown");
        fs::write(
            project.path().join("foil.toml"),
            format!(
                "{PREAMBLE}\
                 worktree-startup = [{startup}]\n\
                 worktree-teardown = [{teardown}]\n\
                 [benchmarks.first]\n\
                 isolate = {isolated}\n\
                 command = ['git', '--version']\n\
                 [benchmarks.second]\n\
                 isolate = {isolated}\n\
                 command = ['git', '--version']\n"
            ),
        )?;

        let (succeeded, _, stderr) = run(&project, &[])?;
        ensure!(succeeded, "foil failed with {stderr}");
        let phases = fs::read_to_string(marker)?;
        assert_eq!(
            phases.lines().filter(|line| *line == "startup").count(),
            expected
        );
        assert_eq!(
            phases.lines().filter(|line| *line == "teardown").count(),
            expected
        );
    }
    Ok(())
}

#[test]
fn worktree_teardown_runs_after_benchmark_failure() -> Result<()> {
    let project = repository("")?;
    let marker = project.path().join("worktree-teardown");
    let teardown = lifecycle_command(&marker, "teardown");
    fs::write(
        project.path().join("foil.toml"),
        format!(
            "{PREAMBLE}\
             worktree-teardown = [{teardown}]\n\
             command = ['git', 'cat-file', '-p', 'absent-object']\n"
        ),
    )?;

    let error = failure(&project, &[])?;
    assert!(error.contains("benchmark failed"), "{error}");
    assert_eq!(fs::read_to_string(marker)?.lines().count(), 2);
    Ok(())
}

#[test]
#[ignore]
fn lifecycle_marker() -> Result<()> {
    let arguments = env::args_os().collect::<Vec<_>>();
    let [.., path, phase] = arguments.as_slice() else {
        anyhow::bail!("missing lifecycle marker arguments");
    };
    writeln!(
        OpenOptions::new().create(true).append(true).open(path)?,
        "{}",
        phase.to_string_lossy()
    )?;
    Ok(())
}
