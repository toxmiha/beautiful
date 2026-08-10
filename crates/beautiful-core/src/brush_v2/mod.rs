//! Brush Engine v2 — Phase 1 CPU stamp (RFC-BrushEngine-v2).
//!
//! Modules: BrushDef · TipMask · DabPlanner · StampKernel.
//! Legacy `engine.rs` remains until cutover (`BrushBackend::Legacy`).

mod dab_planner;
mod def;
mod graph;
mod stamp;
mod tip_mask;

pub use dab_planner::{Dab, DabPlannerState};
pub use def::BrushDef;
pub use graph::{
    BrushGraphNode, BrushGraphNodeData, BrushGraphWire, BrushNodeGraph, BrushOutField,
    CompileError,
};
pub use tip_mask::TipMask;
