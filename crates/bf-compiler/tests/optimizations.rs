//! Optimization regressions against source, CIR, and BF execution.
#[path = "optimizations/arena_advance.rs"]
mod arena_advance;
#[path = "optimizations/arithmetic_fusion.rs"]
mod arithmetic_fusion;
#[path = "optimizations/comparisons.rs"]
mod comparisons;
#[path = "optimizations/local_frames.rs"]
mod local_frames;
#[path = "optimizations/structured_control.rs"]
mod structured_control;

#[path = "optimizations/copy_lifetimes.rs"]
mod copy_lifetimes;
#[path = "optimizations/guarded_compare.rs"]
mod guarded_compare;
#[path = "optimizations/scaled_offsets.rs"]
mod scaled_offsets;
