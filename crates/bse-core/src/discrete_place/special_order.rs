use crate::config::personas::PersonaConfig;
use crate::discrete_place::decoupling::{nc_tag, pwr_tag, DiscreteContext, HolePin};
use crate::model::bitboard::{half_of, is_rail_row, rail_ok, BoardState};
use crate::model::circuit::{BoardLayout, CompSpec, ParsedNetlist, PlacedSpecial};
use crate::model::geometry::{v_seg_free, Footprint};
use crate::model::ids::{CompId, CompType, NetId, PinRef};
use rustc_hash::{FxHashMap, FxHashSet};

/// Cấu hình tùy chọn cho thuật toán đặt linh kiện rời (`placeSpecialComponents`)
#[derive(Clone, Debug, Default)]
pub struct SpecialOptions {
    pub persona: usize,
    pub sr: u32,
    pub r_seed: Option<u32>,
    pub ortho_weight: Option<f64>,
    pub dyn_br_pen: Option<f64>,
    pub dyn_aux_share: Option<f64>,
    pub r_order: Vec<usize>,
    /// Mặt nạ bit các cột bị trục xuất trong chế độ Vi phẫu (cột 1..=63 ứng với bit 1..=63)
    pub evict_cols_mask: u64,
    pub boost_nets: FxHashSet<NetId>,
    pub frozen_specials: FxHashMap<CompId, PlacedSpecial>,
}

/// Bản chụp trạng thái mặt bo ngay sau khi khóa cứng Tụ lọc IC (`snapIC` trong main.html)
#[derive(Clone, Debug)]
pub struct IcSnapshot {
    pub board: BoardState,
    pub col_own: [[Option<CompId>; 64]; 2],
    pub pres: FxHashMap<NetId, [u64; 2]>,
    pub holes: FxHashMap<NetId, Vec<HolePin>>,
    pub locked_cs: Vec<PlacedSpecial>,
    pub unplaced_base: usize,
    pub err_len_base: usize,
}

impl IcSnapshot {
    pub fn capture(
        board: &BoardState,
        ctx: &DiscreteContext,
        unplaced: usize,
        err_len: usize,
    ) -> Self {
        Self {
            board: *board,
            col_own: ctx.col_own,
            pres: ctx.pres.clone(),
            holes: ctx.holes.clone(),
            locked_cs: ctx.locked_ic_caps.clone(),
            unplaced_base: unplaced,
            err_len_base: err_len,
        }
    }

    pub fn restore(
        &self,
        board: &mut BoardState,
        ctx: &mut DiscreteContext,
        layout: &mut BoardLayout,
        unplaced: &mut usize,
        errors: &mut Vec<String>,
    ) {
        *board = self.board;
        ctx.col_own = self.col_own;
        ctx.pres = self.pres.clone();
        ctx.holes = self.holes.clone();
        layout.rs.clear();
        layout.leds.clear();
        layout.cs = self.locked_cs.clone();
        *unplaced = self.unplaced_base;
        errors.truncate(self.err_len_base);
    }
}

/// Kiểm tra cột `col` thuộc nửa `half` có thể nối dây thẳng đứng trực tiếp lên thanh nguồn `net` (`canStraightRail`)
#[inline]
pub fn can_straight_rail(
    board: &BoardState,
    fps: &[Footprint],
    col: u8,
    half: u8,
    net: NetId,
) -> bool {
    if !rail_ok(col) {
        return false;
    }
    let row_d = if half == 0 { 0 } else { 9 };
    let is_vcc = net == NetId::VCC;
    let rr = if half == 0 {
        if is_vcc { -1 } else { -2 }
    } else if is_vcc {
        11
    } else {
        10
    };
    !board.is_occupied(col, row_d) && !board.is_occupied(col, rr) && v_seg_free(fps, col, row_d, rr)
}

/// Kiểm tra linh kiện rời tại cột `col` thuộc nửa `half` có thể cắm thẳng chân vào lỗ thanh nguồn `net` (`canDirectPlugRail`)
#[inline]
pub fn can_direct_plug_rail(
    board: &BoardState,
    fps: &[Footprint],
    col: u8,
    half: u8,
    net: NetId,
) -> bool {
    if !rail_ok(col) {
        return false;
    }
    let is_vcc = net == NetId::VCC;
    let rr = if half == 0 {
        if is_vcc { -1 } else { -2 }
    } else if is_vcc {
        11
    } else {
        10
    };
    let pass_ok = match rr {
        -2 => !board.is_occupied(col, -1),
        11 => !board.is_occupied(col, 10),
        _ => true,
    };
    !board.is_occupied(col, rr)
        && pass_ok
        && v_seg_free(fps, col, if half == 0 { 1 } else { 8 }, rr)
}

/// Tính tổng số chân của từng mạng và tập các mạng vắt qua cả 2 nửa mặt bo (`totalNetPins` & `crossHalfNets`)
pub fn compute_net_stats(
    parsed: &ParsedNetlist,
    ctx: &DiscreteContext,
) -> (FxHashMap<NetId, usize>, FxHashSet<NetId>) {
    let mut total_net_pins = FxHashMap::default();
    let mut cross_half_nets = FxHashSet::default();
    for n in &parsed.nets {
        total_net_pins.insert(n.id, n.pins.len());
        if n.id.is_power() {
            continue;
        }
        if let Some(h_list) = ctx.holes.get(&n.id) {
            let has_top = h_list.iter().any(|h| h.row <= 4);
            let has_bot = h_list.iter().any(|h| h.row >= 5);
            if has_top && has_bot {
                cross_half_nets.insert(n.id);
            }
        }
    }
    (total_net_pins, cross_half_nets)
}

/// Cập nhật tọa độ cột trung tâm của các mạng tín hiệu (`updateCenters` trong main.html)
pub fn update_net_centers(ctx: &DiscreteContext, net_center: &mut FxHashMap<NetId, f64>) {
    net_center.clear();
    for (&net, h_list) in &ctx.holes {
        if net.is_power() || h_list.is_empty() {
            continue;
        }
        let sum: f64 = h_list.iter().map(|h| h.col as f64).sum();
        net_center.insert(net, sum / (h_list.len() as f64));
    }
}

/// Tìm các cặp linh kiện rời mắc nối tiếp trực tiếp qua mạng 2 cực (`seriesPartner` trong main.html)
pub fn find_series_partners(
    comps: &[CompSpec],
    ctx: &DiscreteContext,
    total_net_pins: &FxHashMap<NetId, usize>,
) -> FxHashMap<CompId, (CompId, NetId)> {
    let mut partners = FxHashMap::default();
    let all_spec: Vec<&CompSpec> = comps
        .iter()
        .filter(|c| c.comp_type.is_special() && !ctx.locked_decoupling_ids.contains(&c.cid))
        .collect();

    for i in 0..all_spec.len() {
        for j in (i + 1)..all_spec.len() {
            let (a, b) = (all_spec[i], all_spec[j]);
            let na1 = ctx.net_of.get(&PinRef { comp: a.cid, pin: 1 }).copied();
            let na2 = ctx.net_of.get(&PinRef { comp: a.cid, pin: 2 }).copied();
            let nb1 = ctx.net_of.get(&PinRef { comp: b.cid, pin: 1 }).copied();
            let nb2 = ctx.net_of.get(&PinRef { comp: b.cid, pin: 2 }).copied();
            let shared = [na1, na2].into_iter().flatten().find(|&n| {
                !n.is_power()
                    && total_net_pins.get(&n).copied().unwrap_or(0) == 2
                    && (Some(n) == nb1 || Some(n) == nb2)
            });
            if let Some(net) = shared {
                partners.insert(a.cid, (b.cid, net));
                partners.insert(b.cid, (a.cid, net));
            }
        }
    }
    partners
}

/// Phân nhóm và sắp xếp thứ tự ưu tiên đặt linh kiện rời (`rComps` trong `placeSpecialComponents`)
pub fn build_special_order(
    ordered_comps: &[CompSpec],
    ctx: &DiscreteContext,
    series_partner: &FxHashMap<CompId, (CompId, NetId)>,
    r_order: &[usize],
    p_cfg: &PersonaConfig,
) -> Vec<CompSpec> {
    let all_sp: Vec<&CompSpec> = ordered_comps.iter().filter(|c| c.comp_type.is_special()).collect();
    let mut r_order_rank: FxHashMap<CompId, usize> = FxHashMap::default();
    for (pos, &orig_idx) in r_order.iter().enumerate() {
        if let Some(c) = all_sp.get(orig_idx) {
            r_order_rank.insert(c.cid, pos);
        }
    }

    let has_pres = |n_opt: Option<NetId>| -> bool {
        n_opt.is_some_and(|n| ctx.pres.get(&n).is_some_and(|m| m[0] != 0 || m[1] != 0))
    };

    let r_priority = |c: &CompSpec| -> u8 {
        if matches!(c.comp_type, CompType::C | CompType::CC) {
            return 0;
        }
        let n1 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 1 }).copied();
        let n2 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 2 }).copied();
        let p1 = n1.is_some_and(NetId::is_power);
        let p2 = n2.is_some_and(NetId::is_power);
        if !p1 && !p2 && has_pres(n1) && has_pres(n2) {
            return 1;
        }
        let no_partner = !series_partner.contains_key(&c.cid);
        if (p1 && has_pres(n2) && no_partner) || (p2 && has_pres(n1) && no_partner) {
            return 2;
        }
        3
    };

    let mut base_r: Vec<CompSpec> = ordered_comps
        .iter()
        .filter(|c| c.comp_type.is_special() && !ctx.locked_decoupling_ids.contains(&c.cid))
        .cloned()
        .collect();

    base_r.sort_by(|a, b| {
        let (pa, pb) = (r_priority(a), r_priority(b));
        if pa != pb {
            return pa.cmp(&pb);
        }
        let ra = r_order_rank.get(&a.cid).copied().unwrap_or(0);
        let rb = r_order_rank.get(&b.cid).copied().unwrap_or(0);
        ra.cmp(&rb)
    });

    let mut r_comps = Vec::with_capacity(base_r.len());
    let mut used_r: FxHashSet<CompId> = FxHashSet::default();

    for c in &base_r {
        if !used_r.insert(c.cid) {
            continue;
        }
        let n1 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 1 }).copied();
        let n2 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 2 }).copied();
        let mut group = vec![c.clone()];

        for other in &base_r {
            if used_r.contains(&other.cid) {
                continue;
            }
            let on1 = ctx.net_of.get(&PinRef { comp: other.cid, pin: 1 }).copied();
            let on2 = ctx.net_of.get(&PinRef { comp: other.cid, pin: 2 }).copied();
            let share_n1 = n1.is_some_and(|n| !n.is_power() && (on1 == Some(n) || on2 == Some(n)));
            let share_n2 = n2.is_some_and(|n| !n.is_power() && (on1 == Some(n) || on2 == Some(n)));
            if share_n1 || share_n2 {
                used_r.insert(other.cid);
                group.push(other.clone());
            }
        }

        let grp_len = group.len();
        group.sort_by(|a, b| {
            let cap_a = matches!(a.comp_type, CompType::C | CompType::CC);
            let cap_b = matches!(b.comp_type, CompType::C | CompType::CC);
            if cap_a != cap_b {
                return if cap_a { core::cmp::Ordering::Less } else { core::cmp::Ordering::Greater };
            }
            if p_cfg.led_first && grp_len > 1 && (a.comp_type == CompType::LED) != (b.comp_type == CompType::LED) {
                return if a.comp_type == CompType::LED {
                    core::cmp::Ordering::Less
                } else {
                    core::cmp::Ordering::Greater
                };
            }
            let (pa, pb) = (r_priority(a), r_priority(b));
            if pa != pb {
                return pa.cmp(&pb);
            }
            let a_anc = u8::from(!(has_pres(ctx.net_of.get(&PinRef { comp: a.cid, pin: 1 }).copied())
                || has_pres(ctx.net_of.get(&PinRef { comp: a.cid, pin: 2 }).copied())));
            let b_anc = u8::from(!(has_pres(ctx.net_of.get(&PinRef { comp: b.cid, pin: 1 }).copied())
                || has_pres(ctx.net_of.get(&PinRef { comp: b.cid, pin: 2 }).copied())));
            a_anc.cmp(&b_anc)
        });
        r_comps.extend(group);
    }
    r_comps
}

/// Khôi phục các linh kiện rời đóng băng hợp lệ trong chế độ Bác sĩ Vi phẫu (`activeFrozen`)
#[allow(clippy::too_many_arguments)]
pub fn apply_frozen_specials(
    r_comps: &[CompSpec],
    opts: &SpecialOptions,
    series_partner: &FxHashMap<CompId, (CompId, NetId)>,
    board: &mut BoardState,
    layout: &mut BoardLayout,
    ctx: &mut DiscreteContext,
    placed_special_ids: &mut FxHashSet<CompId>,
    all_comps: &[CompSpec],
    parsed: &ParsedNetlist,
    errors: &mut Vec<String>,
) {
    if opts.frozen_specials.is_empty() {
        return;
    }
    let is_evicted_col = |c: u8| (opts.evict_cols_mask & (1u64 << c)) != 0;
    let in_pres = |ctx_ref: &DiscreteContext, n: NetId, col: u8, row: i8| -> bool {
        ctx_ref.pres.get(&n).is_some_and(|m| (m[half_of(row) as usize] & (1u64 << col)) != 0)
    };

    let should_evict = |cid: CompId, ctx_ref: &DiscreteContext| -> bool {
        let Some(fz) = opts.frozen_specials.get(&cid) else { return true };
        if fz.locked || ctx_ref.locked_decoupling_ids.contains(&cid) {
            return false;
        }
        if is_evicted_col(fz.c1) || is_evicted_col(fz.c2) {
            return true;
        }
        let n1 = ctx_ref.net_of.get(&PinRef { comp: cid, pin: 1 }).copied();
        let n2 = ctx_ref.net_of.get(&PinRef { comp: cid, pin: 2 }).copied();
        if n1.is_some_and(|n| opts.boost_nets.contains(&n))
            || n2.is_some_and(|n| opts.boost_nets.contains(&n))
        {
            return true;
        }
        if let Some(n) = n1.filter(|n| !n.is_power() && ctx_ref.pres.contains_key(n)) {
            if !is_rail_row(fz.r1) && !in_pres(ctx_ref, n, fz.c1, fz.r1) {
                return true;
            }
        }
        if let Some(n) = n2.filter(|n| !n.is_power() && ctx_ref.pres.contains_key(n)) {
            if !is_rail_row(fz.r2) && !in_pres(ctx_ref, n, fz.c2, fz.r2) {
                return true;
            }
        }
        if let Some(&(pt_id, pt_net)) = series_partner.get(&cid) {
            let Some(pfz) = opts.frozen_specials.get(&pt_id) else { return true };
            if is_evicted_col(pfz.c1) || is_evicted_col(pfz.c2) || opts.boost_nets.contains(&pt_net) {
                return true;
            }
            let a_spots = [
                (!is_rail_row(fz.r1)).then(|| (fz.c1, half_of(fz.r1))),
                (!is_rail_row(fz.r2)).then(|| (fz.c2, half_of(fz.r2))),
            ];
            let b_spots = [
                (!is_rail_row(pfz.r1)).then(|| (pfz.c1, half_of(pfz.r1))),
                (!is_rail_row(pfz.r2)).then(|| (pfz.c2, half_of(pfz.r2))),
            ];
            if !a_spots.iter().flatten().any(|a| b_spots.iter().flatten().any(|b| a == b)) {
                return true;
            }
        }
        false
    };

    for c in r_comps {
        if placed_special_ids.contains(&c.cid)
            || ctx.invalid_comp_ids.contains(&c.cid)
            || should_evict(c.cid, ctx)
        {
            continue;
        }
        let fz = &opts.frozen_specials[&c.cid];
        if board.is_occupied(fz.c1, fz.r1) || board.is_occupied(fz.c2, fz.r2) {
            continue;
        }
        let n1 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 1 }).copied();
        let n2 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 2 }).copied();
        if !is_rail_row(fz.r1) {
            let t1 = n1.map_or(0, |n| pwr_tag(n, c.cid, 1, fz.r1, c.comp_type, &ctx.main_ic_pwr_pins));
            let cur = board.col_net[half_of(fz.r1) as usize][fz.c1 as usize];
            if cur != 0 && cur != t1 {
                continue;
            }
        }
        if !is_rail_row(fz.r2) {
            let t2 = n2.map_or(0, |n| pwr_tag(n, c.cid, 2, fz.r2, c.comp_type, &ctx.main_ic_pwr_pins));
            let cur = board.col_net[half_of(fz.r2) as usize][fz.c2 as usize];
            if cur != 0 && cur != t2 {
                continue;
            }
        }

        board.occupy(fz.c1, fz.r1);
        board.occupy(fz.c2, fz.r2);
        if fz.c1 == fz.c2 {
            if fz.r1 == -2 || fz.r2 == -2 {
                board.occupy(fz.c1, -1);
            }
            if fz.r1 == 11 || fz.r2 == 11 {
                board.occupy(fz.c1, 10);
            }
        }
        if n1.is_none() && !is_rail_row(fz.r1) {
            board.col_net[half_of(fz.r1) as usize][fz.c1 as usize] = nc_tag(c.cid, 1);
        }
        if n2.is_none() && !is_rail_row(fz.r2) {
            board.col_net[half_of(fz.r2) as usize][fz.c2 as usize] = nc_tag(c.cid, 2);
        }
        let placed = PlacedSpecial {
            cid: c.cid,
            id: c.id.clone(),
            comp_type: c.comp_type,
            c1: fz.c1,
            r1: fz.r1,
            c2: fz.c2,
            r2: fz.r2,
            locked: false,
            ic_id: None,
        };
        match c.comp_type {
            CompType::R => layout.rs.push(placed),
            CompType::LED => layout.leds.push(placed),
            _ => layout.cs.push(placed),
        }
        placed_special_ids.insert(c.cid);
        ctx.add_pin(board, c.cid, 1, fz.c1, fz.r1, all_comps, parsed, errors);
        ctx.add_pin(board, c.cid, 2, fz.c2, fz.r2, all_comps, parsed, errors);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::personas::get_persona;
    use crate::config::weights::CostWeights;
    use crate::discrete_place::decoupling::setup_macro_and_decoupling;
    use crate::macro_place::topology::build_topology_cache;
    use crate::model::circuit::PlacedIc;
    use crate::parser::parse_netlist;

    #[test]
    fn test_series_partner_and_special_order() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_other(CompId(1), "R1", CompType::R),
            CompSpec::new_other(CompId(2), "L1", CompType::LED),
            CompSpec::new_other(CompId(3), "C1", CompType::C),
        ];
        let txt = "VCC U1:14; GND U1:7; U1:1 -> R1 -> L1 -> GND; U1:2 -> C1 -> GND";
        let parsed = parse_netlist(txt, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        let mut layout = BoardLayout::default();
        layout.ics.push(PlacedIc { cid: CompId(0), id: "U1".into(), pins: 14, rot: 0, c_min: 10, c_max: 16, w: 7 });
        let mut board = BoardState::new(false);
        let mut fps = Vec::new();
        layout.block_space(&mut board, &mut fps);
        let mut errors = Vec::new();
        let ctx = setup_macro_and_decoupling(&mut board, &mut layout, &parsed, &topo, &comps, &CostWeights::default(), &mut errors);

        let (tot_pins, _) = compute_net_stats(&parsed, &ctx);
        let partners = find_series_partners(&comps, &ctx, &tot_pins);
        assert_eq!(partners.get(&CompId(1)).map(|x| x.0), Some(CompId(2)));

        // Với Persona 2 (I3:BridgeLED, led_first = true): C1 đặt trước, sau đó trong nhóm (R1, L1) thì L1 đứng trước R1!
        let ordered = build_special_order(&comps, &ctx, &partners, &[], &get_persona(2));
        let ids: Vec<&str> = ordered.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["C1", "L1", "R1"]);
    }
}
