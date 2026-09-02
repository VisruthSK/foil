pub mod common;

use anyhow::{Context, Result, ensure};
use common::{failure, repository, run};
use std::{
    env, fs,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

const STARTED: &str = "FOIL_PROCESS_STARTED";
const FINISHED: &str = "FOIL_PROCESS_FINISHED";
const RELEASE: &str = "FOIL_PROCESS_RELEASE";
#[cfg(windows)]
const COPY_TARGET: &str = "FOIL_COPY_TARGET";

fn value(text: impl AsRef<str>) -> String {
    toml::Value::String(text.as_ref().to_owned()).to_string()
}

fn helper(test: &str) -> Vec<String> {
    vec![
        env::current_exe().unwrap().to_string_lossy().into_owned(),
        "--exact".to_owned(),
        test.to_owned(),
        "--ignored".to_owned(),
    ]
}

fn command(parts: &[String]) -> String {
    parts.iter().map(value).collect::<Vec<_>>().join(", ")
}

fn containment_config(test: &str, directory: &Path, timeout: Option<u64>) -> String {
    let command = command(&helper(test));
    let started = value(directory.join("started").to_string_lossy());
    let finished = value(directory.join("finished").to_string_lossy());
    let release = value(directory.join("release").to_string_lossy());
    let timeout = timeout.map_or(String::new(), |seconds| format!("timeout = {seconds}\n"));
    format!(
        "baseline = 'HEAD'\n\
         candidate = 'HEAD'\n\
         output-dir = 'bench'\n\
         repetitions = 10\n\
         draws = 1000\n\
         interval = [0.5]\n\
         {timeout}\
         env = {{ {STARTED} = {started}, {FINISHED} = {finished}, {RELEASE} = {release} }}\n\
         command = [{command}]\n"
    )
}

fn wait_for(path: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !path.exists() {
        ensure!(
            Instant::now() < deadline,
            "{} was not created",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn assert_descendant_stopped(directory: &Path) -> Result<()> {
    let started = directory.join("started");
    wait_for(&started)?;
    fs::write(directory.join("release"), "release")?;

    let finished = directory.join("finished");
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        ensure!(
            !finished.exists(),
            "the descendant survived containment cleanup"
        );
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn timeout_kills_the_containment_tree() -> Result<()> {
    let project = repository("")?;
    let config = containment_config("blocking_parent", project.path(), Some(1));
    fs::write(project.path().join("foil.toml"), config)?;

    let error = failure(&project, &[])?;
    assert!(error.contains("timed out"), "{error}");
    assert_descendant_stopped(project.path())
}

#[test]
fn descendants_are_cleaned_after_a_successful_direct_exit() -> Result<()> {
    let project = repository("")?;
    fs::write(
        project.path().join("foil.toml"),
        containment_config("orphan_parent", project.path(), None),
    )?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");
    assert_descendant_stopped(project.path())
}

#[test]
fn descendants_are_cleaned_after_a_nonzero_exit() -> Result<()> {
    let project = repository("")?;
    fs::write(
        project.path().join("foil.toml"),
        containment_config("failing_parent", project.path(), None),
    )?;

    let error = failure(&project, &[])?;
    assert!(error.contains("benchmark failed"), "{error}");
    assert_descendant_stopped(project.path())
}

#[cfg(windows)]
#[test]
fn a_benchmark_startup_can_create_its_executable() -> Result<()> {
    let startup = command(&helper("copy_self"));
    let benchmark = command(&[
        r".\late-tool".to_owned(),
        "--exact".to_owned(),
        "noop".to_owned(),
        "--ignored".to_owned(),
    ]);
    let project = repository(&format!(
        "baseline = 'HEAD'\n\
         candidate = 'HEAD'\n\
         output-dir = 'bench'\n\
         repetitions = 10\n\
         draws = 1000\n\
         env = {{ {COPY_TARGET} = 'late-tool.exe' }}\n\
         [benchmarks.created]\n\
         startup = [{startup}]\n\
         command = [{benchmark}]\n"
    ))?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_uses_the_child_path() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::copy(env::current_exe()?, directory.path().join("path-tool.exe"))?;
    let child_path = value(directory.path().to_string_lossy());
    let command = command(&[
        "path-tool".to_owned(),
        "--exact".to_owned(),
        "noop".to_owned(),
        "--ignored".to_owned(),
    ]);
    let project = repository(&format!(
        "baseline = 'HEAD'\n\
         candidate = 'HEAD'\n\
         output-dir = 'bench'\n\
         repetitions = 10\n\
         draws = 1000\n\
         env = {{ PATH = {child_path} }}\n\
         command = [{command}]\n"
    ))?;

    let (succeeded, _, stderr) = run(&project, &[])?;
    ensure!(succeeded, "foil failed with {stderr}");
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_rejects_batch_files() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("script.cmd"), "@exit /b 0\r\n")?;
    let child_path = value(directory.path().to_string_lossy());
    let project = repository(&format!(
        "baseline = 'HEAD'\n\
         candidate = 'HEAD'\n\
         output-dir = 'bench'\n\
         repetitions = 10\n\
         env = {{ PATH = {child_path} }}\n\
         command = ['script']\n"
    ))?;

    let error = failure(&project, &[])?;
    assert!(error.contains("batch file"), "{error}");
    Ok(())
}

#[test]
#[ignore]
fn blocking_parent() -> Result<()> {
    let mut child = descendant()?;
    wait_for(Path::new(
        &env::var_os(STARTED).context("missing started marker")?,
    ))?;
    child.wait()?;
    Ok(())
}

#[test]
#[ignore]
#[allow(clippy::zombie_processes)]
fn orphan_parent() -> Result<()> {
    let child = descendant()?;
    wait_for(Path::new(
        &env::var_os(STARTED).context("missing started marker")?,
    ))?;
    drop(child);
    Ok(())
}

#[test]
#[ignore]
#[allow(clippy::zombie_processes)]
fn failing_parent() -> Result<()> {
    let child = descendant()?;
    wait_for(Path::new(
        &env::var_os(STARTED).context("missing started marker")?,
    ))?;
    drop(child);
    std::process::exit(3)
}

#[test]
#[ignore]
fn descendant_process() -> Result<()> {
    fs::write(
        env::var_os(STARTED).context("missing started marker")?,
        "started",
    )?;
    wait_for(Path::new(
        &env::var_os(RELEASE).context("missing release marker")?,
    ))?;
    fs::write(
        env::var_os(FINISHED).context("missing finished marker")?,
        "finished",
    )?;
    Ok(())
}

fn descendant() -> Result<std::process::Child> {
    Ok(Command::new(env::current_exe()?)
        .args(["--exact", "descendant_process", "--ignored"])
        .env(
            STARTED,
            env::var_os(STARTED).context("missing started marker")?,
        )
        .env(
            FINISHED,
            env::var_os(FINISHED).context("missing finished marker")?,
        )
        .env(
            RELEASE,
            env::var_os(RELEASE).context("missing release marker")?,
        )
        .spawn()?)
}

#[cfg(windows)]
#[test]
#[ignore]
fn copy_self() -> Result<()> {
    fs::copy(
        env::current_exe()?,
        env::var_os(COPY_TARGET).context("missing copy target")?,
    )?;
    Ok(())
}

#[cfg(windows)]
#[test]
#[ignore]
fn noop() {}
