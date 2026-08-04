use anyhow::Result;
use b3::{Config, Revision, Shrinkage, write_config_json};
use serde_json::json;
use std::{ffi::OsString, fs::read_to_string};
use tempfile::tempdir;

#[test]
fn config_json_contains_reproduction_metadata() -> Result<()> {
    let directory = tempdir()?;
    let path = directory.path().join("config.json");
    let command = ["cargo", "test --workspace"].map(OsString::from);
    let baseline = Revision::resolve("main".to_owned())?;
    let candidate = Revision::resolve("HEAD".to_owned())?;
    let config = Config {
        seed: 42,
        repetitions: 10,
        draws: 1000,
        shrinkage: Shrinkage::new(5.0)?,
        baseline: &baseline,
        candidate: &candidate,
        command: &command,
    };

    write_config_json(&path, &config)?;

    let actual: serde_json::Value = serde_json::from_str(&read_to_string(path)?)?;
    let expected = json!({
        "seed": 42,
        "repetitions": 10,
        "draws": 1000,
        "shrinkage": 5.0,
        "b3_version": env!("CARGO_PKG_VERSION"),
        "baseline": { "revision": "main", "hash": baseline.hash() },
        "candidate": { "revision": "HEAD", "hash": candidate.hash() },
        "command": ["cargo", "test --workspace"],
    });

    assert_eq!(actual, expected);

    Ok(())
}
