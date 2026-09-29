//! Backend regression tests, shared support, and explicit measurement entry points.
use super::*;
use crate::backend::layout_plan::{build_layouts, estimated_frame_chunks};
use crate::backend::regions::maximum_branch_depth;
use crate::{CellId, Instruction};
use bf_interpreter::{run, run_with_stats};
use support::*;

mod calls;
mod dispatch;
mod inlining;
mod instructions;
mod measurements;
mod portal_transport;
mod portals;
mod regions;
mod support;
