use crate::meta_search::builder::{BoardSolution, CandidateOpts, SearchPhase};
use crate::meta_search::rl_bandit::RlBandit;
use crate::model::circuit::CompSpec;
use crate::model::ids::{CompId, CompType, Mulberry32};
use core::cmp::Ordering;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

#[inline]
fn is_ic_id(id: &str) -> bool {
    id.starts_with('U') || id.starts_with("IC")
}

fn hash_dna_variant(
    macro_ids: &[String],
    rot: &FxHashMap<String, u8>,
    half: &FxHashMap<String, u8>,
    gaps: Option<&Vec<i32>>,
    ic_gaps: &[i32],
) -> u64 {
    let mut h = 0xcbf29ce484222325_u64;
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x00000100000001b3);
    };
    for id in macro_ids {
        for &b in id.as_bytes() {
            mix(b);
        }
        mix(b'|');
        if is_ic_id(id) {
            mix(rot.get(id).copied().unwrap_or(0));
        } else {
            mix(half.get(id).copied().unwrap_or(0));
        }
    }
    if let Some(gs) = gaps {
        for &g in gs {
            mix((g.clamp(0, 15) as u8) | 0x80);
        }
    }
    for &ig in ic_gaps {
        mix((ig.clamp(0, 15) as u8) | 0x40);
    }
    h
}

/// Cấu trúc DNA 3 tầng của một bố cục (`dna` trong `KnowledgeBank` - main.html)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BackboneDna {
    pub macro_ids: Vec<String>,
    pub ic_ids: Vec<String>,
    pub io_ids: Vec<String>,
    pub ic_key: String,
    pub var_sig: u64,
    pub ic_gaps: Vec<i32>,
    pub gaps: Option<Vec<i32>>,
    pub rot: FxHashMap<String, u8>,
    pub half: FxHashMap<String, u8>,
    pub r_ids: Vec<String>,
    pub err_count: usize,
    pub ortho_count: usize,
    pub score: f64,
    pub arch_code: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ArchetypeGroup {
    pub ic_key: String,
    pub ic_ids: Vec<String>,
    pub rot: FxHashMap<String, u8>,
    pub arch_code: String,
    pub variants: Vec<BackboneDna>,
}

/// Kho Tri Thức Học Phân Tầng 3 Cấp (`KnowledgeBank` trong main.html)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KnowledgeBank {
    pub archetypes: FxHashMap<String, ArchetypeGroup>,
    pub seeds: Vec<BackboneDna>,
}

impl KnowledgeBank {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cmp_dna(a: &BackboneDna, b: &BackboneDna) -> Ordering {
        if a.err_count != b.err_count {
            return a.err_count.cmp(&b.err_count);
        }
        if b.ortho_count != a.ortho_count {
            return b.ortho_count.cmp(&a.ortho_count);
        }
        a.score.total_cmp(&b.score)
    }

    pub fn normalize_dna(mut dna: BackboneDna) -> Option<BackboneDna> {
        if dna.macro_ids.is_empty() {
            return None;
        }
        dna.ic_ids = dna.macro_ids.iter().filter(|id| is_ic_id(id)).cloned().collect();
        if dna.ic_ids.is_empty() {
            return None;
        }
        dna.io_ids = dna.macro_ids.iter().filter(|id| !is_ic_id(id)).cloned().collect();
        dna.ic_key = dna
            .ic_ids
            .iter()
            .map(|id| format!("{id}:{}", if dna.rot.get(id).copied().unwrap_or(0) != 0 { "R" } else { "0" }))
            .collect::<Vec<_>>()
            .join(" -> ");
        dna.var_sig = hash_dna_variant(&dna.macro_ids, &dna.rot, &dna.half, dna.gaps.as_ref(), &dna.ic_gaps);
        if dna.score == 0.0 && dna.err_count > 0 {
            dna.score = (dna.err_count as f64) * 1e8;
        }
        Some(dna)
    }

    pub fn rebuild_flat_seeds(&mut self) {
        let mut groups: Vec<&mut ArchetypeGroup> = self.archetypes.values_mut().collect();
        groups.sort_by(|a, b| Self::cmp_dna(&a.variants[0], &b.variants[0]));
        for (idx, g) in groups.iter_mut().enumerate() {
            g.arch_code = format!("IC-DNA #{}", idx + 1);
        }
        let mut flat = Vec::with_capacity(80);
        for tier in 0..2 {
            for g in &mut groups {
                if let Some(v) = g.variants.get_mut(tier) {
                    v.arch_code = g.arch_code.clone();
                    flat.push(v.clone());
                }
            }
        }
        flat.truncate(80);
        self.seeds = flat;
    }

    pub fn add_seed(&mut self, raw_dna: BackboneDna) -> bool {
        let Some(dna) = Self::normalize_dna(raw_dna) else { return false };
        let grp = self.archetypes.entry(dna.ic_key.clone()).or_insert_with(|| ArchetypeGroup {
            ic_key: dna.ic_key.clone(),
            ic_ids: dna.ic_ids.clone(),
            rot: dna.rot.clone(),
            arch_code: String::new(),
            variants: Vec::new(),
        });
        if grp.variants.iter().any(|v| v.var_sig == dna.var_sig) {
            return false;
        }
        grp.variants.push(dna);
        grp.variants.sort_by(Self::cmp_dna);
        grp.variants.truncate(2);

        if self.archetypes.len() > 36 {
            let worst_key = self
                .archetypes
                .iter()
                .max_by(|a, b| Self::cmp_dna(&a.1.variants[0], &b.1.variants[0]))
                .map(|(k, _)| k.clone());
            if let Some(k) = worst_key {
                self.archetypes.remove(&k);
            }
        }
        self.rebuild_flat_seeds();
        true
    }

    pub fn extract_from_solution(sol: &BoardSolution) -> Option<BackboneDna> {
        let mut ics_sorted = sol.layout.ics.clone();
        ics_sorted.sort_by_key(|x| x.c_min);
        let mut ic_gaps = Vec::with_capacity(ics_sorted.len().saturating_sub(1));
        for w in ics_sorted.windows(2) {
            ic_gaps.push((w[1].c_min as i32 - w[0].c_max as i32 - 1).max(0));
        }
        let mut placed: Vec<(u8, String, u8, u8)> = Vec::new();
        for x in &ics_sorted {
            placed.push((x.c_min, x.id.clone(), 0, u8::from(x.rot != 0)));
        }
        for x in &sol.layout.btns {
            placed.push((x.c_min, x.id.clone(), 1, u8::from(x.row_top > 4)));
        }
        for x in &sol.layout.sws {
            placed.push((x.c_min, x.id.clone(), 2, 0));
        }
        if placed.is_empty() {
            return None;
        }
        placed.sort_by_key(|p| p.0);

        let mut rot = FxHashMap::default();
        let mut half = FxHashMap::default();
        let mut macro_ids = Vec::with_capacity(placed.len());
        for (_, id, kind, val) in placed {
            if kind == 0 {
                rot.insert(id.clone(), val);
            } else if kind == 1 {
                half.insert(id.clone(), val);
            }
            macro_ids.push(id);
        }
        let r_ids: Vec<String> = sol.layout.cs.iter().chain(&sol.layout.rs).chain(&sol.layout.leds).map(|x| x.id.clone()).collect();

        Self::normalize_dna(BackboneDna {
            macro_ids,
            ic_gaps,
            rot,
            half,
            gaps: sol.opts.gaps.clone(),
            r_ids,
            err_count: sol.errors.len(),
            ortho_count: sol.ortho_count,
            score: sol.score,
            ..Default::default()
        })
    }

    pub fn harvest_pool(&mut self, pool: &[BoardSolution]) -> usize {
        let mut cnt = 0usize;
        for s in pool.iter().filter(|s| s.unplaced == 0 && s.errors.len() <= 4) {
            if let Some(dna) = Self::extract_from_solution(s) {
                if self.add_seed(dna) {
                    cnt += 1;
                }
            }
        }
        cnt
    }

    #[allow(clippy::too_many_arguments)]
    pub fn to_opts(
        &self,
        dna: &BackboneDna,
        fallback_order: &[usize],
        place_comp_count: usize,
        r_comp_count: usize,
        rng: &mut Mulberry32,
        lock_ic_only: bool,
        comps: &[CompSpec],
    ) -> CandidateOpts {
        let id_to_idx: FxHashMap<&str, usize> = comps.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
        let mut used = FxHashSet::default();
        let mut order = Vec::with_capacity(comps.len());

        let src_ids = if lock_ic_only && !dna.ic_ids.is_empty() { &dna.ic_ids } else { &dna.macro_ids };
        for id in src_ids {
            if let Some(&idx) = id_to_idx.get(id.as_str()) {
                let valid = if lock_ic_only { comps[idx].comp_type == CompType::IC } else { !comps[idx].comp_type.is_special() };
                if valid && used.insert(idx) {
                    order.push(idx);
                }
            }
        }
        for &idx in fallback_order {
            if used.insert(idx) {
                order.push(idx);
            }
        }
        for idx in 0..comps.len() {
            if used.insert(idx) {
                order.push(idx);
            }
        }

        let mut rot: FxHashMap<CompId, u8> = FxHashMap::default();
        let mut half: FxHashMap<CompId, u8> = FxHashMap::default();
        for c in comps {
            match c.comp_type {
                CompType::IC => {
                    rot.insert(c.cid, dna.rot.get(&c.id).copied().unwrap_or(c.rot));
                }
                CompType::BTN2 => {
                    half.insert(c.cid, dna.half.get(&c.id).copied().unwrap_or(0));
                }
                _ => {}
            }
        }

        let mut gaps = if !lock_ic_only {
            dna.gaps.as_ref().filter(|g| g.len() == place_comp_count).cloned()
        } else {
            None
        };
        if gaps.is_none() && !dna.ic_gaps.is_empty() {
            let place_items: Vec<&CompSpec> = order.iter().filter_map(|&ci| comps.get(ci)).filter(|c| !c.comp_type.is_special()).collect();
            let mut g_vec: Vec<i32> = place_items.iter().map(|c| if c.comp_type == CompType::IC { 2 } else { 1 }).collect();
            let mut ic_cursor = 0usize;
            for (i, item) in place_items.iter().enumerate() {
                if item.comp_type == CompType::IC && ic_cursor < dna.ic_gaps.len() {
                    g_vec[i] = dna.ic_gaps[ic_cursor].clamp(0, 4);
                    ic_cursor += 1;
                }
            }
            gaps = Some(g_vec);
        }

        let spec_comps: Vec<&CompSpec> = comps.iter().filter(|c| c.comp_type.is_special()).collect();
        let spec_id_to_pos: FxHashMap<&str, usize> = spec_comps.iter().enumerate().map(|(pos, c)| (c.id.as_str(), pos)).collect();
        let mut r_order = Vec::with_capacity(r_comp_count);
        let mut used_r = FxHashSet::default();
        for id in &dna.r_ids {
            if let Some(&pos) = spec_id_to_pos.get(id.as_str()) {
                if used_r.insert(pos) {
                    r_order.push(pos);
                }
            }
        }
        for k in 0..r_comp_count {
            if used_r.insert(k) {
                r_order.push(k);
            }
        }

        let lock_gaps = gaps.is_some() && !lock_ic_only;
        CandidateOpts {
            order,
            rot,
            half,
            gaps,
            lock_gaps,
            r_order,
            ortho_weight: Some(if dna.err_count == 0 { 18.0 } else { 7.5 }),
            passes: Some(26),
            sr: Some(rng.next_usize(1_000_000_000) as u32),
            r_seed: Some(rng.next_usize(1_000_000_000) as u32),
            phase: if dna.err_count == 0 { SearchPhase::P2Ortho } else { SearchPhase::P1Bridge },
            ..Default::default()
        }
    }

    pub fn build_warm_start_queue(
        &self,
        rl: &mut RlBandit,
        fallback_order: &[usize],
        place_comp_count: usize,
        r_comp_count: usize,
        rng: &mut Mulberry32,
        comps: &[CompSpec],
    ) -> Vec<CandidateOpts> {
        if self.seeds.is_empty() {
            return Vec::new();
        }
        let id_to_idx: FxHashMap<&str, usize> = comps.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
        let mut unique_groups: Vec<&ArchetypeGroup> = self.archetypes.values().collect();
        unique_groups.sort_by(|a, b| Self::cmp_dna(&a.variants[0], &b.variants[0]));

        for (rank_idx, grp) in unique_groups.iter().take(24).enumerate() {
            let dna = &grp.variants[0];
            let rank_boost = (1.8 - (rank_idx as f32) * 0.06).max(0.4);
            let err_bonus = match dna.err_count { 0 => 6.5, 1 => 3.5, 2 => 1.8, _ => 0.0 };
            let w = (((10 - dna.err_count.min(9)) as f32 * 0.85).max(0.6) + err_bonus) * rank_boost;

            for (id, &v) in &dna.rot {
                if let Some(&idx) = id_to_idx.get(id.as_str()) {
                    rl.q_rot[comps[idx].cid.0 as usize][usize::from(v != 0)] += w * 0.90;
                }
            }
            let ic_indices: Vec<usize> = dna.ic_ids.iter().filter_map(|id| id_to_idx.get(id.as_str()).copied()).collect();
            for i in 0..ic_indices.len() {
                for j in (i + 1)..ic_indices.len() {
                    let (u, v) = (ic_indices[i], ic_indices[j]);
                    let step_w = if j == i + 1 { 1.75 } else if j == i + 2 { 0.75 } else { 0.35 };
                    rl.q_pair[u][v] += w * step_w;
                    rl.q_pair[v][u] = (rl.q_pair[v][u] * 0.90).max(0.2);
                }
            }
            for (id, &v) in &dna.half {
                if let Some(&idx) = id_to_idx.get(id.as_str()) {
                    rl.q_half[comps[idx].cid.0 as usize][usize::from(v != 0)] += w * 0.28;
                }
            }
            let all_macro: Vec<usize> = dna.macro_ids.iter().filter_map(|id| id_to_idx.get(id.as_str()).copied()).collect();
            for i in 0..all_macro.len() {
                for j in (i + 1)..all_macro.len() {
                    let (u, v) = (all_macro[i], all_macro[j]);
                    if comps[u].comp_type == CompType::IC && comps[v].comp_type == CompType::IC {
                        continue;
                    }
                    rl.q_pair[u][v] += w * if j == i + 1 { 0.32 } else { 0.12 };
                }
            }
        }

        let mut queue = Vec::new();
        let ic_cids: Vec<CompId> = comps.iter().filter(|c| c.comp_type == CompType::IC).map(|c| c.cid).collect();

        for (idx_g, grp) in unique_groups.iter().take(12).enumerate() {
            let top_dna = &grp.variants[0];
            let exact_opt = self.to_opts(top_dna, fallback_order, place_comp_count, r_comp_count, rng, false, comps);
            queue.push(exact_opt.clone());

            let mut backbone_opt = self.to_opts(top_dna, fallback_order, place_comp_count, r_comp_count, rng, true, comps);
            backbone_opt.net_ord = rng.next_usize(20);
            backbone_opt.rev = idx_g % 2 == 1;
            queue.push(backbone_opt);

            if idx_g < 5 && top_dna.ic_ids.len() >= 2 {
                let mut rev_ics = top_dna.ic_ids.clone();
                rev_ics.reverse();
                let mut rev_macro = rev_ics.clone();
                rev_macro.extend(top_dna.io_ids.iter().cloned());
                let mut rev_ic_gaps = top_dna.ic_gaps.clone();
                rev_ic_gaps.reverse();
                let rev_rot: FxHashMap<String, u8> = top_dna.rot.iter().map(|(k, &v)| (k.clone(), u8::from(v == 0))).collect();
                let mirror_dna = BackboneDna {
                    ic_ids: rev_ics,
                    macro_ids: rev_macro,
                    ic_gaps: rev_ic_gaps,
                    rot: rev_rot,
                    gaps: None,
                    ..top_dna.clone()
                };
                queue.push(self.to_opts(&mirror_dna, fallback_order, place_comp_count, r_comp_count, rng, true, comps));
            }

            if idx_g < 4 && !ic_cids.is_empty() {
                let mut tweaked = exact_opt;
                let pick_ic = ic_cids[rng.next_usize(ic_cids.len())];
                let cur_r = tweaked.rot.get(&pick_ic).copied().unwrap_or(0);
                tweaked.rot.insert(pick_ic, u8::from(cur_r == 0));
                tweaked.gaps = None;
                tweaked.lock_gaps = false;
                tweaked.sr = Some(rng.next_usize(1_000_000_000) as u32);
                tweaked.r_seed = Some(rng.next_usize(1_000_000_000) as u32);
                tweaked.net_ord = rng.next_usize(25);
                queue.push(tweaked);
            }
        }
        queue
    }
}
