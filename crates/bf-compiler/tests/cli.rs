//! CLI regressions grouped by observable behavior.
use bf_compiler::{
    SelfhostCirContinuation, SelfhostCirFunction, SelfhostCirInstruction, SelfhostCirProgram,
    SelfhostCirReturnType, SelfhostCirTerminator,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command};
use support::*;

#[path = "cli/backend_options.rs"]
mod backend_options;
#[path = "cli/closed_inline.rs"]
mod closed_inline;
#[path = "cli/direct_regions.rs"]
mod direct_regions;
#[path = "cli/metrics.rs"]
mod metrics;
#[path = "cli/multiple_sources.rs"]
mod multiple_sources;
#[path = "cli/output.rs"]
mod output;
#[path = "cli/support.rs"]
mod support;
#[path = "cli/validation.rs"]
mod validation;
