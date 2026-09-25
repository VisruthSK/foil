mod analysis;
mod app;
mod artifact;
mod config;
mod metric;
mod platform;
mod posterior;
mod repetition;
mod report;
mod run;
mod summary;
mod worktree;

pub use analysis::{Analysis, analyze_measurements};
pub use artifact::{
    Config, LifecycleConfig, MeasurementsCsv, write_config_json, write_posterior_csv,
};
pub use metric::{Metric, PeakMemory, Time, Unit};
pub use posterior::{Draw, Posterior, Shrinkage};
pub use repetition::{Pair, Repetition, Repetitions, RunOrder, Side};
pub(crate) use run::{BenchmarkLog, RunCommand};
pub use run::{Bytes, RunOutput};
pub use summary::{Change, ChangeBounds, Interval, Range, Summary};
pub(crate) use worktree::working_tree_has_modified_tracked_files;
pub use worktree::{Revision, Worktree};

/// Runs the command-line application.
pub fn run() -> anyhow::Result<()> {
    app::run()
}
