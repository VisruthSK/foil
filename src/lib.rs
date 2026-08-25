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
mod seed;
mod summary;
mod worktree;

pub use analysis::{Analysis, analyze_measurements};
pub(crate) use artifact::{
    Config, LifecycleConfig, MeasurementsCsv, write_config_json, write_posterior_csv,
};
pub use metric::{Metric, PeakMemory, Time, Unit};
pub use posterior::{Draw, Posterior, Shrinkage};
pub(crate) use repetition::{Pair, Repetition, Repetitions, RunOrder, Side};
pub use run::Bytes;
pub(crate) use run::{BenchmarkLog, RunCommand, RunOutput};
pub use summary::{Change, ChangeBounds, Interval, Range, Summary};
pub(crate) use worktree::{Revision, Worktree, working_tree_has_modified_tracked_files};

/// Runs the command-line application.
#[doc(hidden)]
pub fn run() -> anyhow::Result<()> {
    app::run()
}
