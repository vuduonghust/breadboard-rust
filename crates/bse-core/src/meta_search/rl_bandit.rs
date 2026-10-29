use crate::model::circuit::CompSpec;
use crate::model::ids::{CompId, CompType, Mulberry32};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

pub const NUM_ISLANDS: usize = 4;

/// Chế độ điều hướng trực tiếp từ giao diện (`#steer-mode` trong main.html)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SteerMode {
    #[default]
    Auto,
    Macro,
    Bridge,
    Surgery,
    LockIo,
}

/// Trọng tâm chiến thuật hiện tại của MetaAgent (`MetaAgent.focus` trong main.html)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentFocus {
    #[default]
    Balanced,
    Macro,
    Bridge,
    Surgery,
    LockIo,
}

impl AgentFocus {
    pub const fn as_str(self) -> &'static str {
        match self {
            AgentFocus::Balanced => "balanced",
            AgentFocus::Macro => "macro",
            AgentFocus::Bridge => "bridge",
            AgentFocus::Surgery => "surgery",
            AgentFocus::LockIo => "lock_io",
        }
    }
}

/// Meta-Agent Tự Phản Tỉnh & Điều Hướng Trọng Số Động (`MetaAgent` trong main.html)
#[derive(Clone, Copy, Debug, Default)]
pub struct MetaAgent {
    pub mode: SteerMode,
    pub focus: AgentFocus,
    pub dyn_br_pen: Option<f64>,
    pub dyn_aux_share: Option<f64>,
}

impl MetaAgent {
    pub fn reflect(&mut self, stall_cnt: u32, best_bridges: usize, best_err_count: Option<usize>) {
        if self.mode != SteerMode::Auto {
            self.focus = match self.mode {
                SteerMode::Auto => AgentFocus::Balanced,
                SteerMode::Macro => AgentFocus::Macro,
                SteerMode::Bridge => AgentFocus::Bridge,
                SteerMode::Surgery => AgentFocus::Surgery,
                SteerMode::LockIo => AgentFocus::LockIo,
            };
            self.dyn_br_pen = (self.mode == SteerMode::Bridge).then_some(42.0);
            self.dyn_aux_share = (self.mode == SteerMode::Bridge).then_some(26.0);
            return;
        }
        if stall_cnt > 110 {
            self.focus = AgentFocus::Macro;
            self.dyn_br_pen = None;
            self.dyn_aux_share = None;
        } else if best_bridges <= 8 && best_err_count.is_some_and(|e| e > 0) {
            self.focus = AgentFocus::Bridge;
            self.dyn_br_pen = Some(42.0);
            self.dyn_aux_share = Some(26.0);
        } else if best_err_count == Some(0) {
            self.focus = AgentFocus::Surgery;
            self.dyn_br_pen = Some(38.0);
            self.dyn_aux_share = Some(24.0);
        } else {
            self.focus = AgentFocus::Balanced;
            self.dyn_br_pen = None;
            self.dyn_aux_share = None;
        }
    }
}

/// Bộ Học Tăng Cường UCB1 Bandit & Ma trận Q-Learning (`RL` trong main.html)
#[derive(Clone, Debug)]
pub struct RlBandit {
    pub q_rot: [[f32; 2]; 256],
    pub q_half: [[f32; 2]; 256],
    pub q_pair: Vec<Vec<f32>>,
    pub arm_pulls: [f32; 6],
    pub arm_reward: [f32; 6],
    pub total_pulls: f32,
    pub i_pulls: [f32; NUM_ISLANDS],
    pub i_reward: [f32; NUM_ISLANDS],
    pub total_i_pulls: f32,
}

impl RlBandit {
    pub fn new(num_comps: usize) -> Self {
        Self {
            q_rot: [[1.0; 2]; 256],
            q_half: [[1.0; 2]; 256],
            q_pair: vec![vec![1.0; num_comps]; num_comps],
            arm_pulls: [1.0; 6],
            arm_reward: [1.0; 6],
            total_pulls: 6.0,
            i_pulls: [1.0; NUM_ISLANDS],
            i_reward: [1.0; NUM_ISLANDS],
            total_i_pulls: NUM_ISLANDS as f32,
        }
    }

    pub fn pick_arm(&self, focus: AgentFocus, rng: &mut Mulberry32) -> usize {
        if focus == AgentFocus::Macro && rng.next_f64() < 0.65 {
            return if rng.next_f64() < 0.5 { 0 } else { 2 };
        }
        if focus == AgentFocus::Bridge && rng.next_f64() < 0.55 {
            return 3;
        }
        let log_n = (self.total_pulls as f64).ln();
        let (mut best_a, mut best_ucb) = (0usize, -1e9_f64);
        for a in 0..6 {
            let pulls = self.arm_pulls[a] as f64;
            let ucb = (self.arm_reward[a] as f64) / pulls
                + 1.35 * (log_n / pulls).sqrt()
                + rng.next_f64() * 0.05;
            if ucb > best_ucb {
                best_ucb = ucb;
                best_a = a;
            }
        }
        best_a
    }

    pub fn pick_island(&self, rr_tick: usize, focus: AgentFocus, rng: &mut Mulberry32) -> usize {
        if focus == AgentFocus::Bridge && rng.next_f64() < 0.45 {
            return 2;
        }
        if rr_tick.is_multiple_of(2) {
            return (rr_tick >> 1) % NUM_ISLANDS;
        }
        let log_n = (self.total_i_pulls as f64).ln();
        let (mut best_i, mut best_ucb) = (0usize, -1e9_f64);
        for i in 0..NUM_ISLANDS {
            let pulls = self.i_pulls[i] as f64;
            let ucb = (self.i_reward[i] as f64) / pulls
                + 1.15 * (log_n / pulls).sqrt()
                + rng.next_f64() * 0.08;
            if ucb > best_ucb {
                best_ucb = ucb;
                best_i = i;
            }
        }
        best_i
    }

    #[inline]
    pub fn sample_binary(&self, q: [f32; 2], eps: f64, rng: &mut Mulberry32) -> u8 {
        if rng.next_f64() < eps {
            return rng.next_usize(2) as u8;
        }
        let e0 = (q[0] as f64).exp();
        let e1 = (q[1] as f64).exp();
        u8::from(rng.next_f64() < (e1 / (e0 + e1)))
    }

    /// Giảm trọng số Q khi bố cục bị cưỡng bức đưa vào Sổ đen Tabu (`TabuBasin.forceBan`)
    pub fn penalize_force_ban(
        &mut self,
        order: &[usize],
        rot: &FxHashMap<CompId, u8>,
        comps: &[CompSpec],
    ) {
        for c in comps.iter().filter(|c| c.comp_type == CompType::IC) {
            let v = usize::from(rot.get(&c.cid).copied().unwrap_or(0) != 0);
            self.q_rot[c.cid.0 as usize][v] *= 0.55;
        }
        let pl: Vec<usize> = order
            .iter()
            .copied()
            .filter(|&ci| comps.get(ci).is_some_and(|c| !c.comp_type.is_special()))
            .collect();
        for w in pl.windows(2) {
            self.q_pair[w[0]][w[1]] *= 0.55;
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        unplaced: usize,
        err_cnt: usize,
        score: f64,
        parent_score: Option<f64>,
        isl_improved: bool,
        newly_tabu: bool,
        island: usize,
        arm: Option<usize>,
        order: &[usize],
        rot: &FxHashMap<CompId, u8>,
        half: &FxHashMap<CompId, u8>,
        comps: &[CompSpec],
    ) {
        if newly_tabu {
            for c in comps.iter().filter(|c| c.comp_type == CompType::IC) {
                let v = usize::from(rot.get(&c.cid).copied().unwrap_or(0) != 0);
                self.q_rot[c.cid.0 as usize][v] *= 0.72;
            }
            return;
        }
        let imp_bonus = if parent_score.is_some_and(|ps| score < ps) {
            2.4_f32
        } else if isl_improved {
            1.8_f32
        } else {
            0.0_f32
        };
        let base_r = if unplaced > 0 {
            0.0_f32
        } else {
            ((10.0 - (err_cnt as f32)).max(0.0)) * 0.18 + if err_cnt == 0 { 5.0 } else { 0.0 }
        };
        let rew = base_r + imp_bonus;
        let lr = 0.12_f32;

        if imp_bonus > 0.0 {
            for c in comps {
                if c.comp_type == CompType::IC {
                    let v = usize::from(rot.get(&c.cid).copied().unwrap_or(0) != 0);
                    let q = &mut self.q_rot[c.cid.0 as usize][v];
                    *q = (1.0 - lr) * (*q) + lr * rew;
                } else if c.comp_type.is_io() {
                    let def_half = u8::from(c.comp_type == CompType::SW);
                    let v = usize::from(half.get(&c.cid).copied().unwrap_or(def_half) != 0);
                    let q = &mut self.q_half[c.cid.0 as usize][v];
                    *q = (1.0 - lr) * (*q) + lr * rew;
                }
            }
            let pl: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&ci| comps.get(ci).is_some_and(|c| !c.comp_type.is_special()))
                .collect();
            for i in 0..pl.len() {
                for j in (i + 1)..pl.len() {
                    let (u, v) = (pl[i], pl[j]);
                    self.q_pair[u][v] = 0.92 * self.q_pair[u][v] + 0.08 * rew;
                }
            }
        }
        if let Some(a) = arm.filter(|&a| a < 6) {
            self.arm_pulls[a] += 1.0;
            self.arm_reward[a] += imp_bonus + if err_cnt <= 2 { 0.45 } else { 0.04 };
            self.total_pulls += 1.0;
        }
        let isl = island % NUM_ISLANDS;
        self.i_pulls[isl] += 1.0;
        self.i_reward[isl] += imp_bonus
            + if err_cnt == 0 {
                3.5
            } else if err_cnt <= 2 {
                0.65
            } else {
                0.08
            };
        self.total_i_pulls += 1.0;
    }

    /// Sinh thứ tự linh kiện dẫn đường bởi ma trận `q_pair` (`buildGuidedOrder` trong main.html)
    pub fn build_guided_order(&self, comps: &[CompSpec], rng: &mut Mulberry32) -> Vec<usize> {
        let mut place_set: Vec<usize> = (0..comps.len())
            .filter(|&i| !comps[i].comp_type.is_special())
            .collect();
        let mut r_set: Vec<usize> = (0..comps.len())
            .filter(|&i| comps[i].comp_type.is_special())
            .collect();
        rng.shuffle(&mut r_set);
        if place_set.is_empty() {
            return r_set;
        }
        let mut out = Vec::with_capacity(comps.len());
        let first_idx = rng.next_usize(place_set.len());
        let mut cur = place_set.remove(first_idx);
        out.push(cur);

        while !place_set.is_empty() {
            let next_pos = if rng.next_f64() < 0.25 {
                rng.next_usize(place_set.len())
            } else {
                let (mut best_pos, mut best_w) = (0usize, -1e9_f64);
                for (pos, &nx) in place_set.iter().enumerate() {
                    let w = (self.q_pair[cur][nx] as f64) + rng.next_f64() * 0.35;
                    if w > best_w {
                        best_w = w;
                        best_pos = pos;
                    }
                }
                best_pos
            };
            cur = place_set.remove(next_pos);
            out.push(cur);
        }
        out.extend(r_set);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta_search::tabu_basin::TabuBasin;
    use crate::meta_search::zobrist::{encode_btn_bend, layout_sig, sol_sig, tabu_key};
    use crate::model::circuit::{BoardLayout, PlacedBtn, PlacedIc};

    #[test]
    fn test_zobrist_tabu_and_rl_bandit() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_ic(CompId(1), "U2", 14, 0),
            CompSpec::new_other(CompId(2), "R1", CompType::R),
            CompSpec::new_other(CompId(3), "S1", CompType::SW),
            CompSpec::new_other(CompId(4), "B1", CompType::BTN2),
        ];
        let mut layout = BoardLayout::default();
        layout.ics.push(PlacedIc { cid: CompId(0), id: "U1".into(), pins: 14, rot: 0, c_min: 10, c_max: 16, w: 7 });
        layout.btns.push(PlacedBtn {
            cid: CompId(4), id: "B1".into(), comp_type: CompType::BTN2,
            c_min: 1, c_max: 3, row_top: 1, row_bot: 2, flip: 0, bend: -1,
        });
        let s1 = layout_sig(&layout);
        layout.btns[0].bend = 1;
        let s1_bent = layout_sig(&layout);
        assert_ne!(s1, s1_bent); // layout_sig phân biệt rõ trạng thái bend của BTN2

        let s2 = sol_sig(&layout, &[], 0);
        assert_ne!(s1_bent, s2);

        let mut rot = FxHashMap::default();
        rot.insert(CompId(0), 0);
        rot.insert(CompId(1), 1);
        rot.insert(CompId(4), encode_btn_bend(-1));
        let mut half = FxHashMap::default();
        half.insert(CompId(3), 0); // S1 ở nửa trên
        half.insert(CompId(4), 1); // B1 ở nửa dưới
        let t_key = tabu_key(&[0, 1, 3, 4, 2], &rot, &half, &comps);

        let mut half_alt = half.clone();
        half_alt.insert(CompId(3), 1); // Đổi S1 xuống nửa dưới -> tabu_key phải thay đổi
        assert_ne!(t_key, tabu_key(&[0, 1, 3, 4, 2], &rot, &half_alt, &comps));

        let mut tabu = TabuBasin::new();
        for it in 1..=25 {
            let banned = tabu.record(t_key, 1000.0, it);
            if it == 25 {
                assert!(banned);
            }
        }
        assert!(tabu.is_tabu(t_key, 100));
        assert!(!tabu.is_tabu(t_key, 400));

        let mut agent = MetaAgent::default();
        agent.reflect(50, 7, Some(2));
        assert_eq!(agent.focus, AgentFocus::Bridge);
        assert_eq!(agent.dyn_br_pen, Some(42.0));

        let mut rl = RlBandit::new(comps.len());
        let mut rng = Mulberry32::new(42);
        rl.update(0, 0, 500.0, Some(800.0), true, false, 2, Some(1), &[0, 1, 3, 4, 2], &rot, &half, &comps);
        assert!(rl.q_rot[1][1] > 1.0);
        assert!(rl.q_half[3][0] > 1.0); // SW (S1) được cập nhật q_half
        assert!(rl.q_half[4][1] > 1.0); // BTN2 (B1) được cập nhật q_half
        let guided = rl.build_guided_order(&comps, &mut rng);
        assert_eq!(guided.len(), 5);
        assert_eq!(guided[4], 2);
    }
}
