use crate::config::weights::CostWeights;
use crate::macro_place::cost::{cross_cost, pure_orientation_bridge_cost};
use crate::macro_place::topology::{infer_optimal_btn_halves, TopologyCache};
use crate::model::circuit::{CompSpec, ParsedNetlist};
use crate::model::ids::{CompId, CompType, Mulberry32};
use rustc_hash::{FxHashMap, FxHashSet};

/// Một phương án phân gán Đại cục (Chiều xoay IC + Nửa bo BTN/SW + Thứ tự linh kiện)
#[derive(Clone, Debug)]
pub struct MacroAssignment {
    pub rot: FxHashMap<CompId, u8>,
    pub half: FxHashMap<CompId, u8>,
    pub order: Vec<usize>,
    pub cost: i64,
}

/// Pha 2 Toán học: Tìm hoán vị vị trí IC & IO bằng Tham lam + Swap + 2-Opt (`connOrder` trong ic-core-engine.js)
pub fn conn_order(
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    interleave_io: bool,
    rot_hint: Option<&FxHashMap<CompId, u8>>,
    half_hint: Option<&FxHashMap<CompId, u8>>,
    comps: &[CompSpec],
    weights: &CostWeights,
) -> Vec<usize> {
    let n = comps.len();
    if n == 0 {
        return Vec::new();
    }

    let mut cid_to_idx = [usize::MAX; 256];
    for (i, c) in comps.iter().enumerate() {
        cid_to_idx[c.cid.0 as usize] = i;
    }

    let mut w = vec![vec![0.0_f64; n]; n];
    for net in &parsed.nets {
        if net.id.is_power() {
            continue;
        }
        let mut ids: Vec<usize> = Vec::with_capacity(net.pins.len());
        for pr in &net.pins {
            let idx = cid_to_idx[pr.comp.0 as usize];
            if idx != usize::MAX && !ids.contains(&idx) {
                ids.push(idx);
            }
        }
        let wt = if ids.len() <= 2 {
            weights.conn_two_pin_weight
        } else {
            weights.conn_multi_pin_weight
        };
        for &a in &ids {
            for &b in &ids {
                if a != b {
                    w[a][b] += wt;
                }
            }
        }
    }

    for (i, c) in comps.iter().enumerate() {
        if !c.comp_type.is_special() {
            continue;
        }
        let nb: Vec<usize> = (0..n).filter(|&j| w[i][j] > 0.0).collect();
        for &a in &nb {
            for &b in &nb {
                if a != b {
                    w[a][b] += weights.conn_special_lookthrough;
                }
            }
        }
    }

    let deg: Vec<f64> = (0..n).map(|i| w[i].iter().sum()).collect();
    let place_idx: Vec<usize> = (0..n).filter(|&i| !comps[i].comp_type.is_special()).collect();
    let r_led_idx: Vec<usize> = (0..n).filter(|&i| comps[i].comp_type.is_special()).collect();

    if place_idx.is_empty() {
        return r_led_idx;
    }

    let ic_bias = if interleave_io { 15.0 } else { 1000.0 };
    let mut left = place_idx.clone();
    left.sort_by(|&a, &b| {
        let sa = (if comps[a].comp_type == CompType::IC { ic_bias } else { 0.0 }) + deg[a];
        let sb = (if comps[b].comp_type == CompType::IC { ic_bias } else { 0.0 }) + deg[b];
        sb.total_cmp(&sa)
    });

    let mut out: Vec<usize> = Vec::with_capacity(place_idx.len());
    let mut cur_opt = left.first().copied();
    while let Some(cur) = cur_opt {
        left.retain(|&x| x != cur);
        out.push(cur);
        if left.is_empty() {
            break;
        }
        let mut best_nx = None;
        let mut best_s = -1.0_f64;
        for &i in &left {
            let type_bonus = if !interleave_io && comps[i].comp_type == CompType::IC {
                250.0
            } else {
                0.0
            };
            let conn_sum: f64 = out.iter().map(|&j| w[i][j]).sum();
            let s = conn_sum * 100.0 + deg[i] + type_bonus;
            if s > best_s {
                best_s = s;
                best_nx = Some(i);
            }
        }
        cur_opt = best_nx;
    }

    let empty_half = FxHashMap::default();
    let eval_order = |arr: &[usize]| -> f64 {
        if let Some(rot) = rot_hint {
            let mut full = Vec::with_capacity(arr.len() + r_led_idx.len());
            full.extend_from_slice(arr);
            full.extend_from_slice(&r_led_idx);
            let h_ref = half_hint.unwrap_or(&empty_half);
            return cross_cost(parsed, topo, rot, h_ref, Some(&full), comps, weights);
        }
        let mut pos = vec![0.0_f64; n];
        let mut x = 0.0_f64;
        for &ci in arr {
            pos[ci] = x;
            x += if comps[ci].comp_type == CompType::IC {
                (comps[ci].pins as f64) / 2.0 + 2.0
            } else {
                3.0
            };
        }
        let mut cost = 0.0_f64;
        for i in 0..arr.len() {
            for j in (i + 1)..arr.len() {
                let u = arr[i];
                let v = arr[j];
                if w[u][v] > 0.0 {
                    cost += w[u][v] * (pos[u] - pos[v]).abs();
                }
            }
        }
        cost
    };

    let mut best_cost = eval_order(&out);
    let mut improved = true;
    while improved {
        improved = false;
        for i in 0..out.len() {
            for j in (i + 1)..out.len() {
                if !interleave_io
                    && (comps[out[i]].comp_type == CompType::IC)
                        != (comps[out[j]].comp_type == CompType::IC)
                {
                    continue;
                }
                out.swap(i, j);
                let c = eval_order(&out);
                if c + 1e-4 < best_cost {
                    best_cost = c;
                    improved = true;
                } else {
                    out.swap(i, j);
                }
            }
        }
        if out.len() >= 3 {
            for i in 0..(out.len() - 2) {
                for j in (i + 2)..out.len() {
                    if !interleave_io
                        && out[i..=j].iter().any(|&ci| comps[ci].comp_type != CompType::IC)
                    {
                        continue;
                    }
                    out[i..=j].reverse();
                    let c = eval_order(&out);
                    if c + 1e-4 < best_cost {
                        best_cost = c;
                        improved = true;
                    } else {
                        out[i..=j].reverse();
                    }
                }
            }
        }
    }

    out.extend_from_slice(&r_led_idx);
    out
}

/// Giải bài toán Kết hợp 2 Pha: Vét cạn 2^(n-1) chiều xoay IC + Cân bằng FM (`bestAssignments` trong ic-core-engine.js)
pub fn best_assignments(
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    k: usize,
    base_order: &[usize],
    comps: &[CompSpec],
    weights: &CostWeights,
) -> Vec<MacroAssignment> {
    let ics: Vec<&CompSpec> = comps.iter().filter(|c| c.comp_type == CompType::IC).collect();
    let io_comps: Vec<&CompSpec> = comps.iter().filter(|c| c.comp_type.is_io()).collect();
    let n_ic = ics.len();
    if n_ic == 0 && io_comps.is_empty() {
        return Vec::new();
    }

    let mut is_interleaved = false;
    if !base_order.is_empty() {
        let mut seen_non_ic = false;
        for &ci in base_order {
            let Some(c) = comps.get(ci) else { continue };
            if c.comp_type.is_special() {
                continue;
            }
            if c.comp_type != CompType::IC {
                seen_non_ic = true;
            } else if seen_non_ic {
                is_interleaved = true;
                break;
            }
        }
    }

    let step_lim = if n_ic == 0 {
        1usize
    } else if n_ic >= 2 {
        1usize << (n_ic - 1).min(20)
    } else {
        2usize
    };

    let mut ic_masks = Vec::with_capacity(step_lim.min(4096));
    if step_lim <= 4096 {
        for m in 0..step_lim {
            ic_masks.push(m);
        }
    } else {
        let mut rng = Mulberry32::new(1337);
        for _ in 0..4096 {
            ic_masks.push(rng.next_usize(step_lim));
        }
    }

    let mut phase1_cands: Vec<(FxHashMap<CompId, u8>, FxHashMap<CompId, u8>, f64)> = ic_masks
        .into_iter()
        .map(|m| {
            let mut rot = FxHashMap::default();
            let mut rem = m;
            for ic in &ics {
                rot.insert(ic.cid, (rem & 1) as u8);
                rem >>= 1;
            }
            let half = infer_optimal_btn_halves(parsed, topo, &rot, comps, weights);
            let p1_score =
                pure_orientation_bridge_cost(parsed, topo, &rot, &half, comps, weights);
            (rot, half, p1_score)
        })
        .collect();

    phase1_cands.sort_by(|a, b| a.2.total_cmp(&b.2));

    let slice_len = phase1_cands.len().min((k * 2).max(20));
    let mut scored: Vec<(FxHashMap<CompId, u8>, FxHashMap<CompId, u8>, Vec<usize>, f64)> =
        Vec::new();

    for (idx, (rot, half, _)) in phase1_cands.into_iter().take(slice_len).enumerate() {
        let opt_order = if idx < 8 {
            conn_order(
                parsed,
                topo,
                is_interleaved,
                Some(&rot),
                Some(&half),
                comps,
                weights,
            )
        } else {
            base_order.to_vec()
        };
        let full_cost = cross_cost(parsed, topo, &rot, &half, Some(&opt_order), comps, weights);
        scored.push((rot.clone(), half.clone(), opt_order.clone(), full_cost));

        if idx < 6 && !io_comps.is_empty() {
            let mut alt_half = half.clone();
            for (bi, b) in io_comps.iter().enumerate() {
                if b.comp_type == CompType::BTN2 {
                    alt_half.insert(b.cid, ((idx + bi) % 2) as u8);
                }
            }
            let alt_cost =
                cross_cost(parsed, topo, &rot, &alt_half, Some(&opt_order), comps, weights);
            scored.push((rot, alt_half, opt_order, alt_cost));
        }
    }

    scored.sort_by(|a, b| a.3.total_cmp(&b.3));

    let reverse_ic_order = |ord: &[usize]| -> Vec<usize> {
        if ord.is_empty() {
            return base_order.to_vec();
        }
        let mut ic_pos = Vec::new();
        let mut ic_vals = Vec::new();
        for (pos, &ci) in ord.iter().enumerate() {
            if comps.get(ci).is_some_and(|c| c.comp_type == CompType::IC) {
                ic_pos.push(pos);
                ic_vals.push(ci);
            }
        }
        ic_vals.reverse();
        let mut res = ord.to_vec();
        for (idx, pos) in ic_pos.into_iter().enumerate() {
            res[pos] = ic_vals[idx];
        }
        res
    };

    let make_sig = |rot: &FxHashMap<CompId, u8>, half: &FxHashMap<CompId, u8>| -> Vec<u8> {
        let mut sig = Vec::with_capacity(ics.len() + io_comps.len());
        for ic in &ics {
            sig.push(rot.get(&ic.cid).copied().unwrap_or(0));
        }
        for io in &io_comps {
            sig.push(half.get(&io.cid).copied().unwrap_or(0));
        }
        sig
    };

    let mut out = Vec::with_capacity(k);
    let mut seen: FxHashSet<Vec<u8>> = FxHashSet::default();

    for (i, (rot, half, order, cost)) in scored.into_iter().enumerate() {
        if out.len() >= k {
            break;
        }
        let key = make_sig(&rot, &half);
        if seen.insert(key) {
            out.push(MacroAssignment {
                rot: rot.clone(),
                half: half.clone(),
                order: order.clone(),
                cost: cost.floor() as i64,
            });
        }
        if i < 3 && out.len() < k {
            let mut flip_rot = FxHashMap::default();
            for (&cid, &v) in &rot {
                flip_rot.insert(cid, if v != 0 { 0 } else { 1 });
            }
            let mut flip_half = half.clone();
            for b in &io_comps {
                if b.comp_type == CompType::BTN2 {
                    let cur = half.get(&b.cid).copied().unwrap_or(0);
                    flip_half.insert(b.cid, if cur != 0 { 0 } else { 1 });
                }
            }
            let rev_order = reverse_ic_order(&order);
            let f_key = make_sig(&flip_rot, &flip_half);
            if seen.insert(f_key) {
                let f_cost = cross_cost(
                    parsed,
                    topo,
                    &flip_rot,
                    &flip_half,
                    Some(&rev_order),
                    comps,
                    weights,
                );
                out.push(MacroAssignment {
                    rot: flip_rot,
                    half: flip_half,
                    order: rev_order,
                    cost: f_cost.floor() as i64,
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macro_place::topology::build_topology_cache;
    use crate::parser::parse_netlist;

    #[test]
    fn test_conn_order_and_best_assignments() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_ic(CompId(1), "U2", 14, 0),
            CompSpec::new_other(CompId(2), "B1", CompType::BTN2),
            CompSpec::new_other(CompId(3), "R1", CompType::R),
        ];
        let txt = "
            VCC U1:14 U2:14; GND U1:7 U2:7
            U1:1 -> U2:2; U1:3 -> R1 -> B1:1; B1:2 -> GND
        ";
        let parsed = parse_netlist(txt, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        let weights = CostWeights::default();

        let ord = conn_order(&parsed, &topo, false, None, None, &comps, &weights);
        assert_eq!(ord.len(), 4);
        assert_eq!(ord[3], 3); // Linh kiện rời R1 luôn nằm ở cuối danh sách đặt chỗ

        let asgs = best_assignments(&parsed, &topo, 6, &ord, &comps, &weights);
        assert!(!asgs.is_empty());
        assert!(asgs[0].cost <= asgs.last().unwrap().cost);
    }
}
