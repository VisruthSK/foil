pub mod common;
use anyhow::{Result, ensure};
use common::*;

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

    Ok(())
}

#[test]
fn benchmark_reports_are_separated_by_one_blank_line() -> Result<()> {
    let project = repository(
        "baseline = 'HEAD'\n\
         candidate = 'HEAD'\n\
         output-dir = 'bench'\n\
         repetitions = 10\n\
         draws = 1000\n\
         seed = 0\n\
         [benchmarks.first]\n\
         command = ['git', '--version']\n\
         [benchmarks.second]\n\
         command = ['git', '--version']\n",
    )?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    let second = stdout
        .find("second: Comparing candidate")
        .expect("second report is missing");
    let before_second = &stdout[..second];
    assert!(before_second.ends_with("\n\n"), "{stdout}");
    assert!(!before_second.ends_with("\n\n\n"), "{stdout}");

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

    Ok(())
}

#[test]
fn a_lone_benchmark_prints_full_report() -> Result<()> {
    let project = repository(&format!(
        "{PREAMBLE}\n\
        [benchmarks.parse]\n\
        command = ['git', '--version']\n"
    ))?;
    let (succeeded, stdout, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");

    assert!(stdout.contains("parse: Comparing candidate"), "{stdout}");

    let bench = project.path().join("bench");
    assert!(bench.join("parse").join("report.txt").is_file());

    Ok(())
}

#[test]
fn report_uses_the_top_level_output_directory() -> Result<()> {
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
            .join("b")
            .join("report.txt")
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
fn an_output_dir_argument_relocates_the_report() -> Result<()> {
    let project = repository(SUITE)?;
    let (succeeded, _, stderr) = run(&project, &["--output-dir", "elsewhere"])?;
    ensure!(succeeded, "foil failed with {stderr}");

    assert!(!project.path().join("bench").exists());

    let elsewhere = project.path().join("elsewhere");
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
