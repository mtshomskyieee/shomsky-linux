//! Shared learn registry, install paths, BATS gate, and apk publish.

pub mod apk;
pub mod bats;
pub mod install;
pub mod name;
pub mod paths;
pub mod registry;

pub use apk::publish_apk;
pub use bats::run_bats;
pub use install::{
    install_artifacts, refuse_overwrite, sanitize_bats, write_workdir, PromoteArtifacts,
};
pub use name::{suggest_slug, validate_name};
pub use paths::LearnPaths;
pub use registry::{list_entries, load_entry, remove_entry, save_entry, LearnEntry};
