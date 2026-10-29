use crate::config::weights::CostWeights;
use crate::macro_place::topology::TopologyCache;
use crate::model::bitboard::{half_of, rail_ok, DEFAULT_POWER_HOLES};
use crate::model::circuit::{BoardLayout, ParsedNetlist};
use crate::model::ids::{NetId, PinRef};
use rustc_hash::FxHashMap;

/// Đánh giá chi phí đặt chân nguồn `VCC`/`GND` của SW/BTN2 tại cột `col` thuộc nửa bo `half` (#2, #3):
/// - Phạt nặng nếu `col` rơi trúng cột nghỉ của thanh nguồn Rail (`!rail_ok(col)`, tức `(c - 3) % 6 == 5`)
/// - Phạt nếu lỗ cắm thẳng trên thanh nguồn bị chiếm bởi 12 lỗ dây nguồn mặc định (`DEFAULT_POWER_HOLES`)
/// - Thưởng điểm nếu khớp/gần với cột nguồn cùng mạng của IC hoặc linh kiện lân cận trên cùng nửa bo
#[inline]
fn eval_pwr_pin_col_cost(
    net: NetId,
    col: u8,
    half: u8,
    pwr_anchors: &[(u8, u8, NetId)],
) -> f64 {
    let is_vcc = net == NetId::VCC;
    let rr = if half == 0 {
        if is_vcc { -1i8 } else { -2i8 }
    } else if is_vcc {
        11i8
    } else {
        10i8
    };
    let is_outer_rail = matches!(rr, -2 | 11);

    let mut cost = 0.0;
    if !rail_ok(col) {
        cost += if is_outer_rail { 28.0 } else { 22.0 };
    } else {
        let pass_r = if half == 0 { -1i8 } else { 10i8 };
        let hits_default = DEFAULT_POWER_HOLES
            .iter()
            .any(|&(dc, dr)| dc == col && (dr == rr || (is_outer_rail && dr == pass_r)));
        if hits_default {
            cost += 15.0;
        }
    }

    let mut min_same_dist = 999i32;
    for &(ac, ah, anet) in pwr_anchors {
        if ah == half && anet == net && ac != col {
            let d = (ac as i32 - col as i32).abs();
            if d < min_same_dist {
                min_same_dist = d;
            }
        }
    }
    if min_same_dist <= 3 {
        cost -= 3.5 - (min_same_dist as f64) * 0.5;
    } else if min_same_dist < 999 {
        cost += (min_same_dist as f64) * 0.25;
    }
    cost
}

/// Tự động hoán đổi chân đối xứng cho BTN2 (1<->2) và SW (1<->3):
/// - Quay chân tín hiệu về phía IC mục tiêu
/// - Tự động lật SW/BTN2 khi chân nguồn VCC/GND rơi trúng cột nghỉ Rail (`(c - 3) % 6 == 5`) hoặc khớp cột nguồn lân cận (#2, #3)
pub fn optimize_symmetric_pins(
    layout: &mut BoardLayout,
    net_of: &FxHashMap<PinRef, NetId>,
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    weights: &CostWeights,
) {
    let mut ic_pin_col: FxHashMap<PinRef, f64> = FxHashMap::default();
    let mut pwr_anchors: Vec<(u8, u8, NetId)> = Vec::new();

    for ic in &layout.ics {
        let h = ic.pins / 2;
        for p in 1..=ic.pins {
            let (c, r) = if ic.rot == 0 {
                if p <= h { (ic.c_min + p - 1, 5i8) } else { (ic.c_min + (ic.pins - p), 4i8) }
            } else if p <= h {
                (ic.c_max - p + 1, 4i8)
            } else {
                (ic.c_min + p - h - 1, 5i8)
            };
            let pr = PinRef { comp: ic.cid, pin: p };
            ic_pin_col.insert(pr, c as f64);
            if let Some(&net) = net_of.get(&pr) {
                if net.is_power() {
                    pwr_anchors.push((c, half_of(r), net));
                }
            }
        }
    }

    let mut net_target_col: FxHashMap<NetId, f64> = FxHashMap::default();
    for net in &parsed.nets {
        if net.id.is_power() {
            continue;
        }
        let mut sum = 0.0;
        let mut cnt = 0usize;
        for pr in &net.pins {
            if let Some(&col) = ic_pin_col.get(pr) {
                sum += col;
                cnt += 1;
            }
        }
        if cnt > 0 {
            net_target_col.insert(net.id, sum / (cnt as f64));
        }
    }

    for &n1 in &topo.r_adj_keys {
        if net_target_col.contains_key(&n1) {
            continue;
        }
        if let Some(neighbors) = topo.r_adj.get(&n1) {
            for &n2 in neighbors {
                if let Some(&target_c) = net_target_col.get(&n2) {
                    net_target_col.insert(n1, target_c);
                    break;
                }
            }
        }
    }

    let pin_col_cost = |net_opt: Option<NetId>, col: u8, half: u8, anchors: &[(u8, u8, NetId)]| -> f64 {
        let Some(net) = net_opt else { return 0.0 };
        if net.is_power() {
            eval_pwr_pin_col_cost(net, col, half, anchors)
        } else if let Some(&t) = net_target_col.get(&net) {
            (col as f64 - t).abs()
        } else {
            0.0
        }
    };

    // Hoán đổi chân 1 <-> 2 của BTN2 nếu giúp chân tín hiệu về gần IC mục tiêu hoặc né cột nghỉ nguồn
    for btn in &mut layout.btns {
        let half = half_of(btn.pin_row());
        let n1 = net_of.get(&PinRef { comp: btn.cid, pin: 1 }).copied();
        let n2 = net_of.get(&PinRef { comp: btn.cid, pin: 2 }).copied();
        if btn.flip == 0 {
            let (c_left, c_right) = (btn.c_min, btn.c_max);
            let cost_normal = pin_col_cost(n1, c_left, half, &pwr_anchors)
                + pin_col_cost(n2, c_right, half, &pwr_anchors);
            let cost_flip = pin_col_cost(n1, c_right, half, &pwr_anchors)
                + pin_col_cost(n2, c_left, half, &pwr_anchors);
            if cost_flip + weights.pin_swap_threshold < cost_normal {
                btn.flip = 1;
            }
        }
        let (c1, c2) = if btn.flip != 0 { (btn.c_max, btn.c_min) } else { (btn.c_min, btn.c_max) };
        if let Some(net) = n1.filter(|n| n.is_power()) {
            pwr_anchors.push((c1, half, net));
        }
        if let Some(net) = n2.filter(|n| n.is_power()) {
            pwr_anchors.push((c2, half, net));
        }
    }

    // Hoán đổi chân 1 <-> 3 của SW nếu giảm khoảng cách dây về IC mục tiêu hoặc giúp chân nguồn VCC/GND né cột nghỉ Rail (#2, #3)
    for sw in &mut layout.sws {
        let half = half_of(sw.row);
        let n1 = net_of.get(&PinRef { comp: sw.cid, pin: 1 }).copied();
        let n3 = net_of.get(&PinRef { comp: sw.cid, pin: 3 }).copied();
        let overlaps_ic = layout.ics.iter().any(|ic| sw.c_min <= ic.c_max && sw.c_max >= ic.c_min);
        if sw.flip == 0 && !overlaps_ic {
            let (c_left, c_right) = (sw.c_min, sw.c_max);
            let cost_normal = pin_col_cost(n1, c_left, half, &pwr_anchors)
                + pin_col_cost(n3, c_right, half, &pwr_anchors);
            let cost_flip = pin_col_cost(n1, c_right, half, &pwr_anchors)
                + pin_col_cost(n3, c_left, half, &pwr_anchors);
            if cost_flip + weights.pin_swap_threshold < cost_normal {
                sw.flip = 1;
            }
        }
        let (c1, c3) = if sw.flip != 0 { (sw.c_max, sw.c_min) } else { (sw.c_min, sw.c_max) };
        if let Some(net) = n1.filter(|n| n.is_power()) {
            pwr_anchors.push((c1, half, net));
        }
        if let Some(net) = n3.filter(|n| n.is_power()) {
            pwr_anchors.push((c3, half, net));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macro_place::topology::build_topology_cache;
    use crate::model::circuit::{CompSpec, PlacedIc, PlacedSw};
    use crate::model::ids::{CompId, CompType};
    use crate::parser::parse_netlist;

    #[test]
    fn test_sw_flip_avoids_rail_gap_and_matches_neighbor_pwr() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_other(CompId(1), "S1", CompType::SW),
            CompSpec::new_other(CompId(2), "S2", CompType::SW),
            CompSpec::new_other(CompId(3), "R1", CompType::R),
            CompSpec::new_other(CompId(4), "R2", CompType::R),
        ];
        // S1: chân 3 nối GND, đặt ở cột 6..=8 (chân 3 rơi vào cột 8 là cột nghỉ nguồn `(8-3)%6 == 5`)
        // S2: chân 1 nối VCC, chân 3 nối GND, đặt ở cột 16..=18 cạnh U1 (chân 7 GND ở cột 15)
        let txt = "
            VCC U1:14; GND U1:7
            S1:2 -> R1 -> U1:1; S1:3 -> GND
            S2:1 -> VCC; S2:2 -> R2 -> U1:2; S2:3 -> GND
        ";
        let parsed = parse_netlist(txt, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        let weights = CostWeights::default();

        let mut layout = BoardLayout::default();
        layout.ics.push(PlacedIc {
            cid: CompId(0), id: "U1".into(), pins: 14, rot: 0, c_min: 9, c_max: 15, w: 7,
        });
        layout.sws.push(PlacedSw {
            cid: CompId(1), id: "S1".into(), c_min: 6, c_max: 8, row: 9, flip: 0,
        });
        layout.sws.push(PlacedSw {
            cid: CompId(2), id: "S2".into(), c_min: 16, c_max: 18, row: 9, flip: 0,
        });

        optimize_symmetric_pins(&mut layout, &parsed.net_of, &parsed, &topo, &weights);

        // S1 tự động lật (flip = 1) để chân 3 (GND) chuyển từ cột nghỉ 8 sang cột 6 (rail_ok(6) == true)
        assert_eq!(layout.sws[0].flip, 1);
        assert_eq!(layout.pin_hole(CompId(1), 3), Some((6, 9)));

        // S2 tự động lật (flip = 1) để chân 3 (GND) chuyển về cột 16 sát chân GND U1:7 (cột 15)
        assert_eq!(layout.sws[1].flip, 1);
        assert_eq!(layout.pin_hole(CompId(2), 3), Some((16, 9)));
    }
}
