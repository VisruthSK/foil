pub mod common;
use anyhow::{Result, ensure};
use common::*;
use std::{fs, process::Command};

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
