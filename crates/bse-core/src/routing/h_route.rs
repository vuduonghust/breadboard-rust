use crate::config::weights::{CostWeights, SpecialSpan};
use crate::discrete_place::decoupling::base_net_of;
use crate::model::bitboard::{half_of, is_rail_row, span_mask, track_index, BoardState};
use crate::model::circuit::{BoardLayout, WireSeg};
use crate::model::geometry::{seg_dist_sq, v_seg_free, vis_y, Footprint};
use crate::model::ids::{CompId, CompType, NetId};

/// Giá trị đánh dấu đoạn dây không chiếm rãnh ngang (dây cầu rãnh dọc hoặc dây nhảy nguồn)
pub const NO_TRACK: u8 = u8::MAX;

/// Đoạn dây 8-byte trên Stack (`Copy`, không cấp phát `String`) dùng trong vòng lặp định tuyến
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FastWire {
    pub net: NetId,
    pub c1: u8,
    pub r1: i8,
    pub c2: u8,
    pub r2: i8,
    pub lane: i8,
    pub trk: u8,
}

impl FastWire {
    #[inline(always)]
    pub const fn new_h(net: NetId, c1: u8, c2: u8, row: i8, lane: i8, trk: usize) -> Self {
        Self { net, c1, r1: row, c2, r2: row, lane, trk: trk as u8 }
    }

    #[inline(always)]
    pub const fn new_v(net: NetId, c1: u8, r1: i8, c2: u8, r2: i8) -> Self {
        Self { net, c1, r1, c2, r2, lane: 0, trk: NO_TRACK }
    }

    #[inline]
    pub fn to_wire_seg(self, net_name: String) -> WireSeg {
        WireSeg {
            net: self.net,
            net_name,
            cells: [[self.c1 as i8, self.r1], [self.c2 as i8, self.r2]],
            lane: self.lane,
            trk: (self.trk != NO_TRACK).then_some(self.trk as usize),
        }
    }
}

/// Thông tin chân linh kiện rời (R, LED, C) phục vụ trượt/đổi chỗ nội cột tối đa 1 hàng (#5)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MovableSpecPin {
    pub cid: CompId,
    pub comp_type: CompType,
    pub c1: u8,
    pub r1: i8,
    pub c2: u8,
    pub r2: i8,
    pub orig_r1: i8,
    pub orig_r2: i8,
    pub locked: bool,
}

/// Quản lý danh sách dây trong Pass hiện tại kèm bộ đệm hoàn tác `try_row_swap_h` & `try_slide_spec_pin_1row`
#[derive(Clone, Debug)]
pub struct PassWires {
    pub wires: Vec<FastWire>,
    pub swap_enabled: bool,
    pub spec_pins: Vec<MovableSpecPin>,
    swap_log: [(u16, FastWire); 64],
    swap_len: usize,
    pin_swap_log: [(u16, i8, i8); 64],
    pin_swap_len: usize,
}

impl PassWires {
    pub fn new(swap_enabled: bool) -> Self {
        Self {
            wires: Vec::with_capacity(96),
            swap_enabled,
            spec_pins: Vec::new(),
            swap_log: [(0, FastWire::default()); 64],
            swap_len: 0,
            pin_swap_log: [(0, 0, 0); 64],
            pin_swap_len: 0,
        }
    }

    pub fn with_layout(swap_enabled: bool, layout: &BoardLayout) -> Self {
        let mut pw = Self::new(swap_enabled);
        pw.load_layout(layout);
        pw
    }

    pub fn load_layout(&mut self, layout: &BoardLayout) {
        self.spec_pins.clear();
        for s in layout.cs.iter().chain(&layout.leds).chain(&layout.rs) {
            self.spec_pins.push(MovableSpecPin {
                cid: s.cid,
                comp_type: s.comp_type,
                c1: s.c1,
                r1: s.r1,
                c2: s.c2,
                r2: s.r2,
                orig_r1: s.r1,
                orig_r2: s.r2,
                locked: s.locked,
            });
        }
        self.pin_swap_len = 0;
    }

    pub fn apply_pin_slides(&self, layout: &mut BoardLayout) {
        for s in layout.cs.iter_mut().chain(&mut layout.leds).chain(&mut layout.rs) {
            if let Some(mp) = self.spec_pins.iter().find(|p| p.cid == s.cid) {
                s.r1 = mp.r1;
                s.r2 = mp.r2;
            }
        }
    }

    #[inline(always)]
    pub const fn snapshot_swaps(&self) -> usize {
        ((self.pin_swap_len & 0xFFFF) << 16) | (self.swap_len & 0xFFFF)
    }

    #[inline]
    pub fn record_swap(&mut self, idx: usize, new_wire: FastWire) {
        if self.swap_len < self.swap_log.len() {
            self.swap_log[self.swap_len] = (idx as u16, self.wires[idx]);
            self.swap_len += 1;
        }
        self.wires[idx] = new_wire;
    }

    #[inline]
    pub fn record_pin_slide(&mut self, idx: usize, new_r1: i8, new_r2: i8) {
        let cur = self.spec_pins[idx];
        if self.pin_swap_len < self.pin_swap_log.len() {
            self.pin_swap_log[self.pin_swap_len] = (idx as u16, cur.r1, cur.r2);
            self.pin_swap_len += 1;
        }
        self.spec_pins[idx].r1 = new_r1;
        self.spec_pins[idx].r2 = new_r2;
    }

    #[inline]
    pub fn restore_swaps(&mut self, snap: usize) {
        let target_swap = snap & 0xFFFF;
        let target_pin = (snap >> 16) & 0xFFFF;
        while self.swap_len > target_swap {
            self.swap_len -= 1;
            let (idx, old_w) = self.swap_log[self.swap_len];
            self.wires[idx as usize] = old_w;
        }
        while self.pin_swap_len > target_pin {
            self.pin_swap_len -= 1;
            let (idx, old_r1, old_r2) = self.pin_swap_log[self.pin_swap_len];
            if let Some(sp) = self.spec_pins.get_mut(idx as usize) {
                sp.r1 = old_r1;
                sp.r2 = old_r2;
            }
        }
    }

    #[inline(always)]
    pub fn commit_swaps(&mut self) {
        self.swap_len = 0;
        self.pin_swap_len = 0;
    }
}

/// Kiểm tra quy tắc cấm dây tới ngang (`lane = 0`) tại các lỗ bị hạn chế ngang của `BTN2` (#2, #6):
/// - Cấm `lane = 0` nếu hàng `row` nằm trong vùng bẻ chân của `BTN2` trên đoạn `[c1, c2]`
/// - Cho phép `lane = ±1` (dây đi ở làn khoảng cách và cắm dọc từ làn khoảng cách vào lỗ)
#[inline]
pub fn is_btn_lane_allowed(fps: &[Footprint], row: i8, lane: i8, c1: u8, c2: u8) -> bool {
    if lane != 0 {
        return true;
    }
    let (c_min, c_max) = (c1.min(c2) as f64, c1.max(c2) as f64);
    let rf = row as f64;
    for f in fps {
        if !f.is_sw && (f.r1 < 3.5 || f.r0 > 5.5) && rf > f.r0 && rf < f.r1 && c_max >= f.c0 && c_min <= f.c1 {
            return false;
        }
    }
    true
}

#[inline]
pub fn get_available_track_with_btn_rule(
    board: &BoardState,
    fps: &[Footprint],
    row: i8,
    c1: u8,
    c2: u8,
    pref_gap_dir: i8,
) -> Option<(i8, usize)> {
    let pref_lane = if row == 4 { -1 } else { 1 };
    let lanes = if pref_gap_dir != 0 {
        [pref_gap_dir, -pref_gap_dir, 0]
    } else {
        [0, pref_lane, -pref_lane]
    };
    let mask = span_mask(c1, c2);
    for lane in lanes {
        if !is_btn_lane_allowed(fps, row, lane, c1, c2) {
            continue;
        }
        if let Some(tid) = track_index(row, lane) {
            if (board.track_masks[tid] & mask) == 0 {
                return Some((lane, tid));
            }
        }
    }
    None
}

/// Kiểm tra cột `col` có tiềm năng làm cầu rãnh E-F (`isPotentialBridgeCol` trong main.html)
#[inline]
pub fn is_potential_bridge_col(
    board: &BoardState,
    fps: &[Footprint],
    col: u8,
    for_tag: u16,
    rem_cross: i32,
) -> bool {
    if rem_cross <= 0 {
        return false;
    }
    if board.is_occupied(col, 4) || board.is_occupied(col, 5) || !v_seg_free(fps, col, 4, 5) {
        return false;
    }
    let cu = col as usize;
    let (n0, n1) = (board.col_net[0][cu], board.col_net[1][cu]);
    let ok0 = n0 == 0 || (for_tag != 0 && n0 == for_tag);
    let ok1 = n1 == 0 || (for_tag != 0 && n1 == for_tag);
    ok0 && ok1
}

/// Độ nghẽn trung bình trên đoạn cột `[c1, c2]` (`spanCongestion` trong main.html)
#[inline]
pub fn span_congestion(col_hist: &[f32; 65], c1: u8, c2: u8) -> f64 {
    let (lo, hi) = (c1.min(c2), c1.max(c2));
    let mut s = 0.0_f64;
    for c in lo..=hi {
        s += col_hist[c as usize] as f64;
    }
    s / ((hi - lo + 1).max(1) as f64)
}

/// Đo áp lực làn ngang trên đoạn `[c_a, c_b]` của nửa `h` (`halfSpanLoad` trong main.html)
#[inline]
pub fn half_span_load(
    board: &BoardState,
    row_hist: &[f32; 10],
    h: u8,
    c_a: u8,
    c_b: u8,
) -> f64 {
    if c_a == c_b {
        return 0.0;
    }
    let (c_min, c_max) = (c_a.min(c_b), c_a.max(c_b));
    let rows = if h == 0 { [1i8, 2, 3, 0] } else { [8i8, 7, 6, 9] };
    let mut blocked_rows = 0usize;
    let mut hist_sum = 0.0_f64;
    for r in rows {
        hist_sum += row_hist[r as usize] as f64;
        if board.get_available_track_for_row(r, c_min, c_max).is_none() {
            blocked_rows += 1;
        }
    }
    let sat_pen = match blocked_rows {
        4.. => 18.0,
        3 => 8.5,
        _ => (blocked_rows as f64) * 1.8,
    };
    sat_pen + hist_sum * 0.45
}

/// Đi 1 đoạn dây ngang trực tiếp từ `c1` sang `c2` trên nửa mặt bo `h` (`routeH` trong main.html)
#[allow(clippy::too_many_arguments)]
pub fn route_h(
    board: &mut BoardState,
    fps: &[Footprint],
    tag: u16,
    c1: u8,
    c2: u8,
    h: u8,
    forbid_mask: u16,
    rem_cross: i32,
    row_hist: &[f32; 10],
    weights: &CostWeights,
) -> Option<FastWire> {
    let w_net = base_net_of(tag);
    let (c_min, c_max) = (c1.min(c2), c1.max(c2));
    let all_rows = match (board.rev, h) {
        (true, 0) => [1i8, 2, 3, 0, 4],
        (true, _) => [8i8, 7, 6, 9, 5],
        (false, 0) => [3i8, 2, 1, 0, 4],
        (false, _) => [6i8, 7, 8, 9, 5],
    };

    let mut best_r: i8 = -1;
    let mut best_track: Option<(i8, usize)> = None;
    let mut best_cost = 1e9_f64;

    for (idx, &r) in all_rows.iter().enumerate() {
        if (forbid_mask & (1u16 << r)) != 0 || board.is_occupied(c1, r) || board.is_occupied(c2, r)
        {
            continue;
        }
        let Some(track) = get_available_track_with_btn_rule(board, fps, r, c_min, c_max, 0) else {
            continue;
        };
        let is_groove = r == 4 || r == 5;
        let bridge_destroy_pen = if rem_cross > 0
            && is_groove
            && (is_potential_bridge_col(board, fps, c1, tag, rem_cross)
                || is_potential_bridge_col(board, fps, c2, tag, rem_cross))
        {
            weights.route_bridge_destroy_pen
        } else {
            0.0
        };
        let groove_base = if rem_cross > 0 {
            weights.route_groove_pen_cross
        } else {
            weights.route_groove_pen_done
        };
        let edge_pen = if r == 0 || r == 9 {
            weights.route_outer_row_pen
        } else if is_groove {
            groove_base + bridge_destroy_pen
        } else {
            (idx as f64) * 2.5
        };
        let lane_pen = if track.0 == 0 { 0.0 } else { weights.route_gap_lane_pen };
        let cost = edge_pen + lane_pen + (row_hist[r as usize] as f64).min(5.0);
        if cost < best_cost {
            best_cost = cost;
            best_r = r;
            best_track = Some(track);
        }
    }

    let (lane, tid) = best_track?;
    board.occupy(c1, best_r);
    board.occupy(c2, best_r);
    board.occupy_segment(tid, c_min, c_max);
    Some(FastWire::new_h(w_net, c1, c2, best_r, lane, tid))
}

/// Trượt chân linh kiện rời (R, LED, C không khóa) tại lỗ `(col, r_old)` sang hàng liền kề `r_new`
/// Quy tắc Cứng #5: Độ lệch tối đa 1 hàng liền kề (`|r_new - r_old| <= 1` và `|r_new - orig_r| <= 1`)!
pub fn try_slide_spec_pin_1row(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    col: u8,
    r_old: i8,
    h: u8,
    forbid_row: Option<i8>,
    weights: &CostWeights,
) -> Option<i8> {
    let idx = pass_wires.spec_pins.iter().position(|s| {
        !s.locked && ((s.c1 == col && s.r1 == r_old) || (s.c2 == col && s.r2 == r_old))
    })?;
    let sp_item = pass_wires.spec_pins[idx];
    let is_pin1 = sp_item.c1 == col && sp_item.r1 == r_old;
    let (other_c, other_r) = if is_pin1 { (sp_item.c2, sp_item.r2) } else { (sp_item.c1, sp_item.r1) };
    let orig_r = if is_pin1 { sp_item.orig_r1 } else { sp_item.orig_r2 };
    let span_lim = SpecialSpan::for_type(sp_item.comp_type);

    let mut cands = [r_old - 1, r_old + 1];
    if (cands[1] == other_r) || (cands[0] == 4 || cands[0] == 5) {
        cands.swap(0, 1);
    }

    for r_new in cands {
        if is_rail_row(r_new)
            || half_of(r_new) != h
            || (r_new - r_old).abs() > 1
            || (r_new - orig_r).abs() > 1
            || forbid_row == Some(r_new)
            || board.is_occupied(col, r_new)
        {
            continue;
        }
        let (nc1, nr1, nc2, nr2) = if is_pin1 {
            (col, r_new, other_c, other_r)
        } else {
            (other_c, other_r, col, r_new)
        };
        let one_rail = is_rail_row(nr1) != is_rail_row(nr2);
        let dy = if one_rail {
            (vis_y(nr1) - vis_y(nr2)).abs()
        } else {
            (nr1 - nr2).abs() as f64
        };
        let d = ((nc1 as f64) - (nc2 as f64)).hypot(dy);
        if d < span_lim.min || d > span_lim.max {
            continue;
        }
        let (p1, p2) = ([nc1 as f64, vis_y(nr1)], [nc2 as f64, vis_y(nr2)]);
        let hits = pass_wires.spec_pins.iter().enumerate().any(|(j, o)| {
            j != idx
                && !o.locked
                && seg_dist_sq(p1, p2, [o.c1 as f64, vis_y(o.r1)], [o.c2 as f64, vis_y(o.r2)])
                    < weights.spec_body_clearance_sq
        });
        if !hits {
            board.unoccupy(col, r_old);
            board.occupy(col, r_new);
            pass_wires.record_pin_slide(idx, nr1, nr2);
            return Some(r_new);
        }
    }
    None
}

/// Di chuyển 1 dây chắn `victim`:
/// - Ưu tiên #1 (`allow_same_row = true`): Lách thân dây sang làn khoảng cách (`lane = ±1`) ngay trên cùng hàng `r_from` (tốn 0 lỗ cắm mới!) (#6)
/// - Ưu tiên #2: Dời sang hàng khác `r_alt` (hỗ trợ đổi chỗ 1 hàng với chân linh kiện rời theo Quy tắc Cứng #5)
#[allow(clippy::too_many_arguments)]
fn move_one_victim(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    fps: &[Footprint],
    v_idx: usize,
    r_from: i8,
    all_rows: &[i8; 5],
    allow_same_row: bool,
    forbid_hole: Option<(u8, i8)>,
    weights: &CostWeights,
) -> bool {
    let victim = pass_wires.wires[v_idx];
    if victim.trk == NO_TRACK {
        return false;
    }
    let (vc1, vc2) = (victim.c1, victim.c2);
    let (v_min, v_max) = (vc1.min(vc2), vc1.max(vc2));

    if allow_same_row {
        let gap_lanes = if r_from == 4 { [-1i8, 1, 0] } else { [1i8, -1, 0] };
        for alt_lane in gap_lanes {
            if alt_lane == victim.lane || !is_btn_lane_allowed(fps, r_from, alt_lane, v_min, v_max) {
                continue;
            }
            let Some(alt_tid) = track_index(r_from, alt_lane) else {
                continue;
            };
            if alt_tid != (victim.trk as usize) && board.is_segment_free(alt_tid, v_min, v_max) {
                board.unoccupy_segment(victim.trk as usize, v_min, v_max);
                board.occupy_segment(alt_tid, v_min, v_max);
                pass_wires.record_swap(
                    v_idx,
                    FastWire::new_h(victim.net, vc1, vc2, r_from, alt_lane, alt_tid),
                );
                return true;
            }
        }
    }

    let h = half_of(r_from);
    for &r_alt in all_rows {
        if r_alt == r_from {
            continue;
        }
        let Some((alt_lane, alt_tid)) =
            get_available_track_with_btn_rule(board, fps, r_alt, v_min, v_max, 0)
        else {
            continue;
        };
        let snap_b = *board;
        let snap_w = pass_wires.snapshot_swaps();
        board.unoccupy_segment(victim.trk as usize, v_min, v_max);
        board.unoccupy(vc1, r_from);
        board.unoccupy(vc2, r_from);

        let f1 = if !board.is_occupied(vc1, r_alt) {
            true
        } else if (r_alt - r_from).abs() <= 1 {
            let f_row = forbid_hole.and_then(|(fc, fr)| (fc == vc1).then_some(fr));
            try_slide_spec_pin_1row(board, pass_wires, vc1, r_alt, h, f_row, weights).is_some()
        } else {
            false
        };
        let f2 = f1
            && if !board.is_occupied(vc2, r_alt) {
                true
            } else if (r_alt - r_from).abs() <= 1 {
                let f_row = forbid_hole.and_then(|(fc, fr)| (fc == vc2).then_some(fr));
                try_slide_spec_pin_1row(board, pass_wires, vc2, r_alt, h, f_row, weights).is_some()
            } else {
                false
            };

        if f2 {
            board.occupy(vc1, r_alt);
            board.occupy(vc2, r_alt);
            board.occupy_segment(alt_tid, v_min, v_max);
            pass_wires.record_swap(
                v_idx,
                FastWire::new_h(victim.net, vc1, vc2, r_alt, alt_lane, alt_tid),
            );
            return true;
        }
        *board = snap_b;
        pass_wires.restore_swaps(snap_w);
    }
    false
}

fn free_endpoint_hole(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    fps: &[Footprint],
    col: u8,
    r: i8,
    h: u8,
    all_rows: &[i8; 5],
    weights: &CostWeights,
) -> Option<(Option<usize>, i8)> {
    if !board.is_occupied(col, r) {
        return Some((None, 0));
    }
    if let Some(r_new) = try_slide_spec_pin_1row(board, pass_wires, col, r, h, Some(r), weights) {
        return Some((None, (r_new - r).signum()));
    }
    if let Some(hv_idx) = pass_wires.wires.iter().position(|w| {
        w.trk != NO_TRACK && w.r1 == r && w.r2 == r && (w.c1 == col || w.c2 == col)
    }) {
        let old_trk = pass_wires.wires[hv_idx].trk as usize;
        if move_one_victim(board, pass_wires, fps, hv_idx, r, all_rows, false, Some((col, r)), weights)
            && !board.is_occupied(col, r)
        {
            return Some((Some(old_trk), 0));
        }
        return None;
    }
    for r_adj in [r - 1, r + 1] {
        if is_rail_row(r_adj) || half_of(r_adj) != h {
            continue;
        }
        let Some(w_idx) = pass_wires.wires.iter().position(|w| {
            w.trk != NO_TRACK && w.r1 == r_adj && w.r2 == r_adj && (w.c1 == col || w.c2 == col)
        }) else {
            continue;
        };
        let snap_b = *board;
        let snap_w = pass_wires.snapshot_swaps();
        if move_one_victim(board, pass_wires, fps, w_idx, r_adj, all_rows, false, Some((col, r)), weights) {
            if let Some(r_new) = try_slide_spec_pin_1row(board, pass_wires, col, r, h, Some(r), weights) {
                return Some((None, (r_new - r).signum()));
            }
        }
        *board = snap_b;
        pass_wires.restore_swaps(snap_w);
    }
    None
}

/// Thử dời dây ngang cũ sang làn phụ / hàng khác hoặc trượt chân linh kiện rời 1 hàng để nhường chỗ cho `[c1, c2]`
#[allow(clippy::too_many_arguments)]
pub fn try_row_swap_h_ext(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    fps: &[Footprint],
    tag: u16,
    c1: u8,
    c2: u8,
    h: u8,
    weights: &CostWeights,
) -> Option<FastWire> {
    if !pass_wires.swap_enabled || (pass_wires.wires.is_empty() && pass_wires.spec_pins.is_empty()) {
        return None;
    }
    let w_net = base_net_of(tag);
    let (c_min, c_max) = (c1.min(c2), c1.max(c2));
    let all_rows = if h == 0 { [1i8, 2, 3, 0, 4] } else { [8i8, 7, 6, 9, 5] };

    for &r in &all_rows {
        let (occ1, occ2) = (board.is_occupied(c1, r), board.is_occupied(c2, r));
        if !occ1 && !occ2 {
            let mut trk_order = [NO_TRACK; 3];
            let mut trk_cnt = 0usize;
            for w in &pass_wires.wires {
                if w.trk != NO_TRACK && w.r1 == r && w.r2 == r {
                    let (v_min, v_max) = (w.c1.min(w.c2), w.c1.max(w.c2));
                    if c_min.max(v_min) <= c_max.min(v_max) && !trk_order[..trk_cnt].contains(&w.trk)
                    {
                        if trk_cnt < 3 {
                            trk_order[trk_cnt] = w.trk;
                            trk_cnt += 1;
                        }
                    }
                }
            }
            for &tk in &trk_order[..trk_cnt] {
                let mut v_group = [0usize; 4];
                let mut v_len = 0usize;
                for (idx, w) in pass_wires.wires.iter().enumerate() {
                    if w.trk == tk && w.r1 == r && w.r2 == r {
                        let (v_min, v_max) = (w.c1.min(w.c2), w.c1.max(w.c2));
                        if c_min.max(v_min) <= c_max.min(v_max) {
                            if v_len < 4 {
                                v_group[v_len] = idx;
                            }
                            v_len += 1;
                        }
                    }
                }
                if v_len == 0 || v_len > 3 {
                    continue;
                }
                let snap_b = *board;
                let snap_w = pass_wires.snapshot_swaps();
                let all_moved = v_group[..v_len].iter().all(|&vi| {
                    move_one_victim(board, pass_wires, fps, vi, r, &all_rows, true, None, weights)
                });
                if all_moved {
                    if let Some((lane, tid)) =
                        get_available_track_with_btn_rule(board, fps, r, c_min, c_max, 0)
                    {
                        board.occupy(c1, r);
                        board.occupy(c2, r);
                        board.occupy_segment(tid, c_min, c_max);
                        return Some(FastWire::new_h(w_net, c1, c2, r, lane, tid));
                    }
                }
                *board = snap_b;
                pass_wires.restore_swaps(snap_w);
            }
        } else {
            let snap_b = *board;
            let snap_w = pass_wires.snapshot_swaps();
            let Some((old_t1, dir1)) =
                free_endpoint_hole(board, pass_wires, fps, c1, r, h, &all_rows, weights)
            else {
                *board = snap_b;
                pass_wires.restore_swaps(snap_w);
                continue;
            };
            let Some((old_t2, dir2)) =
                free_endpoint_hole(board, pass_wires, fps, c2, r, h, &all_rows, weights)
            else {
                *board = snap_b;
                pass_wires.restore_swaps(snap_w);
                continue;
            };
            let pref_dir = if dir1 != 0 { dir1 } else { dir2 };
            if !board.is_occupied(c1, r) && !board.is_occupied(c2, r) {
                let mut new_track =
                    get_available_track_with_btn_rule(board, fps, r, c_min, c_max, pref_dir);
                if new_track.is_none() {
                    let target_trk = old_t1.or(old_t2).map(|t| t as u8);
                    let mut blockers = [0usize; 3];
                    let mut b_len = 0usize;
                    for (idx, w) in pass_wires.wires.iter().enumerate() {
                        if w.trk != NO_TRACK
                            && w.r1 == r
                            && w.r2 == r
                            && target_trk.is_none_or(|tk| w.trk == tk)
                        {
                            let (v_min, v_max) = (w.c1.min(w.c2), w.c1.max(w.c2));
                            if c_min.max(v_min) <= c_max.min(v_max) {
                                if b_len < 3 {
                                    blockers[b_len] = idx;
                                }
                                b_len += 1;
                            }
                        }
                    }
                    if b_len > 0
                        && b_len <= 2
                        && blockers[..b_len].iter().all(|&bi| {
                            move_one_victim(board, pass_wires, fps, bi, r, &all_rows, true, None, weights)
                        })
                    {
                        new_track =
                            get_available_track_with_btn_rule(board, fps, r, c_min, c_max, pref_dir);
                    }
                }
                if let Some((lane, tid)) = new_track {
                    board.occupy(c1, r);
                    board.occupy(c2, r);
                    board.occupy_segment(tid, c_min, c_max);
                    return Some(FastWire::new_h(w_net, c1, c2, r, lane, tid));
                }
            }
            *board = snap_b;
            pass_wires.restore_swaps(snap_w);
        }
    }
    None
}

/// Thử dời dây ngang cũ sang hàng khác để nhường chỗ cho đoạn `[c1, c2]` (`tryRowSwapH`)
pub fn try_row_swap_h(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    tag: u16,
    c1: u8,
    c2: u8,
    h: u8,
) -> Option<FastWire> {
    try_row_swap_h_ext(board, pass_wires, &[], tag, c1, c2, h, &CostWeights::DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::circuit::PlacedSpecial;

    #[test]
    fn test_route_h_and_try_row_swap_h() {
        let mut board = BoardState::new(false);
        let fps = Vec::new();
        let row_hist = [0.0f32; 10];
        let weights = CostWeights::default();
        let mut pass_wires = PassWires::new(true);

        let w1 = route_h(&mut board, &fps, 3, 5, 15, 0, 0, 0, &row_hist, &weights).unwrap();
        assert_eq!(w1.r1, 3);
        assert_eq!(w1.lane, 0);
        pass_wires.wires.push(w1);

        for r in [0, 1, 2, 4] {
            board.occupy(5, r);
        }
        assert!(route_h(&mut board, &fps, 4, 5, 12, 0, 0, 0, &row_hist, &weights).is_none());

        board.unoccupy(5, 1);
        board.occupy(12, 1);
        let w2 = try_row_swap_h(&mut board, &mut pass_wires, 4, 5, 12, 0).unwrap();
        assert_eq!(w2.r1, 3);
        assert_eq!(pass_wires.wires[0].r1, 1);
    }

    #[test]
    fn test_same_row_gap_lane_shift_zero_new_holes_and_btn_rule() {
        let mut board = BoardState::new(false);
        let mut pass_wires = PassWires::new(true);
        let weights = CostWeights::default();

        // 1. Dây victim w1 (10..15) đang nằm ở hàng 3, lane = 0
        let tid_r3_l0 = track_index(3, 0).unwrap();
        board.occupy(10, 3);
        board.occupy(15, 3);
        board.occupy_segment(tid_r3_l0, 10, 15);
        pass_wires.wires.push(FastWire::new_h(NetId(3), 10, 15, 3, 0, tid_r3_l0));

        // Khóa sạch các hàng 0, 1, 2, 4 tại cột 10, 15, 2, 12 -> victim KHÔNG THỂ dời sang hàng khác!
        for c in [2, 10, 12, 15] {
            for r in [0, 1, 2, 4] {
                board.occupy(c, r);
            }
        }
        // Chặn làn phụ (+1 và -1) của hàng 3 trên đoạn [2..4] để dây mới (2..12) bắt buộc cần lane = 0 của hàng 3
        board.occupy_segment(track_index(3, 1).unwrap(), 2, 4);
        board.occupy_segment(track_index(3, -1).unwrap(), 2, 4);

        // try_row_swap_h tự động lách thân w1 (10..15) sang làn khoảng cách (lane = +1) ngay trên hàng 3 (tốn 0 lỗ mới!)
        let w_new = try_row_swap_h(&mut board, &mut pass_wires, 4, 2, 12, 0).unwrap();
        assert_eq!((w_new.r1, w_new.lane), (3, 0));
        assert_eq!((pass_wires.wires[0].r1, pass_wires.wires[0].lane), (3, 1));

        // 2. Kiểm tra quy tắc cấm lane = 0 tại lỗ bẻ chân BTN2 (TH3: bend = +1 tại B, 1..=3 -> cấm lane = 0 trên hàng C=2)
        let fps = vec![Footprint::for_btn_bent(1, 3, 1, 1)];
        assert!(!is_btn_lane_allowed(&fps, 2, 0, 3, 8));
        assert!(is_btn_lane_allowed(&fps, 2, 1, 3, 8));
        assert!(is_btn_lane_allowed(&fps, 0, 0, 3, 8));
        let _ = weights;
    }

    #[test]
    fn test_try_slide_spec_pin_1row_hard_limit() {
        let weights = CostWeights::default();
        let mut layout = BoardLayout::default();
        layout.rs.push(PlacedSpecial {
            cid: CompId(1),
            id: "R1".into(),
            comp_type: CompType::R,
            c1: 5,
            r1: 2,
            c2: 10,
            r2: 2,
            locked: false,
            ic_id: None,
        });
        let mut board = BoardState::new(false);
        let mut fps = Vec::new();
        layout.block_space(&mut board, &mut fps);
        let mut pass_wires = PassWires::with_layout(true, &layout);

        // Cột 5 chỉ trống ở hàng 1 (|1 - 2| == 1), cột 12 chỉ trống ở hàng 2 -> R1 trượt 1 hàng từ (5,2) sang (5,1) để nhường (5,2) cho dây!
        for r in [0, 3, 4] {
            board.occupy(5, r);
        }
        for r in [0, 1, 3, 4] {
            board.occupy(12, r);
        }
        let w = try_row_swap_h_ext(&mut board, &mut pass_wires, &fps, 3, 5, 12, 0, &weights).unwrap();
        assert_eq!(w.r1, 2);
        pass_wires.apply_pin_slides(&mut layout);
        assert_eq!(layout.rs[0].r1, 1);

        // Quy tắc Cứng #5: Không bao giờ cho phép trượt tiếp từ hàng 1 sang hàng 0 vì |0 - orig_r1(2)| = 2 > 1!
        board.unoccupy(5, 0);
        assert!(try_slide_spec_pin_1row(&mut board, &mut pass_wires, 5, 1, 0, None, &weights).is_none());
    }
}
