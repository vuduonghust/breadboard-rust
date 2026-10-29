use crate::config::weights::CostWeights;
use crate::macro_place::ordering::{best_assignments, conn_order, MacroAssignment};
use crate::macro_place::topology::{build_topology_cache, TopologyCache};
use crate::meta_search::builder::{
    build_board, cmp_solutions, extract_frozen_map, parse_err_info, BoardSolution, CandidateOpts,
    SearchPhase,
};
use crate::meta_search::knowledge::KnowledgeBank;
use crate::meta_search::logger::SearchLogger;
use crate::meta_search::mutator::{apply_macro_jump, mutate_candidate, polish_one_ic};
use crate::meta_search::rl_bandit::{AgentFocus, MetaAgent, RlBandit, SteerMode, NUM_ISLANDS};
use crate::meta_search::tabu_basin::TabuBasin;
use crate::meta_search::zobrist::{macro_handover_sig, tabu_key};
use crate::model::circuit::{CompSpec, ParsedNetlist};
use crate::model::ids::{CompType, Mulberry32};
use crate::parser::parse_netlist;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Chọn `k` phương án tốt nhất và đa dạng nhất từ `pool` (`pickTop` trong main.html)
pub fn pick_top_solutions(pool: &[BoardSolution], k: usize) -> Vec<BoardSolution> {
    let mut sorted = pool.to_vec();
    sorted.sort_by(cmp_solutions);
    let mut out = Vec::with_capacity(k);
    let mut used_layout = FxHashSet::default();
    let mut used_persona = FxHashSet::default();
    let mut picked_sigs = FxHashSet::default();

    for r in &sorted {
        if !used_layout.contains(&r.layout_hash) && !used_persona.contains(r.persona.as_str()) {
            used_layout.insert(r.layout_hash);
            used_persona.insert(r.persona.as_str());
            picked_sigs.insert(r.sig);
            out.push(r.clone());
            if out.len() == k {
                return out;
            }
        }
    }
    for r in &sorted {
        if used_layout.insert(r.layout_hash) && picked_sigs.insert(r.sig) {
            out.push(r.clone());
            if out.len() == k {
                return out;
            }
        }
    }
    for r in sorted {
        if out.len() >= k {
            break;
        }
        if picked_sigs.insert(r.sig) {
            out.push(r);
        }
    }
    out
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchStepResult {
    pub improved: bool,
    pub iterations: u32,
    pub best_score: f64,
    pub best_err_count: usize,
    pub best_unplaced: usize,
    pub best_ortho: usize,
    pub focus: String,
    pub tabu_bans: u32,
    pub island_status: String,
}

/// Bộ điều phối Tiến hóa Đa Quần Đảo & Vi Phẫu (`solveAsync` trong main.html)
pub struct MetaSolver {
    pub comps: Vec<CompSpec>,
    pub parsed: ParsedNetlist,
    pub topo: TopologyCache,
    pub weights: CostWeights,
    pub use_star_hub: bool,
    pub pool: Vec<BoardSolution>,
    pub pool_seen: FxHashSet<u64>,
    pub island_pools: [Vec<BoardSolution>; NUM_ISLANDS],
    pub island_stall: [u32; NUM_ISLANDS],
    pub todo: VecDeque<CandidateOpts>,
    pub handover_queue: VecDeque<CandidateOpts>,
    pub handover_seen: FxHashSet<u64>,
    pub tabu: TabuBasin,
    pub agent: MetaAgent,
    pub rl: RlBandit,
    pub kb: KnowledgeBank,
    pub logger: SearchLogger,
    pub rng: Mulberry32,
    pub it: u32,
    pub stall: u32,
    pub rr_tick: usize,
    place_comp_count: usize,
    r_comp_count: usize,
    go: Vec<usize>,
    go_interleaved: Vec<usize>,
    asg: Vec<MacroAssignment>,
    asg_inter: Vec<MacroAssignment>,
}

impl MetaSolver {
    pub fn new(
        comps: Vec<CompSpec>,
        netlist_text: &str,
        use_star_hub: bool,
        seed: u32,
        mut kb: KnowledgeBank,
    ) -> Self {
        let parsed = parse_netlist(netlist_text, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        let weights = CostWeights::default();
        let r_comp_count = comps.iter().filter(|c| c.comp_type.is_special()).count();
        let place_comp_count = comps.len() - r_comp_count;
        let mut rl = RlBandit::new(comps.len());
        let mut rng = Mulberry32::new(seed);

        let go = conn_order(&parsed, &topo, false, None, None, &comps, &weights);
        let go_interleaved = conn_order(&parsed, &topo, true, None, None, &comps, &weights);
        let asg = best_assignments(&parsed, &topo, 16, &go, &comps, &weights);
        let asg_inter = best_assignments(&parsed, &topo, 10, &go_interleaved, &comps, &weights);

        // Hạng mục #3: Đồng bộ default_half chia đều đối xứng 2 nửa bo cho cả SW và BTN2 (is_io())
        let (mut user_rot, mut default_half, mut io_idx) =
            (FxHashMap::default(), FxHashMap::default(), 0usize);
        for c in &comps {
            if c.comp_type == CompType::IC {
                user_rot.insert(c.cid, c.rot);
            } else if c.comp_type.is_io() {
                default_half.insert(c.cid, (io_idx % 2) as u8);
                io_idx += 1;
            }
        }

        let mut todo = VecDeque::new();
        for (i, mut s) in kb
            .build_warm_start_queue(&mut rl, &go, place_comp_count, r_comp_count, &mut rng, &comps)
            .into_iter()
            .enumerate()
        {
            s.island = i % NUM_ISLANDS;
            s.persona = i % NUM_ISLANDS;
            s.use_star_hub = use_star_hub;
            todo.push_back(s);
        }

        let idx_all: Vec<usize> = (0..comps.len()).collect();
        for (isl, ord, sr) in [
            (0, go.clone(), 1),
            (1, go_interleaved.clone(), 2),
            (2, go.clone(), 3),
            (3, idx_all, 4),
        ] {
            todo.push_back(CandidateOpts {
                order: ord,
                rot: user_rot.clone(),
                half: default_half.clone(),
                gap: Some(1),
                island: isl,
                persona: isl,
                sr: Some(sr),
                use_star_hub,
                phase: SearchPhase::Init,
                ..Default::default()
            });
        }
        for (i, a) in asg.iter().enumerate() {
            let isl = match i % 3 {
                0 => 0,
                1 => 2,
                _ => 3,
            };
            let use_ord = if a.order.is_empty() { go.clone() } else { a.order.clone() };
            let isl2 = (isl + 2) % NUM_ISLANDS;
            todo.push_back(CandidateOpts {
                order: use_ord.clone(),
                rot: a.rot.clone(),
                half: a.half.clone(),
                gap: Some(1),
                island: isl,
                persona: isl,
                sr: Some(10 + i as u32),
                use_star_hub,
                phase: SearchPhase::Init,
                ..Default::default()
            });
            todo.push_back(CandidateOpts {
                order: use_ord,
                rot: a.rot.clone(),
                half: a.half.clone(),
                gap: Some(2),
                island: isl2,
                persona: isl2,
                sr: Some(40 + i as u32),
                net_ord: 1,
                use_star_hub,
                phase: SearchPhase::Init,
                ..Default::default()
            });
        }
        for (i, a) in asg_inter.iter().enumerate() {
            let use_ord = if a.order.is_empty() {
                go_interleaved.clone()
            } else {
                a.order.clone()
            };
            for (gap, sr_base, net_ord) in [(1, 80u32, 0usize), (2, 100u32, 2usize)] {
                todo.push_back(CandidateOpts {
                    order: use_ord.clone(),
                    rot: a.rot.clone(),
                    half: a.half.clone(),
                    gap: Some(gap),
                    island: 1,
                    persona: 1,
                    sr: Some(sr_base + i as u32),
                    net_ord,
                    use_star_hub,
                    phase: SearchPhase::Init,
                    ..Default::default()
                });
            }
        }

        let logger = SearchLogger::new(parsed.nets.len());
        kb.rebuild_flat_seeds();
        Self {
            comps,
            parsed,
            topo,
            weights,
            use_star_hub,
            pool: Vec::new(),
            pool_seen: FxHashSet::default(),
            island_pools: Default::default(),
            island_stall: [0; NUM_ISLANDS],
            todo,
            handover_queue: VecDeque::new(),
            handover_seen: FxHashSet::default(),
            tabu: TabuBasin::new(),
            agent: MetaAgent::default(),
            rl,
            kb,
            logger,
            rng,
            it: 0,
            stall: 0,
            rr_tick: 0,
            place_comp_count,
            r_comp_count,
            go,
            go_interleaved,
            asg,
            asg_inter,
        }
    }

    pub fn next_candidate(&mut self) -> CandidateOpts {
        if let Some(mut ho) = self.handover_queue.pop_front() {
            ho.use_star_hub = self.use_star_hub;
            return ho;
        }
        if let Some(mut o) = self.todo.pop_front() {
            o.use_star_hub = self.use_star_hub;
            return o;
        }

        let isl = self.rl.pick_island(self.rr_tick, self.agent.focus, &mut self.rng);
        self.rr_tick += 1;
        let i_stall = self.island_stall[isl];
        let sub_len = self.island_pools[isl].len();
        let top_window = sub_len.min(if i_stall > 30 { 16 } else { 6 });

        if top_window > 0 && self.rng.next_f64() < 0.80 {
            let pick_non_tabu = |rng: &mut Mulberry32,
                                 pool_slice: &[BoardSolution],
                                 win: usize,
                                 tabu: &TabuBasin,
                                 comps: &[CompSpec],
                                 cur_it: u32|
             -> usize {
                let mut idx = rng.next_usize(win);
                for _ in 0..4 {
                    let o = &pool_slice[idx].opts;
                    if !tabu.is_tabu(tabu_key(&o.order, &o.rot, &o.half, comps), cur_it) {
                        return idx;
                    }
                    idx = rng.next_usize(pool_slice.len());
                }
                idx
            };
            let p1_idx = pick_non_tabu(
                &mut self.rng, &self.island_pools[isl], top_window, &self.tabu, &self.comps, self.it,
            );
            let p2_idx = pick_non_tabu(
                &mut self.rng, &self.island_pools[isl], top_window, &self.tabu, &self.comps, self.it,
            );
            let p1 = self.island_pools[isl][p1_idx].clone();
            let p2 = self.island_pools[isl][p2_idx].clone();
            let (parent, fallback_mate) = if cmp_solutions(&p1, &p2).is_le() { (p1, p2) } else { (p2, p1) };
            let other_isl = (isl + 1 + self.rng.next_usize(NUM_ISLANDS - 1)) % NUM_ISLANDS;
            let mate = if !self.island_pools[other_isl].is_empty() && self.rng.next_f64() < 0.25 {
                self.island_pools[other_isl][0].clone()
            } else {
                fallback_mate
            };
            let mut o = mutate_candidate(
                &parent.opts, Some(&parent), i_stall, Some(&mate), isl, self.it,
                self.place_comp_count, self.r_comp_count, &self.agent, &self.tabu, &self.rl, &self.comps, &self.parsed, &mut self.rng,
            );
            if o.phase == SearchPhase::P1Unblock {
                polish_one_ic(
                    &mut o, self.agent.focus, &self.comps, &self.parsed, &self.topo, &self.weights, &mut self.rng,
                );
            }
            o.use_star_hub = self.use_star_hub;
            return o;
        }

        if !self.kb.seeds.is_empty() && self.rng.next_f64() < 0.30 && self.agent.focus != AgentFocus::Macro {
            let seed_dna = self.kb.seeds[self.rng.next_usize(self.kb.seeds.len().min(10))].clone();
            let fb_ord = if isl == 1 { &self.go_interleaved } else { &self.go };
            let mapped = self.kb.to_opts(
                &seed_dna, fb_ord, self.place_comp_count, self.r_comp_count, &mut self.rng, false, &self.comps,
            );
            if !self.tabu.is_tabu(tabu_key(&mapped.order, &mapped.rot, &mapped.half, &self.comps), self.it) {
                let mut o = mutate_candidate(
                    &mapped, None, 0, None, isl, self.it,
                    self.place_comp_count, self.r_comp_count, &self.agent, &self.tabu, &self.rl, &self.comps, &self.parsed, &mut self.rng,
                );
                o.use_star_hub = self.use_star_hub;
                return o;
            }
        }

        let base_ord = if isl == 1 { &self.go_interleaved } else { &self.go };
        let a_list = if isl == 1 && !self.asg_inter.is_empty() { &self.asg_inter } else { &self.asg };
        let picked_a = (!a_list.is_empty() && self.rng.next_f64() < 0.55)
            .then(|| a_list[self.rng.next_usize(a_list.len())].clone());
        let order = if self.rng.next_f64() < 0.48 {
            base_ord.clone()
        } else if self.rng.next_f64() < 0.70 {
            self.rl.build_guided_order(&self.comps, &mut self.rng)
        } else {
            let mut v: Vec<usize> = (0..self.comps.len()).collect();
            self.rng.shuffle(&mut v);
            v
        };

        let (rot, half) = if let Some(a) = picked_a {
            (a.rot, a.half)
        } else {
            let (mut r_map, mut h_map) = (FxHashMap::default(), FxHashMap::default());
            for c in &self.comps {
                if c.comp_type == CompType::IC {
                    r_map.insert(
                        c.cid,
                        self.rl.sample_binary(self.rl.q_rot[c.cid.0 as usize], 0.30, &mut self.rng),
                    );
                } else if c.comp_type.is_io() {
                    h_map.insert(
                        c.cid,
                        self.rl.sample_binary(self.rl.q_half[c.cid.0 as usize], 0.30, &mut self.rng),
                    );
                }
            }
            (r_map, h_map)
        };
        let mut r_order: Vec<usize> = (0..self.r_comp_count).collect();
        self.rng.shuffle(&mut r_order);

        CandidateOpts {
            island: isl,
            persona: isl,
            order,
            rot,
            half,
            r_order,
            dyn_br_pen: self.agent.dyn_br_pen,
            dyn_aux_share: self.agent.dyn_aux_share,
            rev: self.rng.next_f64() < 0.5,
            net_ord: self.rng.next_usize(18),
            sr: Some(self.rng.next_usize(1_000_000_000) as u32),
            r_seed: Some(self.rng.next_usize(1_000_000_000) as u32),
            use_star_hub: self.use_star_hub,
            phase: SearchPhase::Diversify,
            ..Default::default()
        }
    }

    fn check_and_break_stall(&mut self, isl: usize) {
        if self.island_stall[isl] > 55 {
            if let Some(isl_top) = self.island_pools[isl].first().filter(|s| !s.errors.is_empty()) {
                let tk = tabu_key(&isl_top.opts.order, &isl_top.opts.rot, &isl_top.opts.half, &self.comps);
                self.tabu.force_ban(tk, self.it);
            }
            self.island_stall[isl] = 10;
        }
        if self.stall > 110 {
            if let Some(top) = self.pool.first().filter(|s| !s.errors.is_empty()).cloned() {
                let tk = tabu_key(&top.opts.order, &top.opts.rot, &top.opts.half, &self.comps);
                self.tabu.force_ban(tk, self.it);
                self.rl.penalize_force_ban(&top.opts.order, &top.opts.rot, &self.comps);
                for k in 0..4 {
                    let mut jumped = top.opts.clone();
                    jumped.island = k % NUM_ISLANDS;
                    jumped.persona = k % NUM_ISLANDS;
                    jumped.sr = Some(self.rng.next_usize(1_000_000_000) as u32);
                    jumped.r_seed = Some(self.rng.next_usize(1_000_000_000) as u32);
                    apply_macro_jump(&mut jumped, self.agent.focus, &self.comps, &mut self.rng);
                    self.handover_queue.push_back(jumped);
                }
            }
            self.stall = 15;
        }
    }

    pub fn ingest_result(&mut self, sol: BoardSolution, elapsed_ms: u64) -> bool {
        let o = sol.opts.clone();
        self.it += 1;
        if self.it.is_multiple_of(40) {
            let br_cnt = self.logger.best_snap.as_ref().map_or(9, |s| s.bridges.len());
            let err_opt = self.logger.best_snap.as_ref().map(|s| s.err_count);
            self.agent.reflect(self.stall, br_cnt, err_opt);
        }

        let isl = o.island % NUM_ISLANDS;
        let isl_improved = self.island_pools[isl].first().is_none_or(|best| cmp_solutions(&sol, best).is_lt());
        let t_key = tabu_key(&o.order, &o.rot, &o.half, &self.comps);
        let newly_tabu = sol.unplaced == 0
            && !sol.errors.is_empty()
            && sol.errors.len() <= 3
            && self.tabu.record(t_key, sol.score, self.it);

        self.rl.update(
            sol.unplaced, sol.errors.len(), sol.score, o.parent_score, isl_improved,
            newly_tabu, isl, o.arm, &o.order, &o.rot, &o.half, &self.comps,
        );

        let is_dup = self.pool_seen.contains(&sol.sig);
        self.logger.record(&sol, &o, self.it, elapsed_ms, is_dup);
        if is_dup {
            self.stall += 1;
            self.island_stall[isl] += 1;
            self.check_and_break_stall(isl);
            return false;
        }

        if sol.unplaced == 0
            && !sol.errors.is_empty()
            && sol.errors.len() <= 3
            && !o.handed_over
            && !self.tabu.is_tabu(t_key, self.it)
            && self.handover_queue.len() < 12
        {
            let m_sig = macro_handover_sig(
                &o.order, &o.rot, &o.half, o.gaps.as_deref().unwrap_or(&[]), sol.errors.len(), &self.comps,
            );
            if self.handover_seen.insert(m_sig) {
                if self.handover_seen.len() > 2000 {
                    self.handover_seen.clear();
                }
                let frozen_map = extract_frozen_map(&sol);
                let (boost_nets, evict_mask, _) = parse_err_info(&sol.errors, &self.parsed);

                self.handover_queue.push_back(CandidateOpts {
                    island: isl, persona: 4, passes: Some(32), lock_gaps: o.gaps.is_some(),
                    frozen_specials: frozen_map.clone(), evict_cols_mask: 0, boost_nets: boost_nets.clone(),
                    dyn_br_pen: self.agent.dyn_br_pen, dyn_aux_share: self.agent.dyn_aux_share,
                    net_ord: self.rng.next_usize(25), sr: Some(self.rng.next_usize(1_000_000_000) as u32),
                    r_seed: Some(self.rng.next_usize(1_000_000_000) as u32), phase: SearchPhase::P1Bridge,
                    handed_over: true, parent_score: Some(sol.score), ..o.clone()
                });
                if evict_mask != 0 {
                    self.handover_queue.push_back(CandidateOpts {
                        island: isl, persona: 4, passes: Some(32), lock_gaps: o.gaps.is_some(),
                        frozen_specials: frozen_map, evict_cols_mask: evict_mask, boost_nets: boost_nets.clone(),
                        dyn_br_pen: Some(40.0), dyn_aux_share: Some(25.0), net_ord: self.rng.next_usize(25),
                        rev: !o.rev, sr: Some(self.rng.next_usize(1_000_000_000) as u32),
                        r_seed: Some(self.rng.next_usize(1_000_000_000) as u32), phase: SearchPhase::P1Bridge,
                        handed_over: true, parent_score: Some(sol.score), ..o.clone()
                    });
                }
                let alt_isl = if isl == 2 { 3 } else { 2 };
                self.handover_queue.push_back(CandidateOpts {
                    island: alt_isl, persona: alt_isl, passes: Some(26), lock_gaps: o.gaps.is_some(),
                    frozen_specials: Default::default(), evict_cols_mask: 0, boost_nets,
                    dyn_br_pen: Some(38.0), dyn_aux_share: Some(24.0), net_ord: self.rng.next_usize(25),
                    sr: Some(self.rng.next_usize(1_000_000_000) as u32), r_seed: Some(self.rng.next_usize(1_000_000_000) as u32),
                    phase: SearchPhase::P1Bridge, handed_over: true, parent_score: Some(sol.score), ..o
                });
            }
        }

        if self.pool_seen.len() > 12000 {
            self.pool_seen.clear();
        }
        self.pool_seen.insert(sol.sig);

        let sub = &mut self.island_pools[isl];
        sub.push(sol.clone());
        sub.sort_by(cmp_solutions);
        sub.truncate(25);
        if isl_improved {
            self.island_stall[isl] = 0;
        } else {
            self.island_stall[isl] += 1;
        }

        let imp = self.pool.first().is_none_or(|best| cmp_solutions(&sol, best).is_lt());
        self.pool.push(sol.clone());
        self.pool.sort_by(cmp_solutions);
        self.pool.truncate(80);

        if imp {
            self.stall = 0;
            let br_cnt = self.logger.best_snap.as_ref().map_or(9, |s| s.bridges.len());
            self.agent.reflect(0, br_cnt, Some(sol.errors.len()));
            if sol.unplaced == 0 && sol.errors.len() <= 3 {
                if let Some(dna) = KnowledgeBank::extract_from_solution(&sol) {
                    self.kb.add_seed(dna);
                }
            }
        } else {
            self.stall += 1;
            self.check_and_break_stall(isl);
        }
        imp
    }

    pub fn format_island_status(&self) -> String {
        self.island_pools
            .iter()
            .enumerate()
            .map(|(i, sp)| match sp.first() {
                None => format!("I{}:-", i + 1),
                Some(s) if s.unplaced > 0 => format!("I{}:thiếu", i + 1),
                Some(s) => format!("I{}:{}L", i + 1, s.errors.len()),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    pub fn step_batch(&mut self, batch_size: usize, elapsed_ms: u64) -> BatchStepResult {
        let mut any_improved = false;
        for _ in 0..batch_size.max(1) {
            let mut cand = self.next_candidate();
            let sol = build_board(&mut cand, &self.comps, &self.parsed, &self.topo, &self.weights);
            if self.ingest_result(sol, elapsed_ms) {
                any_improved = true;
            }
        }
        let best = self.pool.first();
        BatchStepResult {
            improved: any_improved,
            iterations: self.it,
            best_score: best.map_or(f64::INFINITY, |s| s.score),
            best_err_count: best.map_or(0, |s| s.errors.len()),
            best_unplaced: best.map_or(0, |s| s.unplaced),
            best_ortho: best.map_or(0, |s| s.ortho_count),
            focus: self.agent.focus.as_str().to_string(),
            tabu_bans: self.tabu.ban_count,
            island_status: self.format_island_status(),
        }
    }

    pub fn set_steer_mode(&mut self, mode: SteerMode) {
        self.agent.mode = mode;
        let br_cnt = self.logger.best_snap.as_ref().map_or(9, |s| s.bridges.len());
        let err_opt = self.logger.best_snap.as_ref().map(|s| s.err_count);
        self.agent.reflect(self.stall, br_cnt, err_opt);
    }

    pub fn trigger_tabu_jump(&mut self, tab_idx: usize) {
        let top = pick_top_solutions(&self.pool, 5);
        let Some(cur_sol) = top.get(tab_idx).or_else(|| self.pool.first()) else { return };
        let tk = tabu_key(&cur_sol.opts.order, &cur_sol.opts.rot, &cur_sol.opts.half, &self.comps);
        self.tabu.force_ban(tk, self.it);
        self.rl.penalize_force_ban(&cur_sol.opts.order, &cur_sol.opts.rot, &self.comps);
        self.handover_queue.clear();
        for k in 0..8 {
            let mut jumped = cur_sol.opts.clone();
            jumped.island = k % NUM_ISLANDS;
            jumped.persona = k % NUM_ISLANDS;
            jumped.sr = Some(self.rng.next_usize(1_000_000_000) as u32);
            jumped.r_seed = Some(self.rng.next_usize(1_000_000_000) as u32);
            apply_macro_jump(&mut jumped, self.agent.focus, &self.comps, &mut self.rng);
            self.handover_queue.push_back(jumped);
        }
        self.stall = 0;
    }

    pub fn trigger_deep_surgery(&mut self, tab_idx: usize) {
        let top = pick_top_solutions(&self.pool, 5);
        let Some(cur_sol) = top.get(tab_idx).or_else(|| self.pool.first()) else { return };
        let frozen_map = extract_frozen_map(cur_sol);
        let (boost_nets, evict_mask, _) = parse_err_info(&cur_sol.errors, &self.parsed);
        for k in (0..6).rev() {
            self.handover_queue.push_front(CandidateOpts {
                persona: if k < 4 { 4 } else { 2 },
                passes: Some(36),
                lock_gaps: true,
                frozen_specials: if k % 2 == 0 { frozen_map.clone() } else { Default::default() },
                evict_cols_mask: evict_mask,
                boost_nets: boost_nets.clone(),
                dyn_br_pen: Some(42.0),
                dyn_aux_share: Some(26.0),
                net_ord: self.rng.next_usize(30),
                rev: k % 2 == 1,
                sr: Some(self.rng.next_usize(1_000_000_000) as u32),
                r_seed: Some(self.rng.next_usize(1_000_000_000) as u32),
                phase: SearchPhase::P1Bridge,
                handed_over: true,
                parent_score: Some(cur_sol.score),
                ..cur_sol.opts.clone()
            });
        }
    }

    pub fn pick_top(&self, k: usize) -> Vec<BoardSolution> {
        pick_top_solutions(&self.pool, k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ids::CompId;

    #[test]
    fn test_meta_solver_default_half_balances_sw_and_btn2() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_other(CompId(1), "S1", CompType::SW),
            CompSpec::new_other(CompId(2), "S2", CompType::SW),
            CompSpec::new_other(CompId(3), "S3", CompType::SW),
            CompSpec::new_other(CompId(4), "S4", CompType::SW),
            CompSpec::new_other(CompId(5), "B1", CompType::BTN2),
            CompSpec::new_other(CompId(6), "B2", CompType::BTN2),
        ];
        let txt = "VCC U1:14; GND U1:7; S1:2 -> U1:1; S2:2 -> U1:2; S3:2 -> U1:3; S4:2 -> U1:4; B1:1 -> U1:5; B2:1 -> U1:6";
        let solver = MetaSolver::new(comps, txt, true, 42, KnowledgeBank::new());

        let first_init = solver.todo.front().expect("todo queue must have initial candidates");
        assert_eq!(first_init.half.len(), 6);
        let top_cnt = (1..=6).filter(|&id| first_init.half.get(&CompId(id)) == Some(&0)).count();
        let bot_cnt = (1..=6).filter(|&id| first_init.half.get(&CompId(id)) == Some(&1)).count();
        assert_eq!((top_cnt, bot_cnt), (3, 3));
    }
}
