use crate::cleanup::slide_absorb::{
    absorb_redundant_wires_into_resistors, init_cleanup_board, merge_dogleg_wires,
    push_vertical_specials_to_edge, restore_spec_coords, snapshot_spec_coords,
    try_slide_pin_at_hole,
};
use crate::config::weights::CostWeights;
use crate::model::bitboard::{half_of, is_rail_row, track_index, BoardState};
use crate::model::circuit::BoardLayout;
use crate::routing::h_route::FastWire;

#[derive(Clone, Copy, Debug)]
struct VcgWireMeta {
    w_idx: usize,
    c1: u8,
    c2: u8,
    c_min: u8,
    c_max: u8,
    span: u8,
    vcg_depth: usize,
    has_groove_anchor: bool,
    has_outer_anchor: bool,
}

fn eval_half_planar_cost(
    wires: &[FastWire],
    h_indices: &[usize],
    h: u8,
    weights: &CostWeights,
) -> f64 {
    let mut cost = 0.0;
    for i in 0..h_indices.len() {
        let wi = wires[h_indices[i]];
        let ri = wi.r1;
        let (l1, r1) = (wi.c1.min(wi.c2), wi.c1.max(wi.c2));
        let dist_i = if h == 0 { 4 - ri } else { ri - 5 };
        if wi.lane != 0 {
            cost += weights.bvcg_gap_lane_pen;
        }
        if ri == 0 || ri == 9 {
            cost += weights.bvcg_outer_row_pen;
        }
        if ri == 4 || ri == 5 {
            cost += weights.bvcg_groove_row_pen;
        }
        for &wj_idx in &h_indices[(i + 1)..] {
            let wj = wires[wj_idx];
            let rj = wj.r1;
            let (l2, r2) = (wj.c1.min(wj.c2), wj.c1.max(wj.c2));
            let dist_j = if h == 0 { 4 - rj } else { rj - 5 };
            if (l2 <= l1 && r1 <= r2 && (r2 - l2) > (r1 - l1) && dist_j < dist_i)
                || (l1 <= l2 && r2 <= r1 && (r1 - l1) > (r2 - l2) && dist_i < dist_j)
            {
                cost += weights.bvcg_inversion_pen;
            }
            if wi.net == wj.net && (l1 == r2 || r1 == l2 || l1 == l2 || r1 == r2) {
                cost += ((ri - rj).abs() as f64) * weights.bvcg_step_row_weight;
            }
        }
    }
    cost
}

/// PHÉP 4: Tối ưu Đồ thị Ràng buộc Dọc (B-VCG) + Left-Edge Track Repacking trên từng nửa bo
pub fn repack_half_planar_bvcg(
    layout: &mut BoardLayout,
    wires: &mut Vec<FastWire>,
    board: &mut BoardState,
    weights: &CostWeights,
) {
    for h in [0u8, 1u8] {
        let h_indices: Vec<usize> = wires
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                w.r1 == w.r2 && w.c1 != w.c2 && !is_rail_row(w.r1) && half_of(w.r1) == h
            })
            .map(|(idx, _)| idx)
            .collect();
        if h_indices.len() < 2 {
            continue;
        }

        let cost_before = eval_half_planar_cost(wires, &h_indices, h, weights);
        let snap_wires = wires.clone();
        let snap_spec = snapshot_spec_coords(layout);
        let snap_board = *board;

        for &w_idx in &h_indices {
            let w = wires[w_idx];
            if let Some(tid) = track_index(w.r1, w.lane) {
                board.unoccupy_segment(tid, w.c1, w.c2);
            }
            board.unoccupy(w.c1, w.r1);
            board.unoccupy(w.c2, w.r1);
        }

        let groove_r = if h == 0 { 4 } else { 5 };
        let outer_r = if h == 0 { 0 } else { 9 };
        let mut meta: Vec<VcgWireMeta> = h_indices
            .iter()
            .map(|&w_idx| {
                let w = wires[w_idx];
                let (c1, c2) = (w.c1, w.c2);
                let (c_min, c_max) = (c1.min(c2), c1.max(c2));
                let span = c_max - c_min;
                let vcg_depth = h_indices
                    .iter()
                    .filter(|&&o_idx| {
                        if o_idx == w_idx {
                            return false;
                        }
                        let ow = wires[o_idx];
                        let (o_min, o_max) = (ow.c1.min(ow.c2), ow.c1.max(ow.c2));
                        o_min >= c_min && o_max <= c_max && (o_max - o_min) < span
                    })
                    .count();
                VcgWireMeta {
                    w_idx,
                    c1,
                    c2,
                    c_min,
                    c_max,
                    span,
                    vcg_depth,
                    has_groove_anchor: board.is_occupied(c1, groove_r)
                        || board.is_occupied(c2, groove_r),
                    has_outer_anchor: board.is_occupied(c1, outer_r)
                        || board.is_occupied(c2, outer_r),
                }
            })
            .collect();

        meta.sort_by_key(|m| {
            let tier = if m.has_outer_anchor && !m.has_groove_anchor {
                2
            } else {
                m.vcg_depth.min(2)
            };
            (tier, m.span, m.c_min)
        });

        let mut all_assigned = true;
        for m in &meta {
            let row_order = if m.has_outer_anchor && !m.has_groove_anchor {
                if h == 0 { [1i8, 2, 3, 0, 4] } else { [8i8, 7, 6, 9, 5] }
            } else if m.vcg_depth == 0 && m.span <= 8 {
                if h == 0 { [3i8, 2, 1, 0, 4] } else { [6i8, 7, 8, 9, 5] }
            } else if m.vcg_depth <= 1 && m.span <= 16 {
                if h == 0 { [2i8, 3, 1, 0, 4] } else { [7i8, 6, 8, 9, 5] }
            } else if h == 0 {
                [1i8, 2, 3, 0, 4]
            } else {
                [8i8, 7, 6, 9, 5]
            };

            let mut placed = false;
            for allow_gap_lane in [false, true] {
                if placed {
                    break;
                }
                for r_try in row_order {
                    let lanes: &[i8] = if !allow_gap_lane {
                        &[0]
                    } else if r_try == 4 {
                        &[-1, 1]
                    } else {
                        &[1, -1]
                    };
                    let Some((ln, tid)) = lanes.iter().find_map(|&ln| {
                        let tid = track_index(r_try, ln)?;
                        board.is_segment_free(tid, m.c_min, m.c_max).then_some((ln, tid))
                    }) else {
                        continue;
                    };

                    let pin_snap = snapshot_spec_coords(layout);
                    let b_local = *board;
                    let f1 = !board.is_occupied(m.c1, r_try)
                        || try_slide_pin_at_hole(layout, board, m.c1, r_try, h, weights);
                    let f2 = f1
                        && (!board.is_occupied(m.c2, r_try)
                            || try_slide_pin_at_hole(layout, board, m.c2, r_try, h, weights));
                    if f1 && f2 {
                        board.occupy(m.c1, r_try);
                        board.occupy(m.c2, r_try);
                        board.occupy_segment(tid, m.c_min, m.c_max);
                        let net = wires[m.w_idx].net;
                        wires[m.w_idx] = FastWire::new_h(net, m.c1, m.c2, r_try, ln, tid);
                        placed = true;
                        break;
                    }
                    *board = b_local;
                    restore_spec_coords(layout, &pin_snap);
                }
            }
            if !placed {
                all_assigned = false;
                break;
            }
        }

        let cost_after = if all_assigned {
            eval_half_planar_cost(wires, &h_indices, h, weights)
        } else {
            1e9
        };
        if !all_assigned || cost_after > cost_before {
            *wires = snap_wires;
            restore_spec_coords(layout, &snap_spec);
            *board = snap_board;
        }
    }
}

/// Thực thi trọn vẹn 4 Phép Hậu kiểm Chiến thuật (`DiscreteEngine.runQuiescenceCleanup` - chỉ chạy khi đạt 0 lỗi)
pub fn run_quiescence_cleanup(
    layout: &mut BoardLayout,
    wires: &mut Vec<FastWire>,
    weights: &CostWeights,
) {
    let mut board = init_cleanup_board(layout, wires);
    push_vertical_specials_to_edge(layout, &mut board, weights);
    merge_dogleg_wires(layout, wires, &mut board, weights);
    absorb_redundant_wires_into_resistors(layout, wires, &mut board, weights);
    repack_half_planar_bvcg(layout, wires, &mut board, weights);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::circuit::PlacedSpecial;
    use crate::model::ids::{CompId, CompType, NetId};

    #[test]
    fn test_full_quiescence_cleanup_all_4_passes() {
        let weights = CostWeights::default();
        let mut layout = BoardLayout::default();
        layout.leds.push(PlacedSpecial {
            cid: CompId(1),
            id: "L1".into(),
            comp_type: CompType::LED,
            c1: 5,
            r1: -2,
            c2: 5,
            r2: 2,
            locked: false,
            ic_id: None,
        });
        layout.rs.push(PlacedSpecial {
            cid: CompId(2),
            id: "R1".into(),
            comp_type: CompType::R,
            c1: 20,
            r1: 2,
            c2: 25,
            r2: 2,
            locked: false,
            ic_id: None,
        });

        let mut wires = vec![
            // Dogleg (10,3)->(14,3) và (14,2)->(18,2) -> Gộp thành 1 dây (10..18)
            FastWire::new_h(NetId(3), 10, 14, 3, 0, 3),
            FastWire::new_h(NetId(3), 14, 18, 2, 0, 2),
            // Dây nối thừa (25,1)->(28,1) -> Hút vào chân R1 tại hàng cố định 2: (20,2)->(28,2)
            FastWire::new_h(NetId(4), 25, 28, 1, 0, 1),
            // Dây ngắn (12..16) bị đặt ở hàng 1 (ngoài) trong khi dây dài (10..18) chiếm hàng 3 (sát IC) -> Phép 4 B-VCG đảo lại!
            FastWire::new_h(NetId(5), 12, 16, 1, 0, 1),
        ];

        run_quiescence_cleanup(&mut layout, &mut wires, &weights);

        assert_eq!(layout.leds[0].r2, 0); // Phép 1: L1 đẩy sát mép hàng 0
        assert_eq!((layout.rs[0].c1, layout.rs[0].c2), (20, 28)); // Phép 3: R1 hút dây thừa
        assert_eq!((layout.rs[0].r1, layout.rs[0].r2), (2, 2));   // Giữ nguyên hàng cố định 2
        assert_eq!(wires.len(), 2); // Phép 2 + 3: 4 dây ban đầu giảm còn 2 dây

        // Phép 4 (B-VCG): Dây ngắn (12..16) được xếp vào hàng 3 (sát rãnh IC hơn dây dài 10..18 ở hàng 2)
        let w_long = wires.iter().find(|w| w.c1 == 10 && w.c2 == 18).unwrap();
        let w_short = wires.iter().find(|w| w.c1 == 12 && w.c2 == 16).unwrap();
        assert!(w_short.r1 > w_long.r1);
    }
}
