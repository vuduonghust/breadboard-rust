use crate::config::weights::CostWeights;
use crate::model::circuit::{CompSpec, ParsedNetlist};
use crate::model::ids::{CompId, CompType, NetId, PinRef};
use rustc_hash::FxHashMap;

/// Bộ đệm đồ thị kết nối xuyên qua điện trở/tụ gốm nối tiếp (`buildTopologyCache` trong ic-core-engine.js)
#[derive(Clone, Debug, Default)]
pub struct TopologyCache {
    pub r_adj: FxHashMap<NetId, Vec<NetId>>,
    pub r_adj_keys: Vec<NetId>,
    pub r_edges: Vec<(NetId, NetId)>,
    pub ic_periph_load: FxHashMap<CompId, u32>,
}

/// Xây dựng bộ đệm đồ thị kết nối xuyên qua R/CC và tính tải ngoại vi LED/C/CC của từng IC
pub fn build_topology_cache(parsed: &ParsedNetlist, comps: &[CompSpec]) -> TopologyCache {
    let mut r_adj: FxHashMap<NetId, Vec<NetId>> = FxHashMap::default();
    let mut r_adj_keys: Vec<NetId> = Vec::new();
    let mut r_edges: Vec<(NetId, NetId)> = Vec::new();
    let mut led_demand_by_net: FxHashMap<NetId, u32> = FxHashMap::default();

    for c in comps {
        let n1 = parsed.net_of.get(&PinRef { comp: c.cid, pin: 1 }).copied();
        let n2 = parsed.net_of.get(&PinRef { comp: c.cid, pin: 2 }).copied();

        if matches!(c.comp_type, CompType::R | CompType::CC) {
            let (Some(a), Some(b)) = (n1, n2) else {
                continue;
            };
            if a.is_power() || b.is_power() {
                continue;
            }
            if !r_adj.contains_key(&a) {
                r_adj_keys.push(a);
            }
            r_adj.entry(a).or_default().push(b);
            if !r_adj.contains_key(&b) {
                r_adj_keys.push(b);
            }
            r_adj.entry(b).or_default().push(a);
            if a != b {
                r_edges.push((a, b));
            }
        }

        if matches!(c.comp_type, CompType::LED | CompType::C | CompType::CC) {
            for n_opt in [n1, n2] {
                let Some(n) = n_opt else {
                    continue;
                };
                if n.is_power() {
                    continue;
                }
                *led_demand_by_net.entry(n).or_insert(0) += 1;
                if let Some(neighbors) = r_adj.get(&n) {
                    for &nb in neighbors {
                        *led_demand_by_net.entry(nb).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    let mut ic_periph_load: FxHashMap<CompId, u32> = FxHashMap::default();
    for net in &parsed.nets {
        if net.id.is_power() {
            continue;
        }
        let d = led_demand_by_net.get(&net.id).copied().unwrap_or(0);
        if d == 0 {
            continue;
        }
        for pr in &net.pins {
            *ic_periph_load.entry(pr.comp).or_insert(0) += d;
        }
    }

    TopologyCache {
        r_adj,
        r_adj_keys,
        r_edges,
        ic_periph_load,
    }
}

/// Tính tọa độ cột 1D sát thực tế trên Stack (`computeExact1DPositions` trong ic-core-engine.js)
/// Bảo toàn quy tắc: mỗi IC cách nhau >= 1..2 cột + tính đồng vị (coLoc) cho BTN2/SW khác nửa bo.
pub fn compute_exact_1d_positions(
    order_indices: Option<&[usize]>,
    half: Option<&FxHashMap<CompId, u8>>,
    comps: &[CompSpec],
) -> [i32; 256] {
    let mut pos_id = [0i32; 256];
    let mut col: i32 = 1;
    let mut last_small_col: i32 = -1;
    let mut last_small_half: i32 = -1;

    let mut process_comp = |c: &CompSpec| {
        if c.comp_type.is_special() {
            return;
        }
        let idx = c.cid.0 as usize;
        if c.comp_type == CompType::IC {
            pos_id[idx] = col;
            col += (c.pins as i32) / 2 + 2;
            last_small_col = -1;
        } else {
            let h = half
                .and_then(|m| m.get(&c.cid).copied())
                .unwrap_or(if c.comp_type == CompType::SW { 1 } else { 0 }) as i32;
            if last_small_col != -1 && last_small_half != h {
                pos_id[idx] = last_small_col;
                last_small_col = -1;
            } else {
                pos_id[idx] = col;
                last_small_col = col;
                last_small_half = h;
                col += 3 + 1;
            }
        }
    };

    if let Some(ord) = order_indices {
        for &i in ord {
            if let Some(c) = comps.get(i) {
                process_comp(c);
            }
        }
    } else {
        for c in comps {
            process_comp(c);
        }
    }
    pos_id
}

/// Thuật toán Fiduccia-Mattheyses (FM Gain) kết hợp Cân bằng Đồng vị cho BTN2 & SW (`inferOptimalBtnHalves`)
pub fn infer_optimal_btn_halves(
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    rot: &FxHashMap<CompId, u8>,
    comps: &[CompSpec],
    weights: &CostWeights,
) -> FxHashMap<CompId, u8> {
    let mut by_cid: [Option<&CompSpec>; 256] = [None; 256];
    for c in comps {
        by_cid[c.cid.0 as usize] = Some(c);
    }

    let mut net_half_vote: FxHashMap<NetId, f64> = FxHashMap::default();
    for net in &parsed.nets {
        if net.id.is_power() {
            continue;
        }
        let mut vote = 0.0;
        for pr in &net.pins {
            let Some(c) = by_cid[pr.comp.0 as usize] else {
                continue;
            };
            if c.comp_type != CompType::IC {
                continue;
            }
            let w = c.pins / 2;
            let r = rot.get(&c.cid).copied().unwrap_or(0) != 0;
            let h = if (pr.pin <= w) == r { 0 } else { 1 };
            vote += if h == 1 {
                weights.fm_ic_pin_vote
            } else {
                -weights.fm_ic_pin_vote
            };
        }
        net_half_vote.insert(net.id, vote);
    }

    let io_comps: Vec<&CompSpec> = comps.iter().filter(|c| c.comp_type.is_io()).collect();
    let mut gains: Vec<(CompId, f64)> = io_comps
        .iter()
        .map(|c| {
            let mut score_bot = if c.comp_type == CompType::SW {
                weights.fm_sw_bot_bias
            } else {
                0.0
            };
            for pin in 1..=c.pins {
                let Some(&n) = parsed.net_of.get(&PinRef { comp: c.cid, pin }) else {
                    continue;
                };
                if n.is_power() {
                    continue;
                }
                score_bot += net_half_vote.get(&n).copied().unwrap_or(0.0);
                if let Some(nbs) = topo.r_adj.get(&n) {
                    for &nb in nbs {
                        score_bot +=
                            net_half_vote.get(&nb).copied().unwrap_or(0.0) * weights.fm_radj_factor;
                    }
                }
            }
            (c.cid, score_bot)
        })
        .collect();

    gains.sort_by(|a, b| a.1.total_cmp(&b.1));

    let max_per_half = ((io_comps.len() as f64) * weights.fm_max_per_half_ratio).ceil() as usize;
    let mut top_cnt = 0usize;
    let mut bot_cnt = 0usize;
    let mut half = FxHashMap::default();

    for (cid, score_bot) in gains {
        if score_bot < -0.2 && top_cnt < max_per_half {
            half.insert(cid, 0);
            top_cnt += 1;
        } else if score_bot > 0.2 && bot_cnt < max_per_half {
            half.insert(cid, 1);
            bot_cnt += 1;
        } else if top_cnt <= bot_cnt {
            half.insert(cid, 0);
            top_cnt += 1;
        } else {
            half.insert(cid, 1);
            bot_cnt += 1;
        }
    }
    half
}
