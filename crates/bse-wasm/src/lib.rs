use bse_core::config::weights::CostWeights;
use bse_core::macro_place::topology::build_topology_cache;
use bse_core::meta_search::{
    build_board, BoardSolution, CandidateOpts, KnowledgeBank, MetaSolver, SteerMode,
};
use bse_core::model::circuit::{
    BoardLayout, CompSpec, PlacedBtn, PlacedIc, PlacedSpecial, PlacedSw, WireSeg,
};
use bse_core::model::ids::{CompId, CompType};
use bse_core::parser::parse_netlist;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Deserializer, Serialize};
use wasm_bindgen::prelude::*;

fn de_opt_u8<'de, D>(deserializer: D) -> Result<Option<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawPin {
        Num(u8),
        Str(String),
        Null,
    }
    match Option::<RawPin>::deserialize(deserializer)? {
        Some(RawPin::Num(n)) => Ok(Some(n)),
        Some(RawPin::Str(s)) => Ok(s.trim().parse::<u8>().ok()),
        Some(RawPin::Null) | None => Ok(None),
    }
}

const fn default_pins() -> u8 {
    2
}

#[inline]
pub fn comp_type_to_str(t: CompType) -> &'static str {
    match t {
        CompType::IC => "IC",
        CompType::R => "R",
        CompType::LED => "LED",
        CompType::C => "C",
        CompType::CC => "CC",
        CompType::SW => "SW",
        CompType::BTN2 => "BTN2",
    }
}

#[inline]
pub fn parse_comp_type(s: &str) -> CompType {
    match s.trim().to_ascii_uppercase().as_str() {
        "IC" => CompType::IC,
        "R" => CompType::R,
        "LED" => CompType::LED,
        "C" => CompType::C,
        "CC" => CompType::CC,
        "SW" => CompType::SW,
        "BTN2" | "BTN" => CompType::BTN2,
        _ => CompType::R,
    }
}

/// Dữ liệu linh kiện đầu vào từ mảng `components` của `main.html`
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsCompInput {
    pub id: String,
    #[serde(rename = "type", alias = "comp_type")]
    pub comp_type: String,
    #[serde(default = "default_pins")]
    pub pins: u8,
    #[serde(default)]
    pub rot: u8,
    #[serde(default, alias = "pwr_mode")]
    pub pwr_mode: Option<String>,
    #[serde(default, alias = "custom_pwr")]
    pub custom_pwr: Option<bool>,
    #[serde(default, alias = "vcc_pin", deserialize_with = "de_opt_u8")]
    pub vcc_pin: Option<u8>,
    #[serde(default, alias = "gnd_pin", deserialize_with = "de_opt_u8")]
    pub gnd_pin: Option<u8>,
}

impl JsCompInput {
    pub fn into_comp_spec(self, cid: CompId) -> CompSpec {
        let ct = parse_comp_type(&self.comp_type);
        let pins = match ct {
            CompType::IC => self.pins.max(8),
            CompType::SW => 3,
            _ => 2,
        };
        let is_custom = self.custom_pwr.unwrap_or_else(|| {
            self.pwr_mode
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case("custom"))
        });
        let (vcc_pin, gnd_pin) = if ct == CompType::IC {
            if is_custom {
                (self.vcc_pin, self.gnd_pin)
            } else {
                (Some(pins), Some(pins / 2))
            }
        } else {
            (None, None)
        };
        CompSpec {
            cid,
            id: self.id.trim().to_ascii_uppercase(),
            comp_type: ct,
            pins,
            rot: u8::from(self.rot != 0),
            custom_pwr: is_custom,
            vcc_pin,
            gnd_pin,
        }
    }
}

pub fn parse_js_components(comps_json: &str) -> Vec<CompSpec> {
    if let Ok(js_list) = serde_json::from_str::<Vec<JsCompInput>>(comps_json) {
        return js_list
            .into_iter()
            .enumerate()
            .map(|(i, item)| item.into_comp_spec(CompId(i as u8)))
            .collect();
    }
    serde_json::from_str::<Vec<CompSpec>>(comps_json).unwrap_or_default()
}

/// Ghép cấu hình `LED Đối Xứng` từ giao diện vào chuỗi Netlist dưới dạng chỉ dẫn `#SYM_LED:` (#10)
pub fn merge_sym_led_directive(netlist_text: &str, sym_led_spec: &str) -> String {
    let spec = sym_led_spec.trim();
    if spec.is_empty() {
        return netlist_text.to_string();
    }
    let clean_spec = spec
        .strip_prefix("#SYM_LED:")
        .or_else(|| spec.strip_prefix("SYM_LED:"))
        .unwrap_or(spec)
        .trim();
    if clean_spec.is_empty() {
        return netlist_text.to_string();
    }
    format!("#SYM_LED: {clean_spec}\n{netlist_text}")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsPlacedIc {
    pub id: String,
    pub pins: u8,
    pub rot: u8,
    pub c_min: u8,
    pub c_max: u8,
    pub w: u8,
}

impl From<&PlacedIc> for JsPlacedIc {
    fn from(ic: &PlacedIc) -> Self {
        Self {
            id: ic.id.clone(),
            pins: ic.pins,
            rot: ic.rot,
            c_min: ic.c_min,
            c_max: ic.c_max,
            w: ic.w,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsPlacedSw {
    pub id: String,
    pub c_min: u8,
    pub c_max: u8,
    pub row: i8,
    pub flip: u8,
}

impl From<&PlacedSw> for JsPlacedSw {
    fn from(sw: &PlacedSw) -> Self {
        Self {
            id: sw.id.clone(),
            c_min: sw.c_min,
            c_max: sw.c_max,
            row: sw.row,
            flip: sw.flip,
        }
    }
}

/// Nút bấm 2 chân xuất ra JSON cho `BreadboardRenderer` (Bao gồm trường `bend`: -1, 0, +1) (#2)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsPlacedBtn {
    pub id: String,
    #[serde(rename = "type")]
    pub comp_type: String,
    pub c_min: u8,
    pub c_max: u8,
    pub row_top: i8,
    pub row_bot: i8,
    pub flip: u8,
    #[serde(default)]
    pub bend: i8,
}

impl From<&PlacedBtn> for JsPlacedBtn {
    fn from(b: &PlacedBtn) -> Self {
        Self {
            id: b.id.clone(),
            comp_type: comp_type_to_str(b.comp_type).to_string(),
            c_min: b.c_min,
            c_max: b.c_max,
            row_top: b.row_top,
            row_bot: b.row_bot,
            flip: b.flip,
            bend: b.bend,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsPlacedSpecial {
    pub id: String,
    #[serde(rename = "type")]
    pub comp_type: String,
    pub c1: u8,
    pub r1: i8,
    pub c2: u8,
    pub r2: i8,
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ic_id: Option<String>,
}

impl JsPlacedSpecial {
    pub fn from_core(sp: &PlacedSpecial, comps: &[CompSpec]) -> Self {
        let ic_id = sp.ic_id.and_then(|cid| {
            comps
                .iter()
                .find(|c| c.cid == cid)
                .map(|c| c.id.clone())
        });
        Self {
            id: sp.id.clone(),
            comp_type: comp_type_to_str(sp.comp_type).to_string(),
            c1: sp.c1,
            r1: sp.r1,
            c2: sp.c2,
            r2: sp.r2,
            locked: sp.locked,
            ic_id,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsBoardLayout {
    pub ics: Vec<JsPlacedIc>,
    pub sws: Vec<JsPlacedSw>,
    pub btns: Vec<JsPlacedBtn>,
    pub rs: Vec<JsPlacedSpecial>,
    pub leds: Vec<JsPlacedSpecial>,
    pub cs: Vec<JsPlacedSpecial>,
}

impl JsBoardLayout {
    pub fn from_core(layout: &BoardLayout, comps: &[CompSpec]) -> Self {
        Self {
            ics: layout.ics.iter().map(JsPlacedIc::from).collect(),
            sws: layout.sws.iter().map(JsPlacedSw::from).collect(),
            btns: layout.btns.iter().map(JsPlacedBtn::from).collect(),
            rs: layout
                .rs
                .iter()
                .map(|x| JsPlacedSpecial::from_core(x, comps))
                .collect(),
            leds: layout
                .leds
                .iter()
                .map(|x| JsPlacedSpecial::from_core(x, comps))
                .collect(),
            cs: layout
                .cs
                .iter()
                .map(|x| JsPlacedSpecial::from_core(x, comps))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsWireSeg {
    pub net: String,
    pub cells: [[i8; 2]; 2],
    pub lane: i8,
}

impl From<&WireSeg> for JsWireSeg {
    fn from(w: &WireSeg) -> Self {
        Self {
            net: w.net_name.clone(),
            cells: w.cells,
            lane: w.lane,
        }
    }
}

/// Phương án hoàn chỉnh xuất ra JSON cho `main.html`
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JsBoardSolution {
    #[serde(rename = "L")]
    pub layout: JsBoardLayout,
    pub wires: Vec<JsWireSeg>,
    pub errors: Vec<String>,
    pub warns: Vec<String>,
    pub comp_nets: FxHashMap<String, Vec<String>>,
    pub nets: usize,
    pub len: i32,
    pub max_c: u8,
    pub score: f64,
    pub unplaced: usize,
    pub ortho_count: usize,
    pub persona: String,
    pub sig: u64,
    pub layout_hash: u64,
    pub opts: CandidateOpts,
}

impl JsBoardSolution {
    pub fn from_core(sol: &BoardSolution, comps: &[CompSpec]) -> Self {
        Self {
            layout: JsBoardLayout::from_core(&sol.layout, comps),
            wires: sol.wires.iter().map(JsWireSeg::from).collect(),
            errors: sol.errors.clone(),
            warns: sol.warns.clone(),
            comp_nets: sol.comp_nets.clone(),
            nets: sol.nets,
            len: sol.len,
            max_c: sol.max_c,
            score: sol.score,
            unplaced: sol.unplaced,
            ortho_count: sol.ortho_count,
            persona: sol.persona.clone(),
            sig: sol.sig,
            layout_hash: sol.layout_hash,
            opts: sol.opts.clone(),
        }
    }
}

/// Bộ giải WASM điều phối Đa Quần Đảo & Vi Phẫu kết nối trực tiếp với giao diện `main.html`
#[wasm_bindgen]
pub struct WasmSolver {
    solver: MetaSolver,
    raw_netlist: String,
}

#[wasm_bindgen]
impl WasmSolver {
    #[wasm_bindgen(constructor)]
    pub fn new(
        comps_json: &str,
        netlist_text: &str,
        use_star_hub: bool,
        seed: u32,
        kb_json: Option<String>,
    ) -> WasmSolver {
        let comps = parse_js_components(comps_json);
        let kb = kb_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<KnowledgeBank>(s).ok())
            .unwrap_or_default();
        let solver = MetaSolver::new(comps, netlist_text, use_star_hub, seed, kb);
        WasmSolver {
            solver,
            raw_netlist: netlist_text.to_string(),
        }
    }

    /// Khởi tạo bộ giải kèm cấu hình `LED Đối Xứng` từ giao diện (`sym_led_spec`, VD: "L1=L2, L3=L4")
    pub fn new_with_sym_led(
        comps_json: &str,
        netlist_text: &str,
        sym_led_spec: &str,
        use_star_hub: bool,
        seed: u32,
        kb_json: Option<String>,
    ) -> WasmSolver {
        let merged = merge_sym_led_directive(netlist_text, sym_led_spec);
        let mut ws = Self::new(comps_json, &merged, use_star_hub, seed, kb_json);
        ws.raw_netlist = netlist_text.to_string();
        ws
    }

    /// Cập nhật nóng cấu hình `LED Đối Xứng` từ ô Checkbox + Input trên giao diện (#10)
    pub fn set_sym_led(&mut self, sym_led_spec: &str) {
        let merged = merge_sym_led_directive(&self.raw_netlist, sym_led_spec);
        let updated = parse_netlist(&merged, &self.solver.comps);
        self.solver.parsed.sym_led_groups = updated.sym_led_groups;
    }

    pub fn step_batch(&mut self, batch_size: usize, elapsed_ms: f64) -> String {
        let res = self.solver.step_batch(batch_size, elapsed_ms.max(0.0) as u64);
        serde_json::to_string(&res).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn next_candidate_json(&mut self) -> String {
        let cand = self.solver.next_candidate();
        serde_json::to_string(&cand).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn ingest_result_json(&mut self, sol_json: &str, elapsed_ms: f64) -> bool {
        let Ok(sol) = serde_json::from_str::<BoardSolution>(sol_json) else {
            return false;
        };
        self.solver.ingest_result(sol, elapsed_ms.max(0.0) as u64)
    }

    pub fn get_top_solutions(&self, k: usize) -> String {
        let top = self.solver.pick_top(k);
        let js_top: Vec<JsBoardSolution> = top
            .iter()
            .map(|s| JsBoardSolution::from_core(s, &self.solver.comps))
            .collect();
        serde_json::to_string(&js_top).unwrap_or_else(|_| "[]".to_string())
    }

    pub fn pick_top_json(&self, k: usize) -> String {
        self.get_top_solutions(k)
    }

    pub fn get_log_report(&self) -> String {
        self.solver.logger.build_text_report()
    }

    pub fn get_log_json(&self) -> String {
        serde_json::to_string(&self.solver.logger).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn get_kb_json(&mut self) -> String {
        self.solver.kb.harvest_pool(&self.solver.pool);
        serde_json::to_string(&self.solver.kb).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn set_steer_mode(&mut self, mode_str: &str) {
        let mode = match mode_str.trim().to_ascii_lowercase().as_str() {
            "macro" => SteerMode::Macro,
            "bridge" => SteerMode::Bridge,
            "surgery" => SteerMode::Surgery,
            "lock_io" | "lockio" => SteerMode::LockIo,
            _ => SteerMode::Auto,
        };
        self.solver.set_steer_mode(mode);
    }

    pub fn trigger_tabu_jump(&mut self, tab_idx: usize) {
        self.solver.trigger_tabu_jump(tab_idx);
    }

    pub fn trigger_deep_surgery(&mut self, tab_idx: usize) {
        self.solver.trigger_deep_surgery(tab_idx);
    }
}

/// Hàm WASM độc lập đánh giá 1 cá thể `CandidateOpts` và trả về `BoardSolution` dạng JSON (dùng cho Worker)
#[wasm_bindgen]
pub fn wasm_eval_candidate(
    comps_json: &str,
    netlist_text: &str,
    cand_opts_json: &str,
) -> String {
    let comps = parse_js_components(comps_json);
    let parsed = parse_netlist(netlist_text, &comps);
    let topo = build_topology_cache(&parsed, &comps);
    let weights = CostWeights::default();
    let mut opts = serde_json::from_str::<CandidateOpts>(cand_opts_json).unwrap_or_default();
    let sol = build_board(&mut opts, &comps, &parsed, &topo, &weights);
    serde_json::to_string(&sol).unwrap_or_else(|_| "{}".to_string())
}

/// Hàm WASM độc lập dựng 1 phương án hoàn chỉnh và xuất trực tiếp `JsBoardSolution` cho giao diện
#[wasm_bindgen]
pub fn wasm_build_board_js(
    comps_json: &str,
    netlist_text: &str,
    sym_led_spec: &str,
    use_star_hub: bool,
) -> String {
    let comps = parse_js_components(comps_json);
    let merged = merge_sym_led_directive(netlist_text, sym_led_spec);
    let parsed = parse_netlist(&merged, &comps);
    let topo = build_topology_cache(&parsed, &comps);
    let weights = CostWeights::default();
    let mut opts = CandidateOpts {
        island: 2,
        persona: 2,
        sr: Some(1),
        use_star_hub,
        ..Default::default()
    };
    let sol = build_board(&mut opts, &comps, &parsed, &topo, &weights);
    let js_sol = JsBoardSolution::from_core(&sol, &comps);
    serde_json::to_string(&js_sol).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_js_placed_btn_bend_and_sym_led_wasm_integration() {
        let comps_json = r#"[
            {"id":"U1","type":"IC","pins":14,"rot":0,"pwrMode":"default","vccPin":14,"gndPin":7},
            {"id":"B1","type":"BTN2","pins":2},
            {"id":"R1","type":"R","pins":2},
            {"id":"R2","type":"R","pins":2},
            {"id":"L1","type":"LED","pins":2},
            {"id":"L2","type":"LED","pins":2}
        ]"#;
        let netlist = "
            VCC U1:14; GND U1:7
            U1:12 -> R1 -> L1 -> GND
            U1:3 -> R2 -> L2 -> GND
            B1:1 -> R1:1; B1:2 -> GND
        ";

        let mut solver = WasmSolver::new_with_sym_led(comps_json, netlist, "L1=L2", true, 42, None);
        assert_eq!(
            solver.solver.parsed.sym_led_groups,
            vec![vec![CompId(4), CompId(5)]]
        );

        // Kiểm tra cập nhật nóng cấu hình LED Đối Xứng từ giao diện
        solver.set_sym_led("L1:L2");
        assert_eq!(
            solver.solver.parsed.sym_led_groups,
            vec![vec![CompId(4), CompId(5)]]
        );

        let step_json = solver.step_batch(2, 15.0);
        assert!(step_json.contains("\"iterations\":2"));

        let top_json = solver.get_top_solutions(1);
        assert!(top_json.contains("\"bend\":"));
        let top_sols: Vec<JsBoardSolution> = serde_json::from_str(&top_json).unwrap();
        assert_eq!(top_sols.len(), 1);
        assert_eq!(top_sols[0].layout.btns.len(), 1);
        assert!((-1..=1).contains(&top_sols[0].layout.btns[0].bend));
    }
}
