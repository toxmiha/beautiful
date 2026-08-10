//! Visual brush node editor (egui-snarl).
//!
//! Graph authors a brush; Apply compiles into BrushSettings / BrushDef.

use std::collections::HashMap;

use beautiful_core::{
    BrushGraphNodeData, BrushNodeGraph, BrushOutField, BrushSettings, CompileError,
};
use eframe::egui::{self, Color32, Id, Ui};
use egui_snarl::{
    InPin, InPinId, NodeId, OutPin, OutPinId, Snarl,
    ui::{PinInfo, SnarlStyle, SnarlViewer, SnarlWidget},
};

const FLOAT_PIN: Color32 = Color32::from_rgb(0xe0, 0x90, 0x40);
const SENSOR_PIN: Color32 = Color32::from_rgb(0x60, 0xb0, 0xe0);
const OUT_PIN: Color32 = Color32::from_rgb(0xc0, 0x70, 0xc0);

/// Session state for the visual brush node editor.
pub struct BrushNodeEditorState {
    pub snarl: Snarl<BrushGraphNodeData>,
    pub style: SnarlStyle,
    pub seeded: bool,
    pub last_error: Option<String>,
}

impl Default for BrushNodeEditorState {
    fn default() -> Self {
        let mut style = SnarlStyle::new();
        style.collapsible = Some(false);
        style.centering = Some(false);
        Self {
            snarl: Snarl::new(),
            style,
            seeded: false,
            last_error: None,
        }
    }
}

impl BrushNodeEditorState {
    pub fn ensure_seeded(&mut self, brush: &BrushSettings) {
        if self.seeded {
            return;
        }
        let mut ir = BrushNodeGraph::default();
        ir.sync_from_brush(brush);
        load_ir_into_snarl(&ir, &mut self.snarl);
        self.seeded = true;
        self.last_error = None;
    }

    pub fn sync_from_brush(&mut self, brush: &BrushSettings) {
        let mut ir = BrushNodeGraph::default();
        ir.sync_from_brush(brush);
        load_ir_into_snarl(&ir, &mut self.snarl);
        self.seeded = true;
        self.last_error = None;
    }

    pub fn apply_to_brush(&mut self, brush: &mut BrushSettings) -> Result<(), CompileError> {
        let ir = snarl_to_ir(&self.snarl);
        match ir.compile_to_brush(brush) {
            Ok(()) => {
                self.last_error = None;
                Ok(())
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }
}

fn snarl_to_ir(snarl: &Snarl<BrushGraphNodeData>) -> BrushNodeGraph {
    let mut g = BrushNodeGraph::default();
    let mut map = HashMap::new();
    for (nid, node) in snarl.nodes_ids_data() {
        let gid = g.add_node(node.value.clone(), [node.pos.x, node.pos.y]);
        map.insert(nid, gid);
    }
    for (from, to) in snarl.wires() {
        let Some(&fnid) = map.get(&from.node) else {
            continue;
        };
        let Some(&tnid) = map.get(&to.node) else {
            continue;
        };
        g.connect(fnid, from.output, tnid, to.input);
    }
    g
}

fn load_ir_into_snarl(graph: &BrushNodeGraph, snarl: &mut Snarl<BrushGraphNodeData>) {
    *snarl = Snarl::new();
    let mut map = HashMap::new();
    for n in &graph.nodes {
        let nid = snarl.insert_node(egui::pos2(n.pos[0], n.pos[1]), n.data.clone());
        map.insert(n.id, nid);
    }
    for w in &graph.wires {
        let Some(&from) = map.get(&w.from_node) else {
            continue;
        };
        let Some(&to) = map.get(&w.to_node) else {
            continue;
        };
        snarl.connect(
            OutPinId {
                node: from,
                output: w.from_output,
            },
            InPinId {
                node: to,
                input: w.to_input,
            },
        );
    }
}

struct BrushSnarlViewer;

impl SnarlViewer<BrushGraphNodeData> for BrushSnarlViewer {
    fn connect(
        &mut self,
        from: &OutPin,
        to: &InPin,
        snarl: &mut Snarl<BrushGraphNodeData>,
    ) {
        // Float-only graph: any out → any in is legal; one wire per input.
        for &remote in &to.remotes {
            snarl.disconnect(remote, to.id);
        }
        snarl.connect(from.id, to.id);
    }

    fn title(&mut self, node: &BrushGraphNodeData) -> String {
        node.label()
    }

    fn inputs(&mut self, node: &BrushGraphNodeData) -> usize {
        node.inputs()
    }

    fn outputs(&mut self, node: &BrushGraphNodeData) -> usize {
        node.outputs()
    }

    #[allow(refining_impl_trait)]
    fn show_input(
        &mut self,
        pin: &InPin,
        ui: &mut Ui,
        snarl: &mut Snarl<BrushGraphNodeData>,
    ) -> PinInfo {
        let label = match &snarl[pin.id.node] {
            BrushGraphNodeData::Mul if pin.id.input == 0 => "A",
            BrushGraphNodeData::Mul => "B",
            BrushGraphNodeData::Remap { .. } => "In",
            BrushGraphNodeData::BrushOut { field } => field.label(),
            _ => "In",
        };
        ui.label(label);
        let fill = if matches!(snarl[pin.id.node], BrushGraphNodeData::BrushOut { .. }) {
            OUT_PIN
        } else {
            FLOAT_PIN
        };
        PinInfo::circle().with_fill(fill)
    }

    #[allow(refining_impl_trait)]
    fn show_output(
        &mut self,
        pin: &OutPin,
        ui: &mut Ui,
        snarl: &mut Snarl<BrushGraphNodeData>,
    ) -> PinInfo {
        let (label, fill) = match &snarl[pin.id.node] {
            BrushGraphNodeData::Const { value } => (format!("{value:.3}"), FLOAT_PIN),
            BrushGraphNodeData::Pressure => ("P".into(), SENSOR_PIN),
            BrushGraphNodeData::Speed => ("V".into(), SENSOR_PIN),
            BrushGraphNodeData::Mul => ("×".into(), FLOAT_PIN),
            BrushGraphNodeData::Remap { lo, hi } => (format!("{lo:.2}…{hi:.2}"), FLOAT_PIN),
            BrushGraphNodeData::BrushOut { .. } => ("?".into(), OUT_PIN),
        };
        ui.label(label);
        PinInfo::circle().with_fill(fill)
    }

    fn has_body(&mut self, node: &BrushGraphNodeData) -> bool {
        matches!(
            node,
            BrushGraphNodeData::Const { .. } | BrushGraphNodeData::Remap { .. }
        )
    }

    fn show_body(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        snarl: &mut Snarl<BrushGraphNodeData>,
    ) {
        match &mut snarl[node] {
            BrushGraphNodeData::Const { value } => {
                ui.add(
                    egui::DragValue::new(value)
                        .speed(0.01)
                        .range(-10.0..=600.0),
                );
            }
            BrushGraphNodeData::Remap { lo, hi } => {
                ui.horizontal(|ui| {
                    ui.label("lo");
                    ui.add(egui::DragValue::new(lo).speed(0.01).range(0.0..=600.0));
                    ui.label("hi");
                    ui.add(egui::DragValue::new(hi).speed(0.01).range(0.0..=600.0));
                });
            }
            _ => {}
        }
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<BrushGraphNodeData>) -> bool {
        true
    }

    fn show_graph_menu(
        &mut self,
        pos: egui::Pos2,
        ui: &mut Ui,
        snarl: &mut Snarl<BrushGraphNodeData>,
    ) {
        ui.label("Add node");
        if ui.button("Const").clicked() {
            snarl.insert_node(pos, BrushGraphNodeData::Const { value: 1.0 });
            ui.close();
        }
        if ui.button("Pressure").clicked() {
            snarl.insert_node(pos, BrushGraphNodeData::Pressure);
            ui.close();
        }
        if ui.button("Speed").clicked() {
            snarl.insert_node(pos, BrushGraphNodeData::Speed);
            ui.close();
        }
        if ui.button("Multiply").clicked() {
            snarl.insert_node(pos, BrushGraphNodeData::Mul);
            ui.close();
        }
        if ui.button("Remap").clicked() {
            snarl.insert_node(pos, BrushGraphNodeData::Remap { lo: 0.0, hi: 1.0 });
            ui.close();
        }
        ui.separator();
        ui.label("Brush out");
        for &field in BrushOutField::ALL {
            if ui.button(field.label()).clicked() {
                snarl.insert_node(pos, BrushGraphNodeData::BrushOut { field });
                ui.close();
            }
        }
    }

    fn has_node_menu(&mut self, _node: &BrushGraphNodeData) -> bool {
        true
    }

    fn show_node_menu(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        snarl: &mut Snarl<BrushGraphNodeData>,
    ) {
        if ui.button("Remove").clicked() {
            snarl.remove_node(node);
            ui.close();
        }
    }
}

/// Draw the node canvas + toolbar into `ui`.
/// Returns `true` if Apply succeeded this frame.
pub fn show_brush_node_editor(
    ui: &mut Ui,
    state: &mut BrushNodeEditorState,
    brush: &mut BrushSettings,
) -> bool {
    let mut applied = false;
    ui.horizontal(|ui| {
        if ui.button("Apply → brush").clicked() {
            if state.apply_to_brush(brush).is_ok() {
                applied = true;
            }
        }
        if ui.button("Sync from brush").clicked() {
            state.sync_from_brush(brush);
        }
        if ui.button("Clear").clicked() {
            state.snarl = Snarl::new();
            state.seeded = true;
            state.last_error = None;
        }
        ui.label(
            egui::RichText::new("RMB empty → Add node · drag pins to wire")
                .color(Color32::from_rgb(150, 150, 160))
                .size(12.0),
        );
    });
    if let Some(err) = &state.last_error {
        ui.colored_label(Color32::from_rgb(220, 90, 90), err);
    }
    ui.separator();

    let avail = ui.available_size();
    egui::Frame::NONE
        .fill(Color32::from_rgb(28, 28, 32))
        .stroke(egui::Stroke::new(1.0_f32, Color32::from_rgb(50, 50, 56)))
        .inner_margin(2.0)
        .show(ui, |ui| {
            ui.set_min_size(avail);
            SnarlWidget::new()
                .id(Id::new("beautiful_brush_snarl"))
                .style(state.style)
                .show(&mut state.snarl, &mut BrushSnarlViewer, ui);
        });
    applied
}
