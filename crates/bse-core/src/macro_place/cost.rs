use crate::config::weights::CostWeights;
use crate::macro_place::topology::{compute_exact_1d_positions, TopologyCache};
use crate::model::bitboard::BoardState;
use crate::model::circuit::{CompSpec, ParsedNetlist, PlacedIc};
use crate::model::ids::{CompId, CompType, NetId, PinRef};
use rustc_hash::FxHashMap;

/// Pha 1 Toán học: Hàm mục tiêu Cầu rãnh thuần túy Z = sum(U_k * L_k) (`pureOrientationBridgeCost`)
pub fn pure_orientation_bridge_cost(
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    rot: &FxHashMap<CompId, u8>,
    half: &FxHashMap<CompId, u8>,
    comps: &[CompSpec],
    weights: &CostWeights,
) -> f64 {
    let mut by_cid: [Option<&CompSpec>; 256] = [None; 256];
    for c in comps {
        by_cid[c.cid.0 as usize] = Some(c);
    }

    let get_pin_half = |pr: PinRef| -> Option<u8> {
        let c = by_cid[pr.comp.0 as usize]?;
        match c.comp_type {
            CompType::IC => {
                let w = c.pins / 2;
                let r = rot.get(&c.cid).copied().unwrap_or(0) != 0;
                Some(if (pr.pin <= w) == r { 0 } else { 1 })
            }
            CompType::SW | CompType::BTN2 => {
                let h = half
                    .get(&c.cid)
                    .copied()
                    .map(|v| if v != 0 { 1 } else { 0 })
                    .unwrap_or(if c.comp_type == CompType::SW { 1 } else { 0 });
                Some(h)
            }
            _ => None,
        }
    };

    let (mut bridges, mut top_load, mut bot_load) = (0i32, 0i32, 0i32);
    let mut net_halves: FxHashMap<NetId, (i32, i32)> = FxHashMap::default();

    for net in &parsed.nets {
        if net.id.is_power() {
            continue;
        }
        let (mut t, mut b) = (0i32, 0i32);
        for &pr in &net.pins {
            match get_pin_half(pr) {
                Some(0) => t += 1,
                Some(1) => b += 1,
                _ => {}
            }
        }
        net_halves.insert(net.id, (t, b));
        if t > 0 && b > 0 { bridges += 1; }
        if t >= 2 { top_load += t - 1; }
        if b >= 2 { bot_load += b - 1; }
    }

    let mut r_cross_pen = 0.0;
    for &(n1, n2) in &topo.r_edges {
        let (Some(&(t1, b1)), Some(&(t2, b2))) = (net_halves.get(&n1), net_halves.get(&n2)) else {
            continue;
        };
        let (only_top1, only_bot1) = (t1 > 0 && b1 == 0, b1 > 0 && t1 == 0);
        let (only_top2, only_bot2) = (t2 > 0 && b2 == 0, b2 > 0 && t2 == 0);
        if (only_top1 && only_bot2) || (only_bot1 && only_top2) {
            r_cross_pen += weights.p1_rcross_pen;
        }
    }

    (bridges as f64) * weights.p1_bridge_weight
        + r_cross_pen
        + ((top_load - bot_load).abs() as f64) * weights.p1_load_diff_weight
}

/// Hàm đánh giá tổng hợp: Cầu rãnh + Độ căng dây ngang + Bán kính vàng (`crossCost` trong ic-core-engine.js)
pub fn cross_cost(
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    rot: &FxHashMap<CompId, u8>,
    half: &FxHashMap<CompId, u8>,
    order_indices: Option<&[usize]>,
    comps: &[CompSpec],
    weights: &CostWeights,
) -> f64 {
    let pos_id = compute_exact_1d_positions(order_indices, Some(half), comps);
    let mut by_cid: [Option<&CompSpec>; 256] = [None; 256];
    for c in comps {
        if pos_id[c.cid.0 as usize] > 0 {
            by_cid[c.cid.0 as usize] = Some(c);
        }
    }

    let get_pin_loc = |pr: PinRef| -> Option<(i32, u8)> {
        let c = by_cid[pr.comp.0 as usize]?;
        let base_c = pos_id[c.cid.0 as usize].max(1);
        let p = pr.pin as i32;
        match c.comp_type {
            CompType::IC => {
                let w = (c.pins / 2) as i32;
                let r = rot.get(&c.cid).copied().unwrap_or(0) != 0;
                let h = if (p <= w) == r { 0 } else { 1 };
                let pin_c = if !r {
                    if p <= w { base_c + p - 1 } else { base_c + (c.pins as i32 - p) }
                } else if p <= w {
                    base_c + w - p
                } else {
                    base_c + p - w - 1
                };
                Some((pin_c, h))
            }
            CompType::SW => {
                let h = half.get(&c.cid).copied().map_or(1, |v| u8::from(v != 0));
                Some((base_c + p - 1, h))
            }
            CompType::BTN2 => {
                let h = half.get(&c.cid).copied().map_or(0, |v| u8::from(v != 0));
                Some((base_c + if p == 1 { 0 } else { 2 }, h))
            }
            _ => None,
        }
    };

    let (mut cost, mut top_load, mut bot_load) = (0.0, 0i32, 0i32);
    let mut net_locs: FxHashMap<NetId, Vec<(i32, u8)>> = FxHashMap::default();

    for net in &parsed.nets {
        if net.id.is_power() {
            continue;
        }
        let (mut t, mut b, mut min_c, mut max_c) = (0i32, 0i32, 999i32, -999i32);
        let mut locs = Vec::with_capacity(net.pins.len());
        for &pr in &net.pins {
            if let Some(loc) = get_pin_loc(pr) {
                locs.push(loc);
                if loc.1 != 0 { b += 1; } else { t += 1; }
                min_c = min_c.min(loc.0);
                max_c = max_c.max(loc.0);
            }
        }
        net_locs.insert(net.id, locs);
        if t > 0 && b > 0 { cost += weights.cross_bridge_pen; }
        if max_c >= min_c { cost += ((max_c - min_c) as f64) * weights.cross_span_weight; }
        if t >= 2 { top_load += t - 1; }
        if b >= 2 { bot_load += b - 1; }
    }

    for &(n1, n2) in &topo.r_edges {
        let (Some(l1), Some(l2)) = (net_locs.get(&n1), net_locs.get(&n2)) else { continue };
        for p1 in l1 {
            for p2 in l2 {
                if p1.1 != p2.1 { cost += weights.cross_rcross_pen; }
                let d_col = (p1.0 - p2.0).abs() as f64;
                if p1.1 == p2.1 && d_col < 3.0 {
                    cost += (3.0 - d_col) * weights.cross_rclose_pen;
                } else {
                    cost += d_col * weights.cross_rdist_weight;
                }
            }
        }
    }

    cost += ((top_load - bot_load).abs() as f64) * weights.p1_load_diff_weight;

    let (mut top_io, mut bot_io) = (0i32, 0i32);
    for c in comps.iter().filter(|c| c.comp_type.is_io()) {
        let h = half.get(&c.cid).copied().unwrap_or(if c.comp_type == CompType::SW { 1 } else { 0 });
        if h != 0 { bot_io += 1; } else { top_io += 1; }
    }
    let io_diff = (top_io - bot_io).abs();
    if io_diff > 1 {
        cost += ((io_diff - 1) as f64) * weights.cross_io_imbalance_pen;
    }
    cost
}

/// Tính nhu cầu Khe Cầu Rãnh thực tế (`computeBridgeDemand` trong ic-core-engine.js)
pub fn compute_bridge_demand(
    items: &[CompSpec],
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    rot: &FxHashMap<CompId, u8>,
    half: &FxHashMap<CompId, u8>,
    comps: &[CompSpec],
    weights: &CostWeights,
) -> FxHashMap<CompId, f64> {
    let order_indices: Vec<usize> = items
        .iter()
        .filter_map(|it| comps.iter().position(|x| x.cid == it.cid))
        .collect();
    let pos_id = compute_exact_1d_positions(Some(&order_indices), Some(half), comps);

    let mut by_cid: [Option<&CompSpec>; 256] = [None; 256];
    let mut demand: FxHashMap<CompId, f64> = FxHashMap::default();
    for c in items {
        if pos_id[c.cid.0 as usize] > 0 {
            by_cid[c.cid.0 as usize] = Some(c);
        }
        let load = topo.ic_periph_load.get(&c.cid).copied().unwrap_or(0) as f64;
        demand.insert(c.cid, load * weights.bridge_demand_periph_scale);
    }

    let ic_list: Vec<&CompSpec> = items.iter().filter(|c| c.comp_type == CompType::IC).collect();
    if ic_list.is_empty() {
        return demand;
    }

    for net in &parsed.nets {
        if net.id.is_power() {
            continue;
        }
        let (mut has_top, mut has_bot, mut min_c, mut max_c) = (false, false, 999i32, -999i32);
        for &pr in &net.pins {
            let Some(c) = by_cid[pr.comp.0 as usize] else { continue };
            let base_c = pos_id[c.cid.0 as usize].max(1);
            let p = pr.pin as i32;
            if c.comp_type == CompType::IC {
                let w = (c.pins / 2) as i32;
                let r = rot.get(&c.cid).copied().unwrap_or(0) != 0;
                if (p <= w) == r { has_top = true; } else { has_bot = true; }
                let pin_c = if !r {
                    if p <= w { base_c + p - 1 } else { base_c + (c.pins as i32 - p) }
                } else if p <= w {
                    base_c + w - p
                } else {
                    base_c + p - w - 1
                };
                min_c = min_c.min(pin_c);
                max_c = max_c.max(pin_c);
            } else if c.comp_type.is_io() {
                let h = half
                    .get(&c.cid)
                    .copied()
                    .map_or(if c.comp_type == CompType::SW { 1 } else { 0 }, |v| u8::from(v != 0));
                if h == 0 { has_top = true; } else { has_bot = true; }
                min_c = min_c.min(base_c);
                max_c = max_c.max(base_c);
            }
        }
        if has_top && has_bot && max_c >= min_c {
            for ic in &ic_list {
                let gap_col = pos_id[ic.cid.0 as usize].max(1) + (ic.pins as i32) / 2;
                if gap_col >= min_c - 2 && gap_col <= max_c + 2 {
                    *demand.entry(ic.cid).or_insert(0.0) += weights.bridge_demand_cross_net_add;
                }
            }
        }
    }
    demand
}

/// Chọn cặp chân đại diện (bestT, bestB) gần Khe Cầu Rãnh Trống nhất (`pickBestCrossPair`)
pub fn pick_best_cross_pair(
    top_r: &[[i8; 2]],
    bot_r: &[[i8; 2]],
    placed_ics: &[PlacedIc],
    board: &BoardState,
    net_tag: u16,
    weights: &CostWeights,
) -> ([i8; 2], [i8; 2], i32) {
    let mut ic_occupied_col = [false; 65];
    for ic in placed_ics {
        for c in ic.c_min..=ic.c_max {
            if (c as usize) < ic_occupied_col.len() {
                ic_occupied_col[c as usize] = true;
            }
        }
    }

    let mut open_bridge_cols = [0i32; 64];
    let mut open_len = 0usize;
    for c in 1u8..=63 {
        let cu = c as usize;
        if ic_occupied_col[cu] {
            continue;
        }
        let (n0, n1) = (board.col_net[0][cu], board.col_net[1][cu]);
        if !board.is_occupied(c, 4)
            && !board.is_occupied(c, 5)
            && (n0 == 0 || n0 == net_tag)
            && (n1 == 0 || n1 == net_tag)
        {
            open_bridge_cols[open_len] = c as i32;
            open_len += 1;
        }
    }

    let (mut best_t, mut best_b, mut min_d) = (top_r[0], bot_r[0], 1e9_f64);
    for &t in top_r {
        for &b in bot_r {
            let (tc, bc) = (t[0] as i32, b[0] as i32);
            let span = (tc - bc).abs() as f64;
            let mut gap_dist = 0.0;
            if open_len > 0 {
                let mut best_g = 999i32;
                for &g in &open_bridge_cols[..open_len] {
                    best_g = best_g.min((tc - g).abs() + (bc - g).abs());
                }
                gap_dist = (best_g as f64) * weights.pick_cross_gap_dist_scale;
            }
            let d = span + gap_dist;
            if d < min_d {
                min_d = d;
                best_t = t;
                best_b = b;
            }
        }
    }
    (best_t, best_b, ((best_t[0] as i32) - (best_b[0] as i32)).abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macro_place::topology::{build_topology_cache, infer_optimal_btn_halves};
    use crate::parser::parse_netlist;

    #[test]
    fn test_topology_fm_and_cost_functions() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_ic(CompId(1), "U2", 14, 0),
            CompSpec::new_other(CompId(2), "B1", CompType::BTN2),
            CompSpec::new_other(CompId(3), "S1", CompType::SW),
            CompSpec::new_other(CompId(4), "R1", CompType::R),
            CompSpec::new_other(CompId(5), "L1", CompType::LED),
        ];
        let txt = "
            VCC U1:14 U2:14; GND U1:7 U2:7
            U1:1 -> U2:13
            U1:2 -> R1 -> L1 -> GND
            B1:1 -> R1; B1:2 -> GND
            S1:2 -> U2:1; S1:1 -> VCC; S1:3 -> GND
        ";
        let parsed = parse_netlist(txt, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        assert_eq!(topo.r_edges.len(), 1);
        assert!(topo.ic_periph_load.get(&CompId(0)).copied().unwrap_or(0) > 0);

        let weights = CostWeights::default();
        let mut rot = FxHashMap::default();
        rot.insert(CompId(0), 0);
        rot.insert(CompId(1), 0);

        let half = infer_optimal_btn_halves(&parsed, &topo, &rot, &comps, &weights);
        assert_eq!(half.len(), 2);

        let pos = compute_exact_1d_positions(None, Some(&half), &comps);
        assert_eq!(pos[0], 1);
        assert_eq!(pos[1], 10);

        let p1_cost = pure_orientation_bridge_cost(&parsed, &topo, &rot, &half, &comps, &weights);
        let full_cost = cross_cost(&parsed, &topo, &rot, &half, None, &comps, &weights);
        assert!(p1_cost > 0.0);
        assert!(full_cost > p1_cost);

        let demand = compute_bridge_demand(&comps, &parsed, &topo, &rot, &half, &comps, &weights);
        assert!(demand.get(&CompId(0)).copied().unwrap_or(0.0) > 0.0);
    }
}
