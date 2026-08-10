//! Brush node graph IR — visual DAG that compiles into [`BrushSettings`] / BrushDef.
//!
//! Authoring model (program rule): nodes + wires compile into the sheet.
//! Stamp still reads BrushDef — no per-dab runtime eval in v1.

use serde::{Deserialize, Serialize};

use crate::BrushSettings;

/// Sink field on a BrushOut node (maps to sheet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BrushOutField {
    Size,
    Opacity,
    Flow,
    Hardness,
    Spacing,
    Scatter,
    Jitter,
    Fuzzy,
    ColorJitter,
    WetRate,
    DualSize,
    TaperIn,
    TaperOut,
}

impl BrushOutField {
    pub const ALL: &'static [BrushOutField] = &[
        BrushOutField::Size,
        BrushOutField::Opacity,
        BrushOutField::Flow,
        BrushOutField::Hardness,
        BrushOutField::Spacing,
        BrushOutField::Scatter,
        BrushOutField::Jitter,
        BrushOutField::Fuzzy,
        BrushOutField::ColorJitter,
        BrushOutField::WetRate,
        BrushOutField::DualSize,
        BrushOutField::TaperIn,
        BrushOutField::TaperOut,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BrushOutField::Size => "Size",
            BrushOutField::Opacity => "Opacity",
            BrushOutField::Flow => "Flow",
            BrushOutField::Hardness => "Hardness",
            BrushOutField::Spacing => "Spacing",
            BrushOutField::Scatter => "Scatter",
            BrushOutField::Jitter => "Jitter",
            BrushOutField::Fuzzy => "Fuzzy",
            BrushOutField::ColorJitter => "Color jitter",
            BrushOutField::WetRate => "Wet rate",
            BrushOutField::DualSize => "Dual size",
            BrushOutField::TaperIn => "Taper in",
            BrushOutField::TaperOut => "Taper out",
        }
    }

    pub fn range(self) -> (f32, f32) {
        match self {
            BrushOutField::Size => (crate::BRUSH_SIZE_MIN, crate::BRUSH_SIZE_MAX),
            BrushOutField::DualSize => (0.1, 2.0),
            BrushOutField::Spacing => (0.025, 1.0),
            _ => (0.0, 1.0),
        }
    }
}

/// Node kinds in the authoring DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BrushGraphNodeData {
    Const { value: f32 },
    Pressure,
    Speed,
    Mul,
    Remap { lo: f32, hi: f32 },
    BrushOut { field: BrushOutField },
}

impl BrushGraphNodeData {
    pub fn label(&self) -> String {
        match self {
            BrushGraphNodeData::Const { .. } => "Const".into(),
            BrushGraphNodeData::Pressure => "Pressure".into(),
            BrushGraphNodeData::Speed => "Speed".into(),
            BrushGraphNodeData::Mul => "Multiply".into(),
            BrushGraphNodeData::Remap { .. } => "Remap".into(),
            BrushGraphNodeData::BrushOut { field } => format!("Out · {}", field.label()),
        }
    }

    pub fn inputs(&self) -> usize {
        match self {
            BrushGraphNodeData::Const { .. }
            | BrushGraphNodeData::Pressure
            | BrushGraphNodeData::Speed => 0,
            BrushGraphNodeData::Remap { .. } | BrushGraphNodeData::BrushOut { .. } => 1,
            BrushGraphNodeData::Mul => 2,
        }
    }

    pub fn outputs(&self) -> usize {
        match self {
            BrushGraphNodeData::BrushOut { .. } => 0,
            _ => 1,
        }
    }
}

/// Stable node id for serializable IR (independent of egui-snarl NodeId).
pub type BrushGraphNodeId = u32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrushGraphNode {
    pub id: BrushGraphNodeId,
    pub data: BrushGraphNodeData,
    /// Canvas position (for round-trip with visual editor).
    pub pos: [f32; 2],
}

/// Wire: output pin → input pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrushGraphWire {
    pub from_node: BrushGraphNodeId,
    pub from_output: usize,
    pub to_node: BrushGraphNodeId,
    pub to_input: usize,
}

/// Authoring graph that compiles into the live brush sheet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrushNodeGraph {
    pub nodes: Vec<BrushGraphNode>,
    pub wires: Vec<BrushGraphWire>,
    next_id: BrushGraphNodeId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileError {
    Cycle,
    MissingInput { node: BrushGraphNodeId, pin: usize },
    UnknownNode(BrushGraphNodeId),
    Msg(String),
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::Cycle => write!(f, "Graph has a cycle"),
            CompileError::MissingInput { node, pin } => {
                write!(f, "Node {node} input {pin} is not connected")
            }
            CompileError::UnknownNode(id) => write!(f, "Unknown node {id}"),
            CompileError::Msg(s) => write!(f, "{s}"),
        }
    }
}

/// Evaluated expression shape for one BrushOut sink.
#[derive(Debug, Clone, PartialEq)]
enum Expr {
    /// Constant value (no sensor).
    Const(f32),
    /// `lo + pressure * (hi - lo)` (linear pressure response).
    PressureRemap { lo: f32, hi: f32 },
    /// `lo + speed * (hi - lo)` — compile enables speed_* (fast→toward lo in stamp).
    SpeedRemap { lo: f32, hi: f32 },
}

impl BrushNodeGraph {
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.wires.clear();
        self.next_id = 0;
    }

    pub fn alloc_id(&mut self) -> BrushGraphNodeId {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    pub fn add_node(&mut self, data: BrushGraphNodeData, pos: [f32; 2]) -> BrushGraphNodeId {
        let id = self.alloc_id();
        self.nodes.push(BrushGraphNode { id, data, pos });
        id
    }

    pub fn remove_node(&mut self, id: BrushGraphNodeId) {
        self.nodes.retain(|n| n.id != id);
        self.wires
            .retain(|w| w.from_node != id && w.to_node != id);
    }

    pub fn node(&self, id: BrushGraphNodeId) -> Option<&BrushGraphNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    pub fn node_mut(&mut self, id: BrushGraphNodeId) -> Option<&mut BrushGraphNode> {
        self.nodes.iter_mut().find(|n| n.id == id)
    }

    /// Replace wire into `to_node`/`to_input` (one wire per input).
    pub fn connect(
        &mut self,
        from_node: BrushGraphNodeId,
        from_output: usize,
        to_node: BrushGraphNodeId,
        to_input: usize,
    ) {
        self.wires
            .retain(|w| !(w.to_node == to_node && w.to_input == to_input));
        self.wires.push(BrushGraphWire {
            from_node,
            from_output,
            to_node,
            to_input,
        });
    }

    /// Seed Const → BrushOut wires from current sheet (starter graph).
    pub fn sync_from_brush(&mut self, brush: &BrushSettings) {
        self.clear();
        let fields: &[(BrushOutField, f32)] = &[
            (BrushOutField::Size, brush.size),
            (BrushOutField::Opacity, brush.density),
            (BrushOutField::Flow, brush.flow),
            (BrushOutField::Hardness, brush.hardness),
            (BrushOutField::Spacing, brush.spacing),
            (BrushOutField::Scatter, brush.scatter),
            (BrushOutField::Jitter, brush.jitter),
            (BrushOutField::Fuzzy, brush.fuzzy),
            (BrushOutField::ColorJitter, brush.color_jitter),
            (BrushOutField::WetRate, brush.wet_rate),
            (BrushOutField::DualSize, brush.dual_size_pct),
            (BrushOutField::TaperIn, brush.taper_in),
            (BrushOutField::TaperOut, brush.taper_out),
        ];
        let mut y = 0.0_f32;
        for (i, &(field, value)) in fields.iter().enumerate() {
            let row = (i % 7) as f32;
            let col = (i / 7) as f32;
            let x_c = col * 420.0;
            let y_c = row * 90.0;
            let _ = y;
            y = y_c;
            let cid = self.add_node(
                BrushGraphNodeData::Const { value },
                [x_c, y_c],
            );
            let oid = self.add_node(
                BrushGraphNodeData::BrushOut { field },
                [x_c + 200.0, y_c],
            );
            self.connect(cid, 0, oid, 0);
        }
    }

    /// Compile reachable BrushOut sinks into `brush`.
    pub fn compile_to_brush(&self, brush: &mut BrushSettings) -> Result<(), CompileError> {
        for n in &self.nodes {
            if let BrushGraphNodeData::BrushOut { field } = n.data {
                let expr = self.eval_input(n.id, 0, &mut Vec::new())?;
                apply_expr(brush, field, &expr);
            }
        }
        Ok(())
    }

    /// Alias used by older UI call sites.
    pub fn apply_to(&self, brush: &mut BrushSettings) -> Result<(), CompileError> {
        self.compile_to_brush(brush)
    }

    fn wire_into(&self, node: BrushGraphNodeId, input: usize) -> Option<&BrushGraphWire> {
        self.wires
            .iter()
            .find(|w| w.to_node == node && w.to_input == input)
    }

    fn eval_input(
        &self,
        node: BrushGraphNodeId,
        input: usize,
        stack: &mut Vec<BrushGraphNodeId>,
    ) -> Result<Expr, CompileError> {
        let Some(w) = self.wire_into(node, input) else {
            return Err(CompileError::MissingInput { node, pin: input });
        };
        self.eval_output(w.from_node, w.from_output, stack)
    }

    fn eval_output(
        &self,
        node: BrushGraphNodeId,
        _output: usize,
        stack: &mut Vec<BrushGraphNodeId>,
    ) -> Result<Expr, CompileError> {
        if stack.contains(&node) {
            return Err(CompileError::Cycle);
        }
        let n = self.node(node).ok_or(CompileError::UnknownNode(node))?;
        stack.push(node);
        let out = match &n.data {
            BrushGraphNodeData::Const { value } => Ok(Expr::Const(*value)),
            BrushGraphNodeData::Pressure => Ok(Expr::PressureRemap { lo: 0.0, hi: 1.0 }),
            BrushGraphNodeData::Speed => Ok(Expr::SpeedRemap { lo: 0.0, hi: 1.0 }),
            BrushGraphNodeData::Remap { lo, hi } => {
                let inner = self.eval_input(node, 0, stack)?;
                Ok(compose_remap(inner, *lo, *hi))
            }
            BrushGraphNodeData::Mul => {
                let a = self.eval_input(node, 0, stack)?;
                let b = self.eval_input(node, 1, stack)?;
                Ok(compose_mul(a, b)?)
            }
            BrushGraphNodeData::BrushOut { .. } => Err(CompileError::Msg(
                "BrushOut has no output pin".into(),
            )),
        };
        stack.pop();
        out
    }
}

fn compose_remap(inner: Expr, lo: f32, hi: f32) -> Expr {
    match inner {
        Expr::Const(v) => {
            let t = v.clamp(0.0, 1.0);
            Expr::Const(lo + t * (hi - lo))
        }
        Expr::PressureRemap { .. } => Expr::PressureRemap { lo, hi },
        Expr::SpeedRemap { .. } => Expr::SpeedRemap { lo, hi },
    }
}

fn compose_mul(a: Expr, b: Expr) -> Result<Expr, CompileError> {
    match (a, b) {
        (Expr::Const(x), Expr::Const(y)) => Ok(Expr::Const(x * y)),
        (Expr::Const(k), Expr::PressureRemap { lo, hi })
        | (Expr::PressureRemap { lo, hi }, Expr::Const(k)) => Ok(Expr::PressureRemap {
            lo: lo * k,
            hi: hi * k,
        }),
        (Expr::Const(k), Expr::SpeedRemap { lo, hi })
        | (Expr::SpeedRemap { lo, hi }, Expr::Const(k)) => Ok(Expr::SpeedRemap {
            lo: lo * k,
            hi: hi * k,
        }),
        _ => Err(CompileError::Msg(
            "Unsupported multiply of sensor expressions in v1".into(),
        )),
    }
}

fn apply_expr(brush: &mut BrushSettings, field: BrushOutField, expr: &Expr) {
    let (rlo, rhi) = field.range();
    match (field, expr) {
        (BrushOutField::Size, Expr::Const(v)) => {
            brush.size = v.clamp(rlo, rhi);
            brush.pressure_size = false;
            brush.speed_size = false;
        }
        (BrushOutField::Size, Expr::PressureRemap { lo, hi }) => {
            let hi = hi.clamp(rlo, rhi);
            let lo = lo.clamp(rlo, hi);
            brush.size = hi;
            brush.min_size_pct = if hi > 1e-4 { (lo / hi).clamp(0.0, 1.0) } else { 0.0 };
            brush.pressure_size = true;
            brush.speed_size = false;
        }
        (BrushOutField::Size, Expr::SpeedRemap { lo, hi }) => {
            let hi = hi.clamp(rlo, rhi);
            let lo = lo.clamp(rlo, hi);
            brush.size = hi;
            brush.min_size_pct = if hi > 1e-4 { (lo / hi).clamp(0.0, 1.0) } else { 0.0 };
            brush.speed_size = true;
            brush.pressure_size = false;
        }
        (BrushOutField::Opacity, Expr::Const(v)) => {
            brush.density = v.clamp(rlo, rhi);
            brush.pressure_density = false;
            brush.speed_opacity = false;
        }
        (BrushOutField::Opacity, Expr::PressureRemap { lo, hi }) => {
            let hi = hi.clamp(rlo, rhi);
            let lo = lo.clamp(rlo, hi);
            brush.density = hi;
            brush.min_density = lo;
            brush.pressure_density = true;
            brush.speed_opacity = false;
        }
        (BrushOutField::Opacity, Expr::SpeedRemap { lo, hi }) => {
            let hi = hi.clamp(rlo, rhi);
            let lo = lo.clamp(rlo, hi);
            brush.density = hi;
            brush.min_density = lo;
            brush.speed_opacity = true;
            brush.pressure_density = false;
        }
        (BrushOutField::Flow, Expr::Const(v)) => {
            brush.flow = v.clamp(rlo, rhi);
            brush.pressure_flow = false;
            brush.speed_flow = false;
        }
        (BrushOutField::Flow, Expr::PressureRemap { lo, hi }) => {
            brush.flow = hi.clamp(rlo, rhi);
            brush.min_flow = lo.clamp(rlo, brush.flow);
            brush.pressure_flow = true;
            brush.speed_flow = false;
        }
        (BrushOutField::Flow, Expr::SpeedRemap { lo, hi }) => {
            brush.flow = hi.clamp(rlo, rhi);
            brush.min_flow = lo.clamp(rlo, brush.flow);
            brush.speed_flow = true;
            brush.pressure_flow = false;
        }
        (BrushOutField::Hardness, Expr::Const(v)) => brush.hardness = v.clamp(rlo, rhi),
        (
            BrushOutField::Hardness,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.hardness = hi.clamp(rlo, rhi);
        }
        (BrushOutField::Spacing, Expr::Const(v)) => brush.spacing = v.clamp(rlo, rhi),
        (
            BrushOutField::Spacing,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.spacing = hi.clamp(rlo, rhi);
        }
        (BrushOutField::Scatter, Expr::Const(v)) => brush.scatter = v.clamp(rlo, rhi),
        (
            BrushOutField::Scatter,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.scatter = hi.clamp(rlo, rhi);
        }
        (BrushOutField::Jitter, Expr::Const(v)) => brush.jitter = v.clamp(rlo, rhi),
        (
            BrushOutField::Jitter,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.jitter = hi.clamp(rlo, rhi);
        }
        (BrushOutField::Fuzzy, Expr::Const(v)) => brush.fuzzy = v.clamp(rlo, rhi),
        (
            BrushOutField::Fuzzy,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.fuzzy = hi.clamp(rlo, rhi);
        }
        (BrushOutField::ColorJitter, Expr::Const(v)) => brush.color_jitter = v.clamp(rlo, rhi),
        (
            BrushOutField::ColorJitter,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.color_jitter = hi.clamp(rlo, rhi);
        }
        (BrushOutField::WetRate, Expr::Const(v)) => brush.wet_rate = v.clamp(rlo, rhi),
        (
            BrushOutField::WetRate,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.wet_rate = hi.clamp(rlo, rhi);
        }
        (BrushOutField::DualSize, Expr::Const(v)) => {
            brush.dual_size_pct = v.clamp(rlo, rhi);
            brush.dual_enabled = true;
        }
        (
            BrushOutField::DualSize,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.dual_size_pct = hi.clamp(rlo, rhi);
            brush.dual_enabled = true;
        }
        (BrushOutField::TaperIn, Expr::Const(v)) => brush.taper_in = v.clamp(rlo, rhi),
        (
            BrushOutField::TaperIn,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.taper_in = hi.clamp(rlo, rhi);
        }
        (BrushOutField::TaperOut, Expr::Const(v)) => brush.taper_out = v.clamp(rlo, rhi),
        (
            BrushOutField::TaperOut,
            Expr::PressureRemap { hi, .. } | Expr::SpeedRemap { hi, .. },
        ) => {
            brush.taper_out = hi.clamp(rlo, rhi);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_to_size() {
        let mut g = BrushNodeGraph::default();
        let c = g.add_node(BrushGraphNodeData::Const { value: 128.0 }, [0.0, 0.0]);
        let o = g.add_node(
            BrushGraphNodeData::BrushOut {
                field: BrushOutField::Size,
            },
            [200.0, 0.0],
        );
        g.connect(c, 0, o, 0);
        let mut brush = BrushSettings::preset_pen();
        g.compile_to_brush(&mut brush).unwrap();
        assert!((brush.size - 128.0).abs() < 1e-3);
        assert!(!brush.pressure_size);
    }

    #[test]
    fn pressure_remap_opacity() {
        let mut g = BrushNodeGraph::default();
        let p = g.add_node(BrushGraphNodeData::Pressure, [0.0, 0.0]);
        let r = g.add_node(
            BrushGraphNodeData::Remap { lo: 0.2, hi: 0.9 },
            [150.0, 0.0],
        );
        let o = g.add_node(
            BrushGraphNodeData::BrushOut {
                field: BrushOutField::Opacity,
            },
            [350.0, 0.0],
        );
        g.connect(p, 0, r, 0);
        g.connect(r, 0, o, 0);
        let mut brush = BrushSettings::preset_pen();
        g.compile_to_brush(&mut brush).unwrap();
        assert!(brush.pressure_density);
        assert!((brush.density - 0.9).abs() < 1e-3);
        assert!((brush.min_density - 0.2).abs() < 1e-3);
    }

    #[test]
    fn cycle_detected() {
        let mut g = BrushNodeGraph::default();
        let a = g.add_node(BrushGraphNodeData::Mul, [0.0, 0.0]);
        let b = g.add_node(BrushGraphNodeData::Mul, [100.0, 0.0]);
        g.connect(a, 0, b, 0);
        g.connect(b, 0, a, 0);
        let o = g.add_node(
            BrushGraphNodeData::BrushOut {
                field: BrushOutField::Flow,
            },
            [300.0, 0.0],
        );
        g.connect(a, 0, o, 0);
        let mut brush = BrushSettings::preset_pen();
        assert!(matches!(
            g.compile_to_brush(&mut brush),
            Err(CompileError::Cycle)
        ));
    }
}
