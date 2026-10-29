use crate::config::weights::{CostWeights, SpecialSpan, SPECIAL_R};
use crate::model::bitboard::{half_of, is_rail_row, track_index, BoardState};
use crate::model::circuit::{BoardLayout, PlacedSpecial};
use crate::model::geometry::{seg_dist_sq, vis_y};
use crate::model::ids::CompId;
use crate::routing::h_route::{FastWire, NO_TRACK};

pub type SpecCoordSnap = (CompId, u8, i8, u8, i8);

/// Khởi tạo `BoardState` sạch trên Stack phục vụ hậu kiểm (`runQuiescenceCleanup` trong discrete-engine.js)
pub fn init_cleanup_board(layout: &BoardLayout, wires: &mut [FastWire]) -> BoardState {
    let mut board = BoardState::new(false);
    let mut fps = Vec::new();
    layout.block_space(&mut board, &mut fps);
    for w in wires.iter_mut() {
        board.occupy(w.c1, w.r1);
        board.occupy(w.c2, w.r2);
        if w.r1 == w.r2 && w.c1 != w.c2 && !is_rail_row(w.r1) {
            if let Some(tid) = track_index(w.r1, w.lane) {
                board.occupy_segment(tid, w.c1, w.c2);
                w.trk = tid as u8;
            }
        }
    }
    board
}

pub fn snapshot_spec_coords(layout: &BoardLayout) -> Vec<SpecCoordSnap> {
    layout.cs.iter().chain(&layout.leds).chain(&layout.rs).map(|s| (s.cid, s.c1, s.r1, s.c2, s.r2)).collect()
}

pub fn restore_spec_coords(layout: &mut BoardLayout, snap: &[SpecCoordSnap]) {
    for (s, &(_, c1, r1, c2, r2)) in layout.cs.iter_mut().chain(&mut layout.leds).chain(&mut layout.rs).zip(snap) {
        s.c1 = c1;
        s.r1 = r1;
        s.c2 = c2;
        s.r2 = r2;
    }
}

/// Kiểm tra dịch chuyển chân linh kiện rời hợp lệ (Hạng mục #1: tính `dy` qua `vis_y` khi 1 chân ở thanh nguồn)
#[allow(clippy::too_many_arguments)]
pub fn can_move_spec_pin(
    layout: &BoardLayout,
    target_cid: CompId,
    target_locked: bool,
    sp: SpecialSpan,
    new_c1: u8,
    new_r1: i8,
    new_c2: u8,
    new_r2: i8,
    weights: &CostWeights,
) -> bool {
    if target_locked {
        return false;
    }
    let one_rail = is_rail_row(new_r1) != is_rail_row(new_r2);
    let dy = if one_rail {
        (vis_y(new_r1) - vis_y(new_r2)).abs()
    } else {
        (new_r1 - new_r2).abs() as f64
    };
    let d = ((new_c1 as f64) - (new_c2 as f64)).hypot(dy);
    if d < sp.min || d > sp.max {
        return false;
    }
    let (p1, p2) = ([new_c1 as f64, vis_y(new_r1)], [new_c2 as f64, vis_y(new_r2)]);
    !layout.cs.iter().chain(&layout.leds).chain(&layout.rs).any(|obj| {
        obj.cid != target_cid
            && !obj.locked
            && seg_dist_sq(p1, p2, [obj.c1 as f64, vis_y(obj.r1)], [obj.c2 as f64, vis_y(obj.r2)])
                < weights.spec_body_clearance_sq
    })
}

fn get_spec_mut(layout: &mut BoardLayout, idx: usize) -> Option<&mut PlacedSpecial> {
    let (cs_len, leds_len) = (layout.cs.len(), layout.leds.len());
    if idx < cs_len {
        Some(&mut layout.cs[idx])
    } else if idx < cs_len + leds_len {
        Some(&mut layout.leds[idx - cs_len])
    } else {
        layout.rs.get_mut(idx - cs_len - leds_len)
    }
}

/// PHÉP 1: Đẩy Tụ lọc nhánh (không khóa), LED và Trở nguồn thẳng đứng về sát mép Hàng 0 / Hàng 9
pub fn push_vertical_specials_to_edge(layout: &mut BoardLayout, board: &mut BoardState, weights: &CostWeights) {
    let total = layout.cs.len() + layout.leds.len() + layout.rs.len();
    for idx in 0..total {
        let obj = get_spec_mut(layout, idx).unwrap();
        let (cid, locked, comp_type, c1, r1, c2, r2) = (obj.cid, obj.locked, obj.comp_type, obj.c1, obj.r1, obj.c2, obj.r2);
        if locked || c1 != c2 {
            continue;
        }
        let sp = SpecialSpan::for_type(comp_type);
        if is_rail_row(r1) && !is_rail_row(r2) {
            let tr = if r2 <= 4 { 0 } else { 9 };
            if r2 != tr && !board.is_occupied(c1, tr) && can_move_spec_pin(layout, cid, locked, sp, c1, r1, c1, tr, weights) {
                board.unoccupy(c1, r2);
                board.occupy(c1, tr);
                get_spec_mut(layout, idx).unwrap().r2 = tr;
            }
        } else if is_rail_row(r2) && !is_rail_row(r1) {
            let tr = if r1 <= 4 { 0 } else { 9 };
            if r1 != tr && !board.is_occupied(c1, tr) && can_move_spec_pin(layout, cid, locked, sp, c1, tr, c1, r2, weights) {
                board.unoccupy(c1, r1);
                board.occupy(c1, tr);
                get_spec_mut(layout, idx).unwrap().r1 = tr;
            }
        }
    }
}

/// Trượt chân R/C/LED tại lỗ `(col, row)` sang hàng liền kề trên cùng nửa `h`
/// Quy tắc Cứng #5: Chỉ cho phép trượt chân linh kiện rời tối đa 1 hàng liền kề (`(r_alt - row).abs() <= 1`)!
pub fn try_slide_pin_at_hole(
    layout: &mut BoardLayout,
    board: &mut BoardState,
    col: u8,
    row: i8,
    h: u8,
    weights: &CostWeights,
) -> bool {
    let Some((idx, cid, comp_type, is_pin1, other_c, other_r)) = layout
        .cs.iter().chain(&layout.leds).chain(&layout.rs).enumerate()
        .find(|(_, o)| !o.locked && ((o.c1 == col && o.r1 == row) || (o.c2 == col && o.r2 == row)))
        .map(|(i, o)| {
            let p1 = o.c1 == col && o.r1 == row;
            (i, o.cid, o.comp_type, p1, if p1 { o.c2 } else { o.c1 }, if p1 { o.r2 } else { o.r1 })
        })
    else {
        return false;
    };
    let sp = SpecialSpan::for_type(comp_type);
    let other_is_rail = is_rail_row(other_r);
    let mut cand_rows = if h == 0 { [2i8, 1, 0, 3, 4] } else { [7i8, 8, 9, 6, 5] };
    cand_rows.sort_by(|&ra, &rb| {
        let dya = if other_is_rail { (vis_y(ra) - vis_y(other_r)).abs() } else { (ra - other_r).abs() as f64 };
        let dyb = if other_is_rail { (vis_y(rb) - vis_y(other_r)).abs() } else { (rb - other_r).abs() as f64 };
        let da = ((col as f64) - (other_c as f64)).hypot(dya) + if col == other_c || ra == other_r { -5.0 } else { 0.0 };
        let db = ((col as f64) - (other_c as f64)).hypot(dyb) + if col == other_c || rb == other_r { -5.0 } else { 0.0 };
        da.total_cmp(&db)
    });
    let was_ortho = col == other_c || row == other_r;
    for r_alt in cand_rows {
        if r_alt == row || (r_alt - row).abs() > 1 || board.is_occupied(col, r_alt) {
            continue;
        }
        let (nc1, nr1, nc2, nr2) = if is_pin1 { (col, r_alt, other_c, other_r) } else { (other_c, other_r, col, r_alt) };
        let is_ortho_after = nc1 == nc2 || nr1 == nr2;
        if (!was_ortho || is_ortho_after) && can_move_spec_pin(layout, cid, false, sp, nc1, nr1, nc2, nr2, weights) {
            board.unoccupy(col, row);
            board.occupy(col, r_alt);
            let obj = get_spec_mut(layout, idx).unwrap();
            if is_pin1 { obj.r1 = r_alt; } else { obj.r2 = r_alt; }
            return true;
        }
    }
    false
}

/// PHÉP 2: Gộp 2 dây ngang gấp khúc Dogleg cùng mạng thành 1 dây thẳng (`mergeAny`)
pub fn merge_dogleg_wires(layout: &mut BoardLayout, wires: &mut Vec<FastWire>, board: &mut BoardState, weights: &CostWeights) {
    let mut merged_any = true;
    while merged_any {
        merged_any = false;
        let mut i = 0usize;
        while i < wires.len() && !merged_any {
            let w1 = wires[i];
            if w1.r1 != w1.r2 || w1.c1 == w1.c2 || is_rail_row(w1.r1) {
                i += 1;
                continue;
            }
            let h = half_of(w1.r1);
            let mut j = i + 1;
            while j < wires.len() && !merged_any {
                let w2 = wires[j];
                if w2.net != w1.net || w2.r1 != w2.r2 || w2.c1 == w2.c2 || is_rail_row(w2.r1) || half_of(w2.r1) != h {
                    j += 1;
                    continue;
                }
                let shared = if w1.c1 == w2.c1 { Some((w1.c1, w1.c2, w2.c2)) }
                    else if w1.c1 == w2.c2 { Some((w1.c1, w1.c2, w2.c1)) }
                    else if w1.c2 == w2.c1 { Some((w1.c2, w1.c1, w2.c2)) }
                    else if w1.c2 == w2.c2 { Some((w1.c2, w1.c1, w2.c1)) }
                    else { None };
                let Some((cm, c1, c2)) = shared.filter(|&(_, a, b)| a != b) else { j += 1; continue; };
                if 5 - board.free_hole_count(cm, h) != 2 { j += 1; continue; }

                let (r1, r2) = (w1.r1, w2.r1);
                if let Some(t1) = track_index(r1, w1.lane) { board.unoccupy_segment(t1, w1.c1, w1.c2); }
                if let Some(t2) = track_index(r2, w2.lane) { board.unoccupy_segment(t2, w2.c1, w2.c2); }
                board.unoccupy(w1.c1, r1); board.unoccupy(w1.c2, r1);
                board.unoccupy(w2.c1, r2); board.unoccupy(w2.c2, r2);

                let base_rows = if h == 0 { [3i8, 2, 1, 0] } else { [6i8, 7, 8, 9] };
                let (mut try_rows, mut tr_len) = ([0i8; 6], 0usize);
                for &cr in [r1, r2].iter().chain(&base_rows) {
                    if !try_rows[..tr_len].contains(&cr) { try_rows[tr_len] = cr; tr_len += 1; }
                }
                let mut done = false;
                for &r_try in &try_rows[..tr_len] {
                    let Some((lane, tid)) = board.get_available_track_for_row(r_try, c1, c2) else { continue; };
                    let (snap_b, snap_pins) = (*board, snapshot_spec_coords(layout));
                    let f1 = !board.is_occupied(c1, r_try) || try_slide_pin_at_hole(layout, board, c1, r_try, h, weights);
                    let f2 = f1 && (!board.is_occupied(c2, r_try) || try_slide_pin_at_hole(layout, board, c2, r_try, h, weights));
                    if f1 && f2 {
                        board.occupy(c1, r_try); board.occupy(c2, r_try);
                        board.occupy_segment(tid, c1, c2);
                        wires[i] = FastWire::new_h(w1.net, c1, c2, r_try, lane, tid);
                        wires.remove(j);
                        done = true; merged_any = true;
                        break;
                    }
                    *board = snap_b;
                    restore_spec_coords(layout, &snap_pins);
                }
                if !done {
                    board.occupy(w1.c1, r1); board.occupy(w1.c2, r1);
                    board.occupy(w2.c1, r2); board.occupy(w2.c2, r2);
                    if let Some(t1) = track_index(r1, w1.lane) { board.occupy_segment(t1, w1.c1, w1.c2); }
                    if let Some(t2) = track_index(r2, w2.lane) { board.occupy_segment(t2, w2.c1, w2.c2); }
                }
                j += 1;
            }
            i += 1;
        }
    }
}

/// PHÉP 3: Hút dây nối ngang thừa vào chân Điện trở (`L.rs`)
pub fn absorb_redundant_wires_into_resistors(layout: &mut BoardLayout, wires: &mut Vec<FastWire>, board: &mut BoardState, weights: &CostWeights) {
    for i in (0..wires.len()).rev() {
        let w = wires[i];
        if w.net.is_power() || w.r1 != w.r2 || w.c1 == w.c2 || is_rail_row(w.r1) { continue; }
        let (r_w, h) = (w.r1, half_of(w.r1));
        for (c_relay, c_dest) in [(w.c1, w.c2), (w.c2, w.c1)] {
            if 5 - board.free_hole_count(c_relay, h) != 2 { continue; }
            let Some(r_idx) = layout.rs.iter().position(|rx| {
                !rx.locked && ((rx.c1 == c_relay && half_of(rx.r1) == h) || (rx.c2 == c_relay && half_of(rx.r2) == h))
            }) else { continue; };
            let rx = &layout.rs[r_idx];
            let (rx_cid, move_pin1) = (rx.cid, rx.c1 == c_relay && half_of(rx.r1) == h);
            let (fixed_c, fixed_r) = if move_pin1 { (rx.c2, rx.r2) } else { (rx.c1, rx.r1) };
            let old_r = if move_pin1 { rx.r1 } else { rx.r2 };
            let span = (c_dest as i32 - fixed_c as i32).abs();
            if is_rail_row(fixed_r) || half_of(fixed_r) != h || !(3..=12).contains(&span) { continue; }
            let cand_rows: &[i8] = if fixed_r == r_w { &[r_w] } else { &[fixed_r, r_w] };
            let mut absorbed = false;
            for &t_r in cand_rows {
                if (t_r != r_w && board.is_occupied(c_dest, t_r)) || (t_r != fixed_r && board.is_occupied(fixed_c, t_r)) {
                    continue;
                }
                let (nc1, nr1, nc2, nr2) = if move_pin1 { (c_dest, t_r, fixed_c, t_r) } else { (fixed_c, t_r, c_dest, t_r) };
                if can_move_spec_pin(layout, rx_cid, false, SPECIAL_R, nc1, nr1, nc2, nr2, weights) {
                    if let Some(tid) = track_index(r_w, w.lane).or_else(|| (w.trk != NO_TRACK).then_some(w.trk as usize)) {
                        board.unoccupy_segment(tid, w.c1, w.c2);
                    }
                    board.unoccupy(w.c1, r_w); board.unoccupy(w.c2, r_w); board.unoccupy(c_relay, old_r);
                    if t_r != fixed_r { board.unoccupy(fixed_c, fixed_r); board.occupy(fixed_c, t_r); }
                    board.occupy(c_dest, t_r);
                    let r_mut = &mut layout.rs[r_idx];
                    (r_mut.c1, r_mut.r1, r_mut.c2, r_mut.r2) = (nc1, nr1, nc2, nr2);
                    wires.remove(i);
                    absorbed = true;
                    break;
                }
            }
            if absorbed { break; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ids::CompType;

    #[test]
    fn test_vis_y_pullup_resistor_and_hard_1row_slide_rule() {
        let weights = CostWeights::default();
        let mut layout = BoardLayout::default();
        // Điện trở kéo nguồn R1 cắm dọc từ thanh nguồn VCC (-1) xuống hàng C (row = 2)
        layout.rs.push(PlacedSpecial {
            cid: CompId(1),
            id: "R1".into(),
            comp_type: CompType::R,
            c1: 10,
            r1: -1,
            c2: 10,
            r2: 2,
            locked: false,
            ic_id: None,
        });
        let mut wires = Vec::new();
        let mut board = init_cleanup_board(&layout, &mut wires);

        // Hạng mục #1: Nhờ dy = |vis_y(-1) - vis_y(0)| = 3.0 >= SPECIAL_R.min (3.0), R1 đẩy sát mép hàng A (row = 0)
        push_vertical_specials_to_edge(&mut layout, &mut board, &weights);
        assert_eq!(layout.rs[0].r2, 0);

        // Quy tắc Cứng #5: Khóa hàng 1 (hàng liền kề duy nhất của row = 0), chỉ mở hàng 2, 3, 4 -> Cấm trượt quá 1 hàng!
        board.occupy(10, 1);
        assert!(!try_slide_pin_at_hole(&mut layout, &mut board, 10, 0, 0, &weights));
        assert_eq!(layout.rs[0].r2, 0);

        // Khi mở hàng 1 (|1 - 0| == 1), cho phép trượt chân R1 từ hàng 0 sang hàng 1
        board.unoccupy(10, 1);
        assert!(try_slide_pin_at_hole(&mut layout, &mut board, 10, 0, 0, &weights));
        assert_eq!(layout.rs[0].r2, 1);
    }
}
