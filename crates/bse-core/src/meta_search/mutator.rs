use crate::config::personas::PERSONAS;
use crate::config::weights::CostWeights;
use crate::macro_place::cost::cross_cost;
use crate::macro_place::topology::TopologyCache;
use crate::meta_search::builder::{
    extract_frozen_map, parse_err_info, BoardSolution, CandidateOpts, SearchPhase,
};
use crate::meta_search::rl_bandit::{AgentFocus, MetaAgent, RlBandit, NUM_ISLANDS};
use crate::meta_search::tabu_basin::TabuBasin;
use crate::meta_search::zobrist::{decode_btn_bend, encode_btn_bend, tabu_key};
use crate::model::circuit::{CompSpec, ParsedNetlist};
use crate::model::ids::{CompId, CompType, Mulberry32};
use rustc_hash::FxHashMap;

/// Cân bằng phân bố nửa bo `half` cho toàn bộ linh kiện IO (`SW` + `BTN2`) về tỷ lệ đối xứng (VD: 3 Trên – 3 Dưới) (#3)
fn rebalance_io_halves(
    half: &mut FxHashMap<CompId, u8>,
    comps: &[CompSpec],
    rng: &mut Mulberry32,
) {
    let io_cids: Vec<(CompId, u8)> = comps
        .iter()
        .filter(|c| c.comp_type.is_io())
        .map(|c| {
            let def_h = u8::from(c.comp_type == CompType::SW);
            let h = u8::from(half.get(&c.cid).copied().unwrap_or(def_h) != 0);
            (c.cid, h)
        })
        .collect();
    if io_cids.len() < 2 {
        return;
    }
    let target_top = io_cids.len() / 2;
    let mut top_list: Vec<CompId> = io_cids.iter().filter(|&&(_, h)| h == 0).map(|&(cid, _)| cid).collect();
    let mut bot_list: Vec<CompId> = io_cids.iter().filter(|&&(_, h)| h != 0).map(|&(cid, _)| cid).collect();

    while top_list.len() > target_top + (io_cids.len() % 2) {
        let idx = rng.next_usize(top_list.len());
        let cid = top_list.swap_remove(idx);
        half.insert(cid, 1);
        bot_list.push(cid);
    }
    while bot_list.len() > io_cids.len() - target_top {
        let idx = rng.next_usize(bot_list.len());
        let cid = bot_list.swap_remove(idx);
        half.insert(cid, 0);
        top_list.push(cid);
    }
}

/// Đột biến góc bẻ chân `btn_bend` (`-1, 0, +1`) cho 1 nút bấm `BTN2` (lưu mã hóa trong `rot`) (#2, #8)
fn mutate_one_btn_bend(
    rot: &mut FxHashMap<CompId, u8>,
    half: &FxHashMap<CompId, u8>,
    comps: &[CompSpec],
    rng: &mut Mulberry32,
) -> bool {
    let btns: Vec<CompId> = comps
        .iter()
        .filter(|c| c.comp_type == CompType::BTN2)
        .map(|c| c.cid)
        .collect();
    if btns.is_empty() {
        return false;
    }
    let cid = btns[rng.next_usize(btns.len())];
    let h = half.get(&cid).copied().unwrap_or(0);
    let def_code = if h == 0 { encode_btn_bend(-1) } else { encode_btn_bend(1) };
    let cur_bend = decode_btn_bend(rot.get(&cid).copied().unwrap_or(def_code));
    let choices: [i8; 2] = match cur_bend {
        -1 => [0, 1],
        0 => [-1, 1],
        _ => [-1, 0],
    };
    let next_bend = choices[rng.next_usize(2)];
    rot.insert(cid, encode_btn_bend(next_bend));
    true
}

/// Đột biến Nhảy bậc Vĩ mô (`applyMacroJump` trong main.html) khi bố cục cũ bị khóa Tabu
pub fn apply_macro_jump(
    q: &mut CandidateOpts,
    focus: AgentFocus,
    comps: &[CompSpec],
    rng: &mut Mulberry32,
) {
    let lock_right = focus == AgentFocus::LockIo;
    let swappable: Vec<usize> = q
        .order
        .iter()
        .enumerate()
        .filter_map(|(pos, &ci)| {
            let c = comps.get(ci)?;
            if c.comp_type != CompType::IC || (lock_right && c.id == "U1") {
                return None;
            }
            Some(pos)
        })
        .collect();
    if swappable.len() >= 2 {
        let idx_a = rng.next_usize(swappable.len());
        let mut idx_b = rng.next_usize(swappable.len());
        if idx_a == idx_b {
            idx_b = (idx_a + 1) % swappable.len();
        }
        q.order.swap(swappable[idx_a], swappable[idx_b]);
    }
    let rot_cands: Vec<CompId> = comps
        .iter()
        .filter(|c| c.comp_type == CompType::IC && !(lock_right && c.id == "U1"))
        .map(|c| c.cid)
        .collect();
    if !rot_cands.is_empty() {
        let p1 = rot_cands[rng.next_usize(rot_cands.len())];
        let cur1 = q.rot.get(&p1).copied().unwrap_or(0);
        q.rot.insert(p1, u8::from(cur1 == 0));
        if rng.next_f64() < 0.5 && rot_cands.len() > 1 {
            let p2 = rot_cands[rng.next_usize(rot_cands.len())];
            let cur2 = q.rot.get(&p2).copied().unwrap_or(0);
            q.rot.insert(p2, u8::from(cur2 == 0));
        }
    }
    if !lock_right {
        let io_cids: Vec<(CompId, CompType)> = comps
            .iter()
            .filter(|c| c.comp_type.is_io())
            .map(|c| (c.cid, c.comp_type))
            .collect();
        if !io_cids.is_empty() {
            let (pick_io, ct) = io_cids[rng.next_usize(io_cids.len())];
            let def_h = u8::from(ct == CompType::SW);
            let cur_h = q.half.get(&pick_io).copied().unwrap_or(def_h);
            q.half.insert(pick_io, u8::from(cur_h == 0));
            rebalance_io_halves(&mut q.half, comps, rng);
        }
        mutate_one_btn_bend(&mut q.rot, &q.half, comps, rng);
    }
    q.persona = q.island % NUM_ISLANDS;
    q.gaps = None;
    q.lock_gaps = false;
    q.frozen_specials.clear();
    q.evict_cols_mask = 0;
    q.handed_over = false;
    q.rot_mutated = true;
    q.phase = SearchPhase::Diversify;
}

/// Tinh chỉnh chiều xoay 1 IC hoặc nửa bo 1 IO (`SW`/`BTN2`) bằng hàm mục tiêu nhanh `cross_cost` (`polishOneIC`)
pub fn polish_one_ic(
    q: &mut CandidateOpts,
    focus: AgentFocus,
    comps: &[CompSpec],
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    weights: &CostWeights,
    rng: &mut Mulberry32,
) {
    if q.rot_mutated || rng.next_f64() > 0.45 {
        return;
    }
    let ics: Vec<&CompSpec> = comps.iter().filter(|c| c.comp_type == CompType::IC).collect();
    if !ics.is_empty() {
        let pick = ics[rng.next_usize(ics.len())];
        if !(focus == AgentFocus::LockIo && pick.id == "U1") {
            let c0 = cross_cost(parsed, topo, &q.rot, &q.half, Some(&q.order), comps, weights);
            let old_r = q.rot.get(&pick.cid).copied().unwrap_or(0);
            q.rot.insert(pick.cid, u8::from(old_r == 0));
            if cross_cost(parsed, topo, &q.rot, &q.half, Some(&q.order), comps, weights) > c0 {
                q.rot.insert(pick.cid, old_r);
            }
        }
    }
    if focus != AgentFocus::LockIo && rng.next_f64() < 0.35 {
        let ios: Vec<&CompSpec> = comps.iter().filter(|c| c.comp_type.is_io()).collect();
        if !ios.is_empty() {
            let pick_io = ios[rng.next_usize(ios.len())];
            let c0 = cross_cost(parsed, topo, &q.rot, &q.half, Some(&q.order), comps, weights);
            let def_h = u8::from(pick_io.comp_type == CompType::SW);
            let old_h = q.half.get(&pick_io.cid).copied().unwrap_or(def_h);
            q.half.insert(pick_io.cid, u8::from(old_h == 0));
            if cross_cost(parsed, topo, &q.rot, &q.half, Some(&q.order), comps, weights) > c0 {
                q.half.insert(pick_io.cid, old_h);
            }
        }
    }
}

/// Hàm đột biến & lai ghép đa chiến thuật (`mutate` trong main.html)
#[allow(clippy::too_many_arguments)]
pub fn mutate_candidate(
    base: &CandidateOpts,
    parent: Option<&BoardSolution>,
    stall_cnt: u32,
    mate: Option<&BoardSolution>,
    isl: usize,
    cur_it: u32,
    place_comp_count: usize,
    r_comp_count: usize,
    agent: &MetaAgent,
    tabu: &TabuBasin,
    rl: &RlBandit,
    comps: &[CompSpec],
    parsed: &ParsedNetlist,
    rng: &mut Mulberry32,
) -> CandidateOpts {
    let default_gap = base.gap.unwrap_or(1);
    let mut q = CandidateOpts {
        island: isl % NUM_ISLANDS,
        persona: isl % NUM_ISLANDS,
        order: base.order.clone(),
        rot: base.rot.clone(),
        half: base.half.clone(),
        io_flip: base.io_flip.clone(),
        gap: base.gap,
        gaps: Some(base.gaps.clone().unwrap_or_else(|| vec![default_gap; place_comp_count])),
        compact: false,
        lock_gaps: false,
        r_order: if !base.r_order.is_empty() { base.r_order.clone() } else { (0..r_comp_count).collect() },
        ortho_weight: base.ortho_weight,
        dyn_br_pen: agent.dyn_br_pen,
        dyn_aux_share: agent.dyn_aux_share,
        evict_cols_mask: 0,
        boost_nets: Default::default(),
        frozen_specials: Default::default(),
        rev: base.rev,
        net_ord: base.net_ord,
        passes: Some(PERSONAS[isl % NUM_ISLANDS].passes),
        sr: Some(rng.next_usize(1_000_000_000) as u32),
        r_seed: Some(rng.next_usize(1_000_000_000) as u32),
        use_star_hub: base.use_star_hub,
        phase: base.phase,
        rot_mutated: false,
        handed_over: false,
        parent_score: parent.map(|p| p.score),
        arm: None,
    };

    let has_err = parent.is_none_or(|p| p.unplaced > 0 || !p.errors.is_empty());
    let is_near_solved = parent.is_some_and(|p| p.unplaced == 0 && (1..=3).contains(&p.errors.len()));
    let t_key = tabu_key(&q.order, &q.rot, &q.half, comps);

    // 1. CHỐNG KẸT TUYỆT ĐỐI: Kiểm tra Tabu và stall_cnt TRƯỚC khi xét nhánh P5:Surgeon
    if has_err
        && (agent.focus == AgentFocus::Macro
            || tabu.is_tabu(t_key, cur_it)
            || (stall_cnt > 24 && rng.next_f64() < 0.65))
    {
        apply_macro_jump(&mut q, agent.focus, comps, rng);
        return q;
    }

    if let Some(m) = mate.filter(|_| rng.next_f64() < 0.28) {
        for c in comps {
            if c.comp_type == CompType::IC {
                if agent.focus == AgentFocus::LockIo && c.id == "U1" {
                    continue;
                }
                if rng.next_f64() < 0.5 {
                    if let Some(&rv) = m.opts.rot.get(&c.cid) {
                        q.rot.insert(c.cid, rv);
                    }
                }
            } else if c.comp_type.is_io() && agent.focus != AgentFocus::LockIo && rng.next_f64() < 0.5 {
                if let Some(&hv) = m.opts.half.get(&c.cid) {
                    q.half.insert(c.cid, hv);
                }
                if c.comp_type == CompType::BTN2 {
                    if let Some(&bv) = m.opts.rot.get(&c.cid) {
                        q.rot.insert(c.cid, bv);
                    }
                }
            }
        }
    }

    // 2. KHI ĐÃ ĐẠT 0 LỖI (!has_err): Kích hoạt P5:Surgeon + Nắn thẳng Ortho + Thu gọn Compact
    if !has_err && rng.next_f64() < 0.88 {
        let p = parent.unwrap();
        if agent.focus == AgentFocus::Surgery && rng.next_f64() < 0.55 {
            q.persona = 4; // P5:Surgeon trực tiếp tinh chỉnh trên phôi 0 lỗi
            q.passes = Some(28);
        }
        q.ortho_weight = Some(14.0 + (rng.next_usize(16) as f64));
        if r_comp_count > 1 && rng.next_f64() < 0.75 {
            let i = rng.next_usize(r_comp_count);
            let j = rng.next_usize(r_comp_count);
            q.r_order.swap(i, j);
        }
        if p.ortho_count >= r_comp_count && q.gaps.as_ref().is_some_and(|g| !g.is_empty()) && rng.next_f64() < 0.58 {
            q.compact = true;
            q.lock_gaps = true;
            if let Some(gs) = q.gaps.as_mut() {
                let gi = rng.next_usize(gs.len());
                if gs[gi] > 0 {
                    gs[gi] -= 1;
                }
            }
            q.phase = SearchPhase::P3Compact;
        } else {
            q.phase = SearchPhase::P2Ortho;
        }
        q.net_ord = rng.next_usize(18);
        if rng.next_f64() < 0.35 {
            q.rev = !q.rev;
        }
        return q;
    }

    // 3. KHI CÒN 1..=3 LỖI (#8): Hạ nhẹ ortho_weight (2.5..6.0) + Đột biến btn_bend (-1, 0, +1) + Vi phẫu P5:Surgeon
    let parent_already_surgeon = parent.is_some_and(|p| p.opts.persona == 4 || p.opts.handed_over);
    let surg_prob = if parent_already_surgeon && stall_cnt > 8 {
        0.15
    } else {
        match agent.focus {
            AgentFocus::Surgery => 0.80,
            AgentFocus::Bridge => 0.65,
            _ => 0.48,
        }
    };

    if is_near_solved && stall_cnt <= 18 && rng.next_f64() < surg_prob {
        let p = parent.unwrap();
        q.phase = SearchPhase::P1Bridge;
        // Hạ nhẹ ortho_weight xuống 2.5..6.0 để R/LED/C linh hoạt xoay chéo cứu lỗi trước (#8)
        q.ortho_weight = Some(2.5 + rng.next_f64() * 3.5);
        if rng.next_f64() < 0.65 {
            mutate_one_btn_bend(&mut q.rot, &q.half, comps, rng);
        }
        let (boost_nets, evict_mask, raw_cols) = parse_err_info(&p.errors, parsed);
        q.boost_nets = boost_nets;
        if !parent_already_surgeon && (agent.focus == AgentFocus::Surgery || rng.next_f64() < 0.50) {
            q.persona = 4;
            q.passes = Some(32);
            q.frozen_specials = extract_frozen_map(p);
            q.evict_cols_mask = if agent.focus == AgentFocus::Surgery || rng.next_f64() < 0.75 { evict_mask } else { 0 };
            q.lock_gaps = true;
            q.handed_over = true;
        } else {
            q.persona = isl % NUM_ISLANDS;
            q.passes = Some(24);
        }
        q.net_ord = rng.next_usize(30);
        if r_comp_count > 1 {
            let i = rng.next_usize(r_comp_count);
            let j = rng.next_usize(r_comp_count);
            q.r_order.swap(i, j);
        }
        let place_items: Vec<CompId> = q.order.iter().filter_map(|&ci| comps.get(ci)).filter(|c| !c.comp_type.is_special()).map(|c| c.cid).collect();
        if (q.frozen_specials.is_empty() || agent.focus == AgentFocus::Bridge)
            && !raw_cols.is_empty()
            && q.gaps.as_ref().is_some_and(|g| g.len() == place_items.len())
        {
            let target_col = raw_cols[rng.next_usize(raw_cols.len())];
            let near_cands: Vec<CompId> = p.layout.ics.iter().map(|x| (x.cid, x.c_min, x.c_max))
                .chain(p.layout.sws.iter().map(|x| (x.cid, x.c_min, x.c_max)))
                .chain(p.layout.btns.iter().map(|x| (x.cid, x.c_min, x.c_max)))
                .filter(|&(_, c_min, c_max)| c_max >= target_col.saturating_sub(8) && c_min <= target_col.saturating_add(8))
                .map(|(cid, _, _)| cid)
                .collect();
            if !near_cands.is_empty() {
                let picked = near_cands[rng.next_usize(near_cands.len())];
                if let Some(near_idx) = place_items.iter().position(|&cid| cid == picked) {
                    q.lock_gaps = true;
                    let gs = q.gaps.as_mut().unwrap();
                    let boost = if rng.next_f64() < 0.45 && near_idx > 0 { near_idx - 1 } else { near_idx };
                    gs[boost] = (gs[boost] + 1).min(4);
                    let shrink_cands: Vec<usize> = gs.iter().enumerate().filter_map(|(idx, &g)| (idx != boost && g > 0).then_some(idx)).collect();
                    if !shrink_cands.is_empty() {
                        let s_idx = shrink_cands[rng.next_usize(shrink_cands.len())];
                        gs[s_idx] -= 1;
                    }
                }
            }
        }
        if rng.next_f64() < 0.35 {
            q.rev = !q.rev;
        }
        return q;
    }

    // 4. ĐỘT BIẾN ĐẠI CỤC TỰ NHIÊN TRÊN 4 QUẦN ĐẢO (I1..I4)
    q.phase = SearchPhase::P1Unblock;
    q.persona = isl % NUM_ISLANDS;
    q.passes = Some(PERSONAS[isl % NUM_ISLANDS].passes);
    // Nếu phôi cha còn 1..=3 lỗi, duy trì ortho_weight thấp (2.5..6.0) và thử bẻ chân BTN2 (#8)
    if is_near_solved {
        q.ortho_weight = Some(2.5 + rng.next_f64() * 3.5);
        if rng.next_f64() < 0.55 {
            mutate_one_btn_bend(&mut q.rot, &q.half, comps, rng);
        }
    } else {
        q.ortho_weight = None;
    }

    let lock_right = agent.focus == AgentFocus::LockIo;
    let arm = rl.pick_arm(agent.focus, rng);
    q.arm = Some(arm);

    let place_pos: Vec<usize> = q.order.iter().enumerate().filter_map(|(pos, &ci)| {
        let c = comps.get(ci)?;
        if c.comp_type.is_special() || (lock_right && (c.id == "U1" || c.comp_type != CompType::IC)) {
            return None;
        }
        if isl != 1 && arm <= 1 && c.comp_type != CompType::IC && rng.next_f64() < 0.75 {
            return None;
        }
        Some(pos)
    }).collect();

    let ic_cids: Vec<CompId> = comps.iter().filter(|c| c.comp_type == CompType::IC && !(lock_right && c.id == "U1")).map(|c| c.cid).collect();
    let io_comps: Vec<(CompId, CompType)> = comps.iter().filter(|c| c.comp_type.is_io()).map(|c| (c.cid, c.comp_type)).collect();

    match arm {
        0 if place_pos.len() > 1 => {
            let i = place_pos[rng.next_usize(place_pos.len())];
            let j = place_pos[rng.next_usize(place_pos.len())];
            q.order.swap(i, j);
            q.gaps = None;
        }
        1 if place_pos.len() > 1 => {
            let i = place_pos[rng.next_usize(place_pos.len())];
            let dest = place_pos[rng.next_usize(place_pos.len())];
            let v = q.order.remove(i);
            q.order.insert(dest.min(q.order.len()), v);
            q.gaps = None;
        }
        2 if !ic_cids.is_empty() => {
            let cid = ic_cids[rng.next_usize(ic_cids.len())];
            let cur = q.rot.get(&cid).copied().unwrap_or(0);
            q.rot.insert(cid, u8::from(cur == 0));
            q.rot_mutated = true;
        }
        3 if q.gaps.as_ref().is_some_and(|g| !g.is_empty()) => {
            q.lock_gaps = true;
            let gs = q.gaps.as_mut().unwrap();
            let gi = rng.next_usize(gs.len());
            let gj = rng.next_usize(gs.len());
            gs[gi] = (gs[gi] + 1).min(4);
            if gs[gj] > 0 {
                gs[gj] -= 1;
            }
        }
        4 if !io_comps.is_empty() && !lock_right => {
            let (cid, ct) = io_comps[rng.next_usize(io_comps.len())];
            let def_h = u8::from(ct == CompType::SW);
            let cur = q.half.get(&cid).copied().unwrap_or(def_h);
            q.half.insert(cid, u8::from(cur == 0));
            if rng.next_f64() < 0.65 {
                rebalance_io_halves(&mut q.half, comps, rng);
            }
            if ct == CompType::BTN2 || rng.next_f64() < 0.45 {
                mutate_one_btn_bend(&mut q.rot, &q.half, comps, rng);
            }
            q.rot_mutated = true;
        }
        _ => {
            if rng.next_f64() < 0.45 && ic_cids.len() > 1 && !lock_right {
                for &cid in &ic_cids {
                    let cur = q.rot.get(&cid).copied().unwrap_or(0);
                    q.rot.insert(cid, u8::from(cur == 0));
                }
                q.rot_mutated = true;
            }
            if r_comp_count > 1 {
                let i = rng.next_usize(r_comp_count);
                let j = rng.next_usize(r_comp_count);
                q.r_order.swap(i, j);
            }
            q.rev = !q.rev;
            q.net_ord = rng.next_usize(24);
        }
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::circuit::BoardLayout;
    use crate::parser::parse_netlist;

    #[test]
    fn test_mutator_io_symmetry_and_near_solved_btn_bend_ortho_lowering() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_other(CompId(1), "S1", CompType::SW),
            CompSpec::new_other(CompId(2), "S2", CompType::SW),
            CompSpec::new_other(CompId(3), "S3", CompType::SW),
            CompSpec::new_other(CompId(4), "S4", CompType::SW),
            CompSpec::new_other(CompId(5), "B1", CompType::BTN2),
            CompSpec::new_other(CompId(6), "B2", CompType::BTN2),
            CompSpec::new_other(CompId(7), "R1", CompType::R),
        ];
        let parsed = parse_netlist("VCC U1:14; GND U1:7; U1:1 -> R1 -> B1:1; B1:2 -> GND", &comps);
        let mut rng = Mulberry32::new(7);

        // 1. Kiểm tra cân bằng đối xứng 3 Trên – 3 Dưới cho 6 linh kiện IO (4 SW + 2 BTN2) (#3)
        let mut half = FxHashMap::default();
        for id in 1..=6 {
            half.insert(CompId(id), 1); // Ban đầu dồn cả 6 IO xuống nửa dưới
        }
        rebalance_io_halves(&mut half, &comps, &mut rng);
        let top_cnt = (1..=6).filter(|&id| half.get(&CompId(id)) == Some(&0)).count();
        let bot_cnt = (1..=6).filter(|&id| half.get(&CompId(id)) == Some(&1)).count();
        assert_eq!((top_cnt, bot_cnt), (3, 3));

        // 2. Kiểm tra khi phôi cha còn 1..=3 lỗi: hạ nhẹ ortho_weight (2.5..6.0) và đột biến btn_bend (#8)
        let base_opts = CandidateOpts {
            order: (0..comps.len()).collect(),
            half,
            ..Default::default()
        };
        let parent_sol = BoardSolution {
            layout: BoardLayout::default(),
            wires: Vec::new(),
            fast_wires: Vec::new(),
            errors: vec![
                "Net NET1: không đi được dây từ cột 5 tới cột 12".into(),
                "Net NET2: không đi được dây từ cột 8 tới cột 15".into(),
                "Net NET3: không đi được dây từ cột 10 tới cột 20".into(),
            ],
            warns: Vec::new(),
            comp_nets: FxHashMap::default(),
            nets: 3,
            len: 20,
            raw_len: 20.0,
            max_c: 20,
            score: 3e8,
            unplaced: 0,
            ortho_count: 1,
            persona: "I3:BridgeLED".into(),
            opts: base_opts.clone(),
            sig: 123,
            layout_hash: 456,
        };

        let mut agent = MetaAgent::default();
        agent.focus = AgentFocus::Surgery;
        let tabu = TabuBasin::new();
        let rl = RlBandit::new(comps.len());

        let mut mutated_bend = false;
        for _ in 0..10 {
            let child = mutate_candidate(
                &base_opts,
                Some(&parent_sol),
                2,
                None,
                2,
                10,
                7,
                1,
                &agent,
                &tabu,
                &rl,
                &comps,
                &parsed,
                &mut rng,
            );
            let ow = child.ortho_weight.expect("ortho_weight must be lowered on 1..=3 errors");
            assert!((2.5..=6.0).contains(&ow), "ortho_weight out of 2.5..=6.0: {ow}");
            if child.rot.contains_key(&CompId(5)) || child.rot.contains_key(&CompId(6)) {
                mutated_bend = true;
            }
        }
        assert!(mutated_bend, "BTN2 bend should be mutated when parent has 1..=3 errors");
    }
}
