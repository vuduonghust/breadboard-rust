use crate::cleanup::run_quiescence_cleanup;
use crate::config::personas::get_persona;
use crate::config::weights::{CostWeights, SpecialSpan};
use crate::discrete_place::{
    nc_tag, place_special_components, pwr_tag, setup_macro_and_decoupling, DiscreteContext,
    IcSnapshot, SpecialOptions,
};
use crate::macro_place::packer::{place_components, PlaceOptions};
use crate::macro_place::topology::TopologyCache;
use crate::meta_search::zobrist::{decode_btn_bend, encode_btn_bend, layout_sig, sol_sig};
use crate::model::bitboard::{half_of, is_rail_row, rail_ok, track_index, BoardState};
use crate::model::circuit::{BoardLayout, CompSpec, ParsedNetlist, PlacedSpecial, WireSeg};
use crate::model::geometry::{seg_dist_sq, vis_y, Footprint};
use crate::model::ids::{CompId, CompType, Mulberry32, NetId, PinRef};
use crate::routing::h_route::{FastWire, PassWires};
use crate::routing::orchestrator::{run_routing_engine, RouteItem};
use crate::routing::rail::{rail_connect, PwrSegTracker};
use crate::routing::steiner::{connect_steiner, strip_idx, StripUf};
use core::cmp::Ordering;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

/// Các pha tiến hóa trong quá trình tìm kiếm (`_phase` trong main.html)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchPhase {
    #[default]
    Init,
    P1Unblock,
    P1Bridge,
    P2Ortho,
    P3Compact,
    Diversify,
}

impl SearchPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            SearchPhase::Init => "init",
            SearchPhase::P1Unblock => "p1_unblock",
            SearchPhase::P1Bridge => "p1_bridge",
            SearchPhase::P2Ortho => "p2_ortho",
            SearchPhase::P3Compact => "p3_compact",
            SearchPhase::Diversify => "diversify",
        }
    }
}

/// Cấu hình tham số đầu vào cho 1 cá thể tìm kiếm (`opts` của `buildBoard`)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CandidateOpts {
    pub island: usize,
    pub persona: usize,
    pub order: Vec<usize>,
    pub rot: FxHashMap<CompId, u8>,
    pub half: FxHashMap<CompId, u8>,
    pub io_flip: FxHashMap<CompId, u8>,
    pub gap: Option<i32>,
    pub gaps: Option<Vec<i32>>,
    pub compact: bool,
    pub lock_gaps: bool,
    pub r_order: Vec<usize>,
    pub ortho_weight: Option<f64>,
    pub dyn_br_pen: Option<f64>,
    pub dyn_aux_share: Option<f64>,
    pub evict_cols_mask: u64,
    pub boost_nets: FxHashSet<NetId>,
    pub frozen_specials: FxHashMap<CompId, PlacedSpecial>,
    pub rev: bool,
    pub net_ord: usize,
    pub passes: Option<usize>,
    pub sr: Option<u32>,
    pub r_seed: Option<u32>,
    pub use_star_hub: bool,
    pub phase: SearchPhase,
    pub rot_mutated: bool,
    pub handed_over: bool,
    pub parent_score: Option<f64>,
    pub arm: Option<usize>,
}

/// Kết quả hoàn chỉnh của một phương án lắp mạch (`buildBoard` return object)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BoardSolution {
    pub layout: BoardLayout,
    pub wires: Vec<WireSeg>,
    #[serde(skip)]
    pub fast_wires: Vec<FastWire>,
    pub errors: Vec<String>,
    pub warns: Vec<String>,
    pub comp_nets: FxHashMap<String, Vec<String>>,
    pub nets: usize,
    pub len: i32,
    pub raw_len: f64,
    pub max_c: u8,
    pub score: f64,
    pub unplaced: usize,
    pub ortho_count: usize,
    pub persona: String,
    pub opts: CandidateOpts,
    pub sig: u64,
    pub layout_hash: u64,
}

/// So sánh 2 phương án theo thứ tự ưu tiên phân tầng (`cmpSolutions` trong main.html)
pub fn cmp_solutions(a: &BoardSolution, b: &BoardSolution) -> Ordering {
    if a.unplaced != b.unplaced {
        return a.unplaced.cmp(&b.unplaced);
    }
    if a.errors.len() != b.errors.len() {
        return a.errors.len().cmp(&b.errors.len());
    }
    if a.warns.len() != b.warns.len() {
        return a.warns.len().cmp(&b.warns.len());
    }
    if !a.errors.is_empty() {
        let sc = a.score.total_cmp(&b.score);
        if sc != Ordering::Equal {
            return sc;
        }
        return b.ortho_count.cmp(&a.ortho_count);
    }
    if b.ortho_count != a.ortho_count {
        return b.ortho_count.cmp(&a.ortho_count);
    }
    if (a.score - b.score).abs() >= 500.0 {
        return a.score.total_cmp(&b.score);
    }
    let cost_a = (a.len as f64) + (a.wires.len() as f64) * 3.5 + (a.max_c as f64) * 0.5;
    let cost_b = (b.len as f64) + (b.wires.len() as f64) * 3.5 + (b.max_c as f64) * 0.5;
    cost_a.total_cmp(&cost_b)
}

/// Trích xuất bản đồ linh kiện rời đóng băng phục vụ Vi phẫu (`extractFrozenMap`)
pub fn extract_frozen_map(sol: &BoardSolution) -> FxHashMap<CompId, PlacedSpecial> {
    sol.layout
        .rs
        .iter()
        .chain(&sol.layout.leds)
        .chain(&sol.layout.cs)
        .filter(|x| x.locked || half_of(x.r1) == half_of(x.r2))
        .map(|x| (x.cid, x.clone()))
        .collect()
}

/// Phân tích danh sách lỗi để tìm các mạng cần ưu tiên (`boostNets`) và mặt nạ cột trục xuất (`evictCols`)
pub fn parse_err_info(errs: &[String], parsed: &ParsedNetlist) -> (FxHashSet<NetId>, u64, Vec<u8>) {
    let mut boost_nets = FxHashSet::default();
    let mut evict_mask = 0u64;
    let mut raw_cols = Vec::new();

    for e in errs {
        if let Some(rest) = e.strip_prefix("Net ") {
            if let Some((net_name, _)) = rest.split_once(':') {
                if let Some(n) = parsed.nets.iter().find(|n| n.name == net_name.trim()) {
                    boost_nets.insert(n.id);
                }
            }
        }
        let mut search = e.as_str();
        while let Some(pos) = search.find("cột ") {
            let after = &search[pos + "cột ".len()..];
            let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(col) = num_str.parse::<u8>() {
                if (1..=63).contains(&col) {
                    raw_cols.push(col);
                    for dc in -1i8..=1 {
                        let c_adj = (col as i8 + dc).clamp(1, 63) as u8;
                        evict_mask |= 1u64 << c_adj;
                    }
                }
            }
            search = after;
        }
    }
    (boost_nets, evict_mask, raw_cols)
}

/// Đếm số cặp LED thỏa mãn đối xứng gương qua rãnh E-F hoặc thẳng hàng theo `sym_led_groups` (#10)
pub fn count_sym_led_pairs(layout: &BoardLayout, parsed: &ParsedNetlist) -> usize {
    let mut cnt = 0usize;
    for grp in &parsed.sym_led_groups {
        for i in 0..grp.len() {
            for j in (i + 1)..grp.len() {
                let Some(l1) = layout.leds.iter().find(|l| l.cid == grp[i]) else { continue };
                let Some(l2) = layout.leds.iter().find(|l| l.cid == grp[j]) else { continue };
                let same_cols =
                    (l1.c1 == l2.c1 && l1.c2 == l2.c2) || (l1.c1 == l2.c2 && l1.c2 == l2.c1);
                let opp_half =
                    half_of(l1.r1) != half_of(l2.r1) && half_of(l1.r2) != half_of(l2.r2);
                let aligned = half_of(l1.r1) == half_of(l2.r1)
                    && l1.r1 == l2.r1
                    && l1.r2 == l2.r2
                    && (l1.c1 as i32 - l1.c2 as i32) == (l2.c1 as i32 - l2.c2 as i32);
                if (same_cols && opp_half) || aligned {
                    cnt += 1;
                }
            }
        }
    }
    cnt
}

fn rebuild_routing_state(
    layout: &BoardLayout,
    ctx: &DiscreteContext,
    fast_wires: &[FastWire],
    all_comps: &[CompSpec],
    rev: bool,
) -> (BoardState, Vec<Footprint>, PassWires, StripUf, PwrSegTracker) {
    let mut board = BoardState::new(rev);
    let mut fps = Vec::new();
    layout.block_space(&mut board, &mut fps);

    let placed_macros: FxHashSet<CompId> = layout
        .ics
        .iter()
        .map(|x| x.cid)
        .chain(layout.sws.iter().map(|x| x.cid))
        .chain(layout.btns.iter().map(|x| x.cid))
        .collect();

    for c in all_comps.iter().filter(|c| !c.comp_type.is_special() && placed_macros.contains(&c.cid)) {
        for p in 1..=c.pins {
            let Some((col, row)) = layout.pin_hole(c.cid, p) else { continue };
            if is_rail_row(row) {
                continue;
            }
            let (h, cu) = (half_of(row) as usize, col as usize);
            if board.col_net[h][cu] == 0 {
                board.col_net[h][cu] = match ctx.net_of.get(&PinRef { comp: c.cid, pin: p }) {
                    Some(&net) => pwr_tag(net, c.cid, p, row, c.comp_type, &ctx.main_ic_pwr_pins),
                    None => nc_tag(c.cid, p),
                };
            }
        }
    }

    for sp in layout.cs.iter().chain(&layout.rs).chain(&layout.leds) {
        if sp.c1 == sp.c2 {
            if sp.r1 == -2 || sp.r2 == -2 {
                board.occupy(sp.c1, -1);
            }
            if sp.r1 == 11 || sp.r2 == 11 {
                board.occupy(sp.c1, 10);
            }
        }
        for (pin, col, row) in [(1u8, sp.c1, sp.r1), (2u8, sp.c2, sp.r2)] {
            if is_rail_row(row) {
                continue;
            }
            let (h, cu) = (half_of(row) as usize, col as usize);
            if board.col_net[h][cu] == 0 {
                if let Some(&net) = ctx.net_of.get(&PinRef { comp: sp.cid, pin }) {
                    board.col_net[h][cu] =
                        pwr_tag(net, sp.cid, pin, row, sp.comp_type, &ctx.main_ic_pwr_pins);
                } else if !sp.locked {
                    board.col_net[h][cu] = nc_tag(sp.cid, pin);
                }
            }
        }
    }

    let mut uf = StripUf::new();
    for &(a, b) in &ctx.btn_bridges {
        uf.union(strip_idx(a.0, a.1), strip_idx(b.0, b.1));
    }
    let mut pwr_segs = PwrSegTracker::new();
    let mut pass_wires = PassWires::with_layout(true, layout);

    for &w in fast_wires {
        board.occupy(w.c1, w.r1);
        board.occupy(w.c2, w.r2);
        if is_rail_row(w.r1) || is_rail_row(w.r2) {
            let (cd, rd, cr, rr) = if is_rail_row(w.r1) {
                (w.c2, w.r2, w.c1, w.r1)
            } else {
                (w.c1, w.r1, w.c2, w.r2)
            };
            if cd == cr && rr == -2 {
                board.occupy(cr, -1);
            }
            if cd == cr && rr == 11 {
                board.occupy(cr, 10);
            }
            pwr_segs.push(cd, rd, cr, rr);
            uf.union(strip_idx(cd, half_of(rd)), if w.net == NetId::VCC { 0 } else { 1 });
            pass_wires.wires.push(w);
        } else if w.r1 == w.r2 && w.c1 != w.c2 {
            let h = half_of(w.r1);
            let mut wf = w;
            if let Some(tid) = track_index(w.r1, w.lane) {
                board.occupy_segment(tid, w.c1, w.c2);
                wf.trk = tid as u8;
            }
            if board.col_net[h as usize][w.c1 as usize] == 0 {
                board.col_net[h as usize][w.c1 as usize] = w.net.0;
            }
            if board.col_net[h as usize][w.c2 as usize] == 0 {
                board.col_net[h as usize][w.c2 as usize] = w.net.0;
            }
            uf.union(strip_idx(w.c1, h), strip_idx(w.c2, h));
            pass_wires.wires.push(wf);
        } else {
            if w.c1 == w.c2 && half_of(w.r1) != half_of(w.r2) {
                if board.col_net[0][w.c1 as usize] == 0 {
                    board.col_net[0][w.c1 as usize] = w.net.0;
                }
                if board.col_net[1][w.c1 as usize] == 0 {
                    board.col_net[1][w.c1 as usize] = w.net.0;
                }
                uf.union(strip_idx(w.c1, 0), strip_idx(w.c1, 1));
            }
            pass_wires.wires.push(w);
        }
    }
    (board, fps, pass_wires, uf, pwr_segs)
}

/// Lượt đi dây vét cạn (`retry_failed_items` - #7) sau khi `run_quiescence_cleanup` hoặc LNS giải phóng lỗ cắm
#[allow(clippy::too_many_arguments)]
pub fn retry_failed_items(
    failed_items: &[RouteItem],
    layout: &mut BoardLayout,
    fast_wires: &mut Vec<FastWire>,
    ctx: &DiscreteContext,
    all_comps: &[CompSpec],
    parsed: &ParsedNetlist,
    col_hist: &[f32; 65],
    row_hist: &[f32; 10],
    weights: &CostWeights,
    rev: bool,
) -> (Vec<String>, Vec<RouteItem>) {
    let (mut board, fps, mut pass_wires, mut uf, mut pwr_segs) =
        rebuild_routing_state(layout, ctx, fast_wires, all_comps, rev);
    let mut rem_errs = Vec::new();
    let mut rem_failed = Vec::new();

    let resolve_ep = |pt: (u8, i8), tag: u16, b_ref: &BoardState| -> (u8, i8) {
        let h = half_of(pt.1);
        if b_ref.col_net[h as usize][pt.0 as usize] == tag {
            return pt;
        }
        for c in 1u8..=63 {
            if b_ref.col_net[h as usize][c as usize] == tag {
                return (c, if h == 0 { 2 } else { 7 });
            }
        }
        pt
    };

    for &it in failed_items {
        if it.rail {
            let mut local_err = Vec::new();
            let res = rail_connect(
                &mut board, &mut pass_wires, &mut pwr_segs, &fps, &mut uf,
                it.net, it.tag, &[it.a], 0, col_hist, row_hist, weights, &mut local_err,
            );
            if local_err.is_empty() {
                pass_wires.wires.extend(res);
                pass_wires.commit_swaps();
            } else {
                rem_errs.extend(local_err);
                rem_failed.push(it);
            }
        } else {
            let a = resolve_ep(it.a, it.tag, &board);
            let b = resolve_ep(it.b, it.tag, &board);
            let mut routed = connect_steiner(
                &mut board, &mut pass_wires, &fps, &mut uf, it.tag, a, b,
                63, it.star_hub, 0, col_hist, row_hist, weights,
            );
            if routed.is_none() {
                board.rev = !board.rev;
                routed = connect_steiner(
                    &mut board, &mut pass_wires, &fps, &mut uf, it.tag, a, b,
                    63, it.star_hub, 0, col_hist, row_hist, weights,
                );
                board.rev = !board.rev;
            }
            if let Some(w) = routed {
                pass_wires.wires.extend(w);
                pass_wires.commit_swaps();
            } else {
                let net_name = parsed
                    .nets
                    .iter()
                    .find(|n| n.id == it.net)
                    .map_or("?", |n| n.name.as_str());
                rem_errs.push(format!(
                    "Net {net_name}: không đi được dây từ cột {} tới cột {}",
                    a.0, b.0
                ));
                rem_failed.push(it);
            }
        }
    }

    pass_wires.apply_pin_slides(layout);
    *fast_wires = pass_wires.wires;
    (rem_errs, rem_failed)
}

/// Đại phẫu Đảo ngược vùng kẹt `Routing-First LNS` (#8):
/// Nhấc tạm R/LED/C (không khóa) quanh vùng lỗi -> Cho 1–2 sợi dây đồng lỗi đi TRƯỚC -> Cắm lại R/LED/C vắt qua dây đồng SAU
#[allow(clippy::too_many_arguments)]
fn try_routing_first_lns(
    failed_items: &mut Vec<RouteItem>,
    errs: &mut Vec<String>,
    layout: &mut BoardLayout,
    fast_wires: &mut Vec<FastWire>,
    ctx: &DiscreteContext,
    all_comps: &[CompSpec],
    parsed: &ParsedNetlist,
    col_hist: &[f32; 65],
    row_hist: &[f32; 10],
    weights: &CostWeights,
    rev: bool,
) {
    if failed_items.is_empty() || failed_items.len() > 3 {
        return;
    }
    let snap_layout = layout.clone();
    let snap_wires = fast_wires.clone();

    let mut zone_mask = 0u64;
    for it in failed_items.iter() {
        let (c1, c2) = (it.a.0, if it.rail { it.a.0 } else { it.b.0 });
        for c in c1.saturating_sub(4).max(1)..=(c1 + 4).min(63) {
            zone_mask |= 1u64 << c;
        }
        for c in c2.saturating_sub(4).max(1)..=(c2 + 4).min(63) {
            zone_mask |= 1u64 << c;
        }
        if (c1 as i32 - c2 as i32).abs() <= 18 {
            for c in c1.min(c2)..=c1.max(c2) {
                zone_mask |= 1u64 << c;
            }
        }
    }

    // SỬA LỖI LNS: Cho phép nhấc CẢ các linh kiện rời thuộc chính mạng lỗi nếu mạng lỗi đó có chân IC cứng
    // (Ví dụ: R4 nối vào U4:3 ở cột 56 của NET25 — nhấc R4 ra để dây đồng nối U4:3 <-> U4:12 trước, sau đó cắm R4 vào bất kỳ cột nào của NET25!)
    let mut lift_cids = FxHashSet::default();
    for sp in layout.rs.iter().chain(&layout.leds).chain(&layout.cs) {
        if sp.locked {
            continue;
        }
        if (zone_mask & ((1u64 << sp.c1) | (1u64 << sp.c2))) != 0 {
            lift_cids.insert(sp.cid);
        }
    }
    for sp in layout.rs.iter().chain(&layout.leds).chain(&layout.cs) {
        if sp.locked || lift_cids.contains(&sp.cid) {
            continue;
        }
        let n1 = ctx.net_of.get(&PinRef { comp: sp.cid, pin: 1 }).copied();
        let n2 = ctx.net_of.get(&PinRef { comp: sp.cid, pin: 2 }).copied();
        for net_opt in [n1, n2] {
            let Some(net) = net_opt.filter(|n| !n.is_power()) else { continue };
            let pin_cnt = parsed.nets.iter().find(|n| n.id == net).map_or(0, |n| n.pins.len());
            if pin_cnt == 2 && !fast_wires.iter().any(|w| w.net == net) {
                let partner_lifted = layout
                    .rs
                    .iter()
                    .chain(&layout.leds)
                    .chain(&layout.cs)
                    .any(|o| {
                        lift_cids.contains(&o.cid)
                            && (ctx.net_of.get(&PinRef { comp: o.cid, pin: 1 }) == Some(&net)
                                || ctx.net_of.get(&PinRef { comp: o.cid, pin: 2 }) == Some(&net))
                    });
                if partner_lifted {
                    lift_cids.insert(sp.cid);
                }
            }
        }
    }

    if lift_cids.is_empty() {
        return;
    }

    let mut lifted = Vec::with_capacity(lift_cids.len());
    for vec_ref in [&mut layout.cs, &mut layout.rs, &mut layout.leds] {
        let mut i = 0;
        while i < vec_ref.len() {
            if lift_cids.contains(&vec_ref[i].cid) {
                lifted.push(vec_ref.remove(i));
            } else {
                i += 1;
            }
        }
    }

    // Bước 1: Cho các sợi dây đồng lỗi đi TRƯỚC khi các R/LED/C vùng nghẽn đã nhấc ra
    let (new_errs, new_failed) = retry_failed_items(
        failed_items, layout, fast_wires, ctx, all_comps, parsed, col_hist, row_hist, weights, rev,
    );
    if new_errs.len() >= errs.len() {
        *layout = snap_layout;
        *fast_wires = snap_wires;
        return;
    }

    // Bước 2: Cắm lại các R/LED/C đã nhấc bay vắt qua đầu dây đồng vừa đi
    let (mut board, _, _, _, _) =
        rebuild_routing_state(layout, ctx, fast_wires, all_comps, rev);

    let cand_holes_for = |b_ref: &BoardState,
                          net_opt: Option<NetId>,
                          orig_c: u8,
                          orig_r: i8,
                          cid: CompId,
                          pin: u8,
                          ct: CompType|
     -> Vec<(u8, i8)> {
        let mut out = Vec::with_capacity(48);
        if let Some(net) = net_opt.filter(|n| n.is_power()) {
            let r_rows = if net == NetId::VCC { [-1i8, 11i8] } else { [-2i8, 10i8] };
            for c in 3u8..=61 {
                if rail_ok(c) {
                    for rr in r_rows {
                        if !b_ref.is_occupied(c, rr) {
                            out.push((c, rr));
                        }
                    }
                }
            }
            if !is_rail_row(orig_r) {
                let h = half_of(orig_r);
                let tag = pwr_tag(net, cid, pin, orig_r, ct, &ctx.main_ic_pwr_pins);
                if b_ref.col_net[h as usize][orig_c as usize] == tag {
                    let r_range = if h == 0 { 0i8..=4 } else { 5i8..=9 };
                    for r in r_range {
                        if !b_ref.is_occupied(orig_c, r) {
                            out.push((orig_c, r));
                        }
                    }
                }
            }
            return out;
        }
        if let Some(net) = net_opt {
            for h in [0u8, 1u8] {
                for c in 1u8..=63 {
                    if b_ref.col_net[h as usize][c as usize] == net.0 {
                        let r_range = if h == 0 { 0i8..=4 } else { 5i8..=9 };
                        for r in r_range {
                            if !b_ref.is_occupied(c, r) {
                                out.push((c, r));
                            }
                        }
                    }
                }
            }
            if out.is_empty() {
                for h in [0u8, 1u8] {
                    for c in 1u8..=63 {
                        if b_ref.col_net[h as usize][c as usize] == 0 {
                            let r_range = if h == 0 { 0i8..=3 } else { 6i8..=9 };
                            for r in r_range {
                                if !b_ref.is_occupied(c, r) {
                                    out.push((c, r));
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    };

    for sp_item in lifted {
        let sp_span = SpecialSpan::for_type(sp_item.comp_type);
        let n1 = ctx.net_of.get(&PinRef { comp: sp_item.cid, pin: 1 }).copied();
        let n2 = ctx.net_of.get(&PinRef { comp: sp_item.cid, pin: 2 }).copied();
        let is_pwr1 = n1.is_some_and(NetId::is_power);
        let is_pwr2 = n2.is_some_and(NetId::is_power);
        let holes_a = cand_holes_for(&board, n1, sp_item.c1, sp_item.r1, sp_item.cid, 1, sp_item.comp_type);
        let holes_b = cand_holes_for(&board, n2, sp_item.c2, sp_item.r2, sp_item.cid, 2, sp_item.comp_type);

        let placed_obstacles: Vec<([f64; 2], [f64; 2])> = layout
            .rs
            .iter()
            .chain(&layout.leds)
            .chain(layout.cs.iter().filter(|x| !x.locked))
            .map(|pl| ([pl.c1 as f64, vis_y(pl.r1)], [pl.c2 as f64, vis_y(pl.r2)]))
            .collect();

        let (mut best_pair, mut best_cost) = (None, 1e9_f64);
        for &(ca, ra) in &holes_a {
            let rail_a = is_rail_row(ra);
            let ha = half_of(ra);
            let vya = vis_y(ra);
            for &(cb, rb) in &holes_b {
                let rail_b = is_rail_row(rb);
                let hb = half_of(rb);
                if (rail_a && rail_b) || (!rail_a && !rail_b && ca == cb && ha == hb) {
                    continue;
                }
                let one_rail = rail_a != rail_b;
                let crosses_groove = ha != hb;
                if crosses_groove && (one_rail || is_pwr1 || is_pwr2) {
                    continue;
                }
                if ca == cb {
                    if (ra == -2 || rb == -2) && board.is_occupied(ca, -1) {
                        continue;
                    }
                    if (ra == 11 || rb == 11) && board.is_occupied(ca, 10) {
                        continue;
                    }
                }
                let vyb = vis_y(rb);
                let dy = if one_rail || crosses_groove { (vya - vyb).abs() } else { (ra - rb).abs() as f64 };
                let d = ((ca as f64) - (cb as f64)).hypot(dy);
                if d < sp_span.min || d > sp_span.max {
                    continue;
                }
                let is_ortho = ca == cb || ra == rb || (one_rail && (ca as i32 - cb as i32).abs() <= 1);
                let dist_orig = ((ca as i32 - sp_item.c1 as i32).abs()
                    + (cb as i32 - sp_item.c2 as i32).abs()) as f64;
                let groove_pen = if ra == 4 || ra == 5 || rb == 4 || rb == 5 { 6.0 } else { 0.0 };
                let cost = (d - sp_span.pref).abs() * 0.4 + dist_orig * 0.25 + groove_pen
                    - if is_ortho { 8.0 } else { 0.0 };
                if cost >= best_cost {
                    continue;
                }
                let (p1, p2) = ([ca as f64, vya], [cb as f64, vyb]);
                let hits = placed_obstacles.iter().any(|&(q1, q2)| {
                    seg_dist_sq(p1, p2, q1, q2) < weights.spec_body_clearance_sq
                });
                if !hits {
                    best_cost = cost;
                    best_pair = Some(((ca, ra), (cb, rb)));
                }
            }
        }

        let Some(((ca, ra), (cb, rb))) = best_pair else {
            *layout = snap_layout;
            *fast_wires = snap_wires;
            return;
        };
        board.occupy(ca, ra);
        board.occupy(cb, rb);
        if ca == cb {
            if ra == -2 || rb == -2 {
                board.occupy(ca, -1);
            }
            if ra == 11 || rb == 11 {
                board.occupy(ca, 10);
            }
        }
        if !is_rail_row(ra) && board.col_net[half_of(ra) as usize][ca as usize] == 0 {
            if let Some(net) = n1 {
                board.col_net[half_of(ra) as usize][ca as usize] =
                    pwr_tag(net, sp_item.cid, 1, ra, sp_item.comp_type, &ctx.main_ic_pwr_pins);
            }
        }
        if !is_rail_row(rb) && board.col_net[half_of(rb) as usize][cb as usize] == 0 {
            if let Some(net) = n2 {
                board.col_net[half_of(rb) as usize][cb as usize] =
                    pwr_tag(net, sp_item.cid, 2, rb, sp_item.comp_type, &ctx.main_ic_pwr_pins);
            }
        }
        let placed = PlacedSpecial { c1: ca, r1: ra, c2: cb, r2: rb, ..sp_item };
        match placed.comp_type {
            CompType::R => layout.rs.push(placed),
            CompType::LED => layout.leds.push(placed),
            _ => layout.cs.push(placed),
        }
    }

    *errs = new_errs;
    *failed_items = new_failed;
}

/// Xây dựng, đặt linh kiện, định tuyến đa lượt và hậu kiểm toàn bộ mặt bo (`buildBoard` trong main.html)
pub fn build_board(
    opts: &mut CandidateOpts,
    all_comps: &[CompSpec],
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    weights: &CostWeights,
) -> BoardSolution {
    let p_cfg = get_persona(opts.persona);
    let mut rng = opts.sr.map(Mulberry32::new);

    let ordered_comps: Vec<CompSpec> = if !opts.order.is_empty() {
        opts.order
            .iter()
            .filter_map(|&idx| all_comps.get(idx).cloned())
            .collect()
    } else {
        all_comps.to_vec()
    };

    let mut errors = parsed.errors.clone();
    let mut warns = parsed.warns.clone();

    let mut comp_heat = FxHashMap::default();
    if p_cfg.hot_gap {
        for n in &parsed.nets {
            if n.id.is_power() {
                continue;
            }
            let add_w = if n.pins.len() >= 3 { 1.5 } else { 1.0 };
            for pr in &n.pins {
                *comp_heat.entry(pr.comp).or_insert(0.0) += add_w;
            }
        }
    }

    let mut btn_bend = FxHashMap::default();
    for c in all_comps.iter().filter(|c| c.comp_type == CompType::BTN2) {
        if let Some(&code) = opts.rot.get(&c.cid) {
            btn_bend.insert(c.cid, decode_btn_bend(code));
        }
    }

    let mut place_opts = PlaceOptions {
        persona: opts.persona,
        sr: opts.sr,
        gap: opts.gap,
        gaps: opts.gaps.clone(),
        compact: opts.compact,
        lock_gaps: opts.lock_gaps,
        rot: opts.rot.clone(),
        half: opts.half.clone(),
        io_flip: opts.io_flip.clone(),
        btn_bend,
        comp_heat,
    };

    let pack_res = place_components(
        &ordered_comps,
        all_comps,
        &mut place_opts,
        Some((parsed, topo)),
        weights,
    );
    opts.gaps = place_opts.gaps;
    for (&cid, &bend) in &place_opts.btn_bend {
        opts.rot.entry(cid).or_insert_with(|| encode_btn_bend(bend));
    }
    let mut layout = pack_res.layout;

    let mut board = BoardState::new(opts.rev);
    let mut fps = Vec::new();
    layout.block_space(&mut board, &mut fps);

    let placed_ids: FxHashSet<CompId> = layout
        .ics
        .iter()
        .map(|x| x.cid)
        .chain(layout.sws.iter().map(|x| x.cid))
        .chain(layout.btns.iter().map(|x| x.cid))
        .collect();

    let mut unplaced = 0usize;
    for c in all_comps {
        if !c.comp_type.is_special() && !placed_ids.contains(&c.cid) {
            warns.push(format!("{}: hết chỗ trên board, không được đặt", c.id));
            unplaced += 1;
        }
    }

    let mut ctx = setup_macro_and_decoupling(
        &mut board,
        &mut layout,
        parsed,
        topo,
        all_comps,
        weights,
        &mut errors,
    );

    let mut col_hist = [0.0f32; 65];
    let mut row_hist = [0.0f32; 10];
    let snap_ic = IcSnapshot::capture(&board, &ctx, unplaced, errors.len());

    let mut spec_opts = SpecialOptions {
        persona: opts.persona,
        sr: opts.sr.unwrap_or(1),
        r_seed: opts.r_seed,
        ortho_weight: opts.ortho_weight,
        dyn_br_pen: opts.dyn_br_pen,
        dyn_aux_share: opts.dyn_aux_share,
        r_order: opts.r_order.clone(),
        evict_cols_mask: opts.evict_cols_mask,
        boost_nets: opts.boost_nets.clone(),
        frozen_specials: opts.frozen_specials.clone(),
    };

    place_special_components(
        false,
        0,
        &spec_opts,
        &snap_ic,
        &mut board,
        &mut layout,
        &mut ctx,
        &fps,
        &ordered_comps,
        all_comps,
        parsed,
        &col_hist,
        &row_hist,
        weights,
        &mut unplaced,
        &mut errors,
    );

    let max_passes = if unplaced > 0 {
        1
    } else {
        opts.passes.unwrap_or(p_cfg.passes).max(1)
    };

    let mut best_pass = run_routing_engine(
        max_passes,
        opts.use_star_hub,
        opts.net_ord,
        &opts.boost_nets,
        &p_cfg,
        &mut rng,
        &mut board,
        &layout,
        &ctx,
        &fps,
        parsed,
        &mut col_hist,
        &mut row_hist,
        weights,
    );
    best_pass.apply_spec_pins(&mut layout);

    let mut best_spec_snap = (
        layout.rs.clone(),
        layout.leds.clone(),
        layout.cs.clone(),
        unplaced,
        errors.clone(),
    );

    let fb_rounds = if unplaced == 0 && !best_pass.errs.is_empty() && best_pass.errs.len() <= 3 {
        p_cfg.fb
    } else {
        0
    };

    for fb in 1..=fb_rounds {
        let mut cur_frozen = FxHashMap::default();
        for x in best_spec_snap
            .0
            .iter()
            .chain(&best_spec_snap.1)
            .chain(&best_spec_snap.2)
        {
            if (x.locked || half_of(x.r1) == half_of(x.r2))
                && col_hist[x.c1 as usize] < 2.2
                && col_hist[x.c2 as usize] < 2.2
            {
                cur_frozen.insert(x.cid, x.clone());
            }
        }
        spec_opts.frozen_specials = cur_frozen;
        place_special_components(
            true,
            (fb as u32) * 137,
            &spec_opts,
            &snap_ic,
            &mut board,
            &mut layout,
            &mut ctx,
            &fps,
            &ordered_comps,
            all_comps,
            parsed,
            &col_hist,
            &row_hist,
            weights,
            &mut unplaced,
            &mut errors,
        );
        if unplaced == 0 {
            let fb_budget = (((max_passes as f64) * 0.75).floor() as usize).max(14);
            let fb_pass = run_routing_engine(
                fb_budget,
                opts.use_star_hub,
                opts.net_ord,
                &opts.boost_nets,
                &p_cfg,
                &mut rng,
                &mut board,
                &layout,
                &ctx,
                &fps,
                parsed,
                &mut col_hist,
                &mut row_hist,
                weights,
            );
            let is_better_global = fb_pass.errs.len() < best_pass.errs.len()
                || (fb_pass.errs.len() == best_pass.errs.len()
                    && !best_pass.errs.is_empty()
                    && fb_pass.fail_span < best_pass.fail_span)
                || (fb_pass.errs.is_empty()
                    && best_pass.errs.is_empty()
                    && (fb_pass.w_len + (fb_pass.wires.len() as f64) * 3.5)
                        < (best_pass.w_len + (best_pass.wires.len() as f64) * 3.5));
            if is_better_global {
                best_pass = fb_pass;
                best_pass.apply_spec_pins(&mut layout);
                best_spec_snap = (
                    layout.rs.clone(),
                    layout.leds.clone(),
                    layout.cs.clone(),
                    unplaced,
                    errors.clone(),
                );
                if best_pass.errs.is_empty() {
                    break;
                }
            }
        }
        layout.rs = best_spec_snap.0.clone();
        layout.leds = best_spec_snap.1.clone();
        layout.cs = best_spec_snap.2.clone();
        unplaced = best_spec_snap.3;
        errors = best_spec_snap.4.clone();
    }

    let mut fast_wires = best_pass.wires;

    // Hạng mục #7: Kích hoạt run_quiescence_cleanup ngay khi còn <= 3 lỗi kèm lượt đi dây vét cạn retry_failed_items
    if unplaced == 0 && errors.is_empty() && best_pass.errs.len() <= 3 {
        run_quiescence_cleanup(&mut layout, &mut fast_wires, weights);
        if !best_pass.failed_items.is_empty() {
            let (rem_errs, rem_failed) = retry_failed_items(
                &best_pass.failed_items,
                &mut layout,
                &mut fast_wires,
                &ctx,
                all_comps,
                parsed,
                &col_hist,
                &row_hist,
                weights,
                opts.rev,
            );
            if rem_errs.len() < best_pass.errs.len() {
                best_pass.errs = rem_errs;
                best_pass.failed_items = rem_failed;
                if best_pass.errs.is_empty() {
                    run_quiescence_cleanup(&mut layout, &mut fast_wires, weights);
                }
            }
        }
        // Hạng mục #8: Nếu sau Bước #7 vẫn còn 1–3 dây đồng bị đứt, kích hoạt Gỡ rối đảo ngược Routing-First LNS
        if (1..=3).contains(&best_pass.errs.len()) {
            try_routing_first_lns(
                &mut best_pass.failed_items,
                &mut best_pass.errs,
                &mut layout,
                &mut fast_wires,
                &ctx,
                all_comps,
                parsed,
                &col_hist,
                &row_hist,
                weights,
                opts.rev,
            );
            if best_pass.errs.is_empty() {
                run_quiescence_cleanup(&mut layout, &mut fast_wires, weights);
            }
        }
    }

    errors.extend(best_pass.errs);

    let mut net_names: FxHashMap<NetId, String> = FxHashMap::default();
    net_names.insert(NetId::VCC, "VCC".into());
    net_names.insert(NetId::GND, "GND".into());
    for n in &parsed.nets {
        net_names.insert(n.id, n.name.clone());
    }

    let mut comp_nets: FxHashMap<String, Vec<String>> = FxHashMap::default();
    for (&pr, &nid) in &ctx.net_of {
        if let Some(c) = all_comps.iter().find(|c| c.cid == pr.comp) {
            let n_name = net_names.get(&nid).cloned().unwrap_or_default();
            let list = comp_nets.entry(c.id.clone()).or_default();
            if !list.contains(&n_name) {
                list.push(n_name);
            }
        }
    }
    for lc in &ctx.locked_ic_caps {
        comp_nets.insert(lc.id.clone(), vec!["VCC".into(), "GND".into()]);
    }

    let raw_len: f64 = fast_wires
        .iter()
        .map(|w| (w.c2 as f64 - w.c1 as f64).hypot(w.r2 as f64 - w.r1 as f64))
        .sum();
    let wires: Vec<WireSeg> = fast_wires
        .iter()
        .map(|w| {
            let n_str = net_names.get(&w.net).cloned().unwrap_or_else(|| "?".into());
            w.to_wire_seg(n_str)
        })
        .collect();

    let max_c = layout
        .ics
        .iter()
        .map(|x| x.c_max)
        .chain(layout.sws.iter().map(|x| x.c_max))
        .chain(layout.btns.iter().map(|x| x.c_max))
        .chain(layout.rs.iter().map(|x| x.c1.max(x.c2)))
        .chain(layout.leds.iter().map(|x| x.c1.max(x.c2)))
        .chain(layout.cs.iter().map(|x| x.c1.max(x.c2)))
        .max()
        .unwrap_or(1);

    let mut ortho_count = 0usize;
    let mut span_penalty = 0.0_f64;
    for c in layout.rs.iter().chain(&layout.leds).chain(&layout.cs) {
        let same_half = half_of(c.r1) == half_of(c.r2);
        let one_rail = is_rail_row(c.r1) != is_rail_row(c.r2);
        let near_rail_ortho = one_rail && same_half && (c.c1 as i32 - c.c2 as i32).abs() <= 1;
        // SỬA LỖI CHẤM ĐIỂM ORTHO: Không bao giờ cộng ortho_count cho linh kiện không khóa mà vắt qua rãnh giữa!
        if c.locked || (same_half && (c.c1 == c.c2 || c.r1 == c.r2 || near_rail_ortho)) {
            ortho_count += 1;
        }
        if !c.locked {
            let d_span = (c.c1 as i32 - c.c2 as i32).abs();
            if d_span > 9 {
                span_penalty += ((d_span - 9) as f64) * 2.5;
            }
        }
    }

    let sym_led_pairs = count_sym_led_pairs(&layout, parsed);
    let sym_led_bonus = (sym_led_pairs as f64) * (weights.sym_led_pair_bonus * 31.25);

    let err_gradient = if !errors.is_empty() {
        (best_pass.fail_span as f64) * 2000.0 + (max_c as f64)
    } else {
        raw_len + (wires.len() as f64) * 4.5 + span_penalty + (max_c as f64) * 0.5
    };
    let score = (unplaced as f64) * 1e10
        + (errors.len() as f64) * 1e8
        + (warns.len() as f64) * 1e6
        + ((1000usize.saturating_sub(ortho_count)) as f64) * 1e3
        - sym_led_bonus
        + err_gradient;

    let sig = sol_sig(&layout, &fast_wires, errors.len());
    let layout_hash = layout_sig(&layout);

    BoardSolution {
        layout,
        wires,
        fast_wires,
        errors,
        warns,
        comp_nets,
        nets: parsed.nets.len(),
        len: raw_len.round() as i32,
        raw_len,
        max_c,
        score,
        unplaced,
        ortho_count,
        persona: p_cfg.name.to_string(),
        opts: opts.clone(),
        sig,
        layout_hash,
    }
}
