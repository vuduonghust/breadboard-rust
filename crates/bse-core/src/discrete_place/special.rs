use crate::config::personas::get_persona;
use crate::config::weights::{CostWeights, SpecialSpan};
use crate::discrete_place::decoupling::{
    base_net_of, is_pwr_tag, nc_tag, pwr_aux_tag, pwr_tag, DiscreteContext,
};
use crate::discrete_place::special_order::{
    apply_frozen_specials, build_special_order, can_direct_plug_rail, can_straight_rail,
    compute_net_stats, find_series_partners, update_net_centers, IcSnapshot, SpecialOptions,
};
use crate::model::bitboard::{half_of, is_rail_row, rail_ok, BoardState};
use crate::model::circuit::{BoardLayout, CompSpec, ParsedNetlist, PlacedSpecial};
use crate::model::geometry::{seg_dist_sq, v_seg_free, vis_y, Footprint};
use crate::model::ids::{CompId, CompType, Mulberry32, NetId, PinRef};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Clone, Copy, Debug)]
struct Lookahead {
    net: Option<NetId>,
    sp: SpecialSpan,
}

#[derive(Clone, Copy, Debug)]
struct PrepHole {
    col: u8,
    row: i8,
    half: u8,
    is_rail: bool,
    is_shared: bool,
    is_pwr: bool,
    vy: f64,
    base: f64,
}

fn valid_holes(
    board: &BoardState,
    ctx: &DiscreteContext,
    net_opt: Option<NetId>,
    cid: CompId,
    pin: u8,
    comp_type: CompType,
) -> Vec<(u8, i8)> {
    let mut out = Vec::with_capacity(64);
    if let Some(net) = net_opt.filter(|n| n.is_power()) {
        let r_rows = if net == NetId::VCC { [-1i8, 11i8] } else { [-2i8, 10i8] };
        for c in 3u8..=61 {
            if !rail_ok(c) {
                continue;
            }
            for rr in r_rows {
                if !board.is_occupied(c, rr) {
                    out.push((c, rr));
                }
            }
        }
        if out.len() >= 8 {
            return out;
        }
    }
    for c in 1u8..=63 {
        for h in [0u8, 1u8] {
            let cur_tag = board.col_net[h as usize][c as usize];
            match net_opt {
                Some(net) => {
                    let tag = pwr_tag(net, cid, pin, if h != 0 { 5 } else { 0 }, comp_type, &ctx.main_ic_pwr_pins);
                    if cur_tag != 0 && cur_tag != tag {
                        continue;
                    }
                }
                None if cur_tag != 0 => continue,
                None => {}
            }
            let r_range = if h != 0 { 5i8..=9 } else { 0i8..=4 };
            let mut has_non_groove = false;
            for r in r_range.clone() {
                if !board.is_occupied(c, r) && r != 4 && r != 5 {
                    has_non_groove = true;
                    break;
                }
            }
            for r in r_range {
                if !board.is_occupied(c, r) && (!has_non_groove || (r != 4 && r != 5)) {
                    out.push((c, r));
                }
            }
        }
    }
    out
}

fn lookahead_info(
    cid: CompId,
    pin_net: Option<NetId>,
    r_comps: &[CompSpec],
    placed_ids: &FxHashSet<CompId>,
    ctx: &DiscreteContext,
) -> Option<Lookahead> {
    let net = pin_net.filter(|n| !n.is_power())?;
    for other in r_comps {
        if other.cid == cid || placed_ids.contains(&other.cid) {
            continue;
        }
        let on1 = ctx.net_of.get(&PinRef { comp: other.cid, pin: 1 }).copied();
        let on2 = ctx.net_of.get(&PinRef { comp: other.cid, pin: 2 }).copied();
        if on1 == Some(net) {
            return Some(Lookahead { net: on2, sp: SpecialSpan::for_type(other.comp_type) });
        }
        if on2 == Some(net) {
            return Some(Lookahead { net: on1, sp: SpecialSpan::for_type(other.comp_type) });
        }
    }
    None
}

fn can_partner_ortho_share(
    col: u8,
    h: u8,
    la: Lookahead,
    board: &BoardState,
    fps: &[Footprint],
    ctx: &DiscreteContext,
) -> bool {
    let Some(net) = la.net else { return false };
    let (min_d, max_d) = (la.sp.min.ceil() as i32, la.sp.max.floor() as i32);
    if !net.is_power() {
        if let Some(masks) = ctx.pres.get(&net) {
            let m = masks[h as usize];
            for tc in 1u8..=63 {
                if (m & (1u64 << tc)) != 0 {
                    let d = (tc as i32 - col as i32).abs();
                    if d >= min_d && d <= max_d {
                        return true;
                    }
                }
            }
        }
        return false;
    }
    if can_direct_plug_rail(board, fps, col, h, net) {
        return true;
    }
    let lo = (col as i32 - max_d).max(1) as u8;
    let hi = (col as i32 + max_d).min(63) as u8;
    let aux_tag = pwr_aux_tag(net, h);
    for tc in lo..=hi {
        if (tc as i32 - col as i32).abs() < min_d {
            continue;
        }
        let cur = board.col_net[h as usize][tc as usize];
        if (cur == 0 || cur == aux_tag) && can_straight_rail(board, fps, tc, h, net) {
            return true;
        }
    }
    false
}

/// Tính điểm thưởng LED Đối Xứng (#10) khi đặt LED `c` so với các LED cùng nhóm đã đặt trước đó
#[inline]
fn eval_sym_led_bonus(
    a_col: u8,
    a_row: i8,
    b_col: u8,
    b_row: i8,
    sym_partners: &[PlacedSpecial],
    pair_bonus: f64,
) -> f64 {
    if sym_partners.is_empty() || pair_bonus <= 0.0 {
        return 0.0;
    }
    let (ha, hb) = (half_of(a_row), half_of(b_row));
    let mut best_bonus = 0.0_f64;
    for pl in sym_partners {
        let (pha, phb) = (half_of(pl.r1), half_of(pl.r2));
        let same_cols = (a_col == pl.c1 && b_col == pl.c2) || (a_col == pl.c2 && b_col == pl.c1);
        // 1. Đối xứng gương qua rãnh E-F: Cùng cặp cột (c1, c2), nằm ở nửa bo đối diện
        if same_cols && ha != pha && hb != phb {
            let mut b = pair_bonus;
            let mirror_r1 = (!is_rail_row(a_row) && !is_rail_row(pl.r1) && a_row + pl.r1 == 9)
                || (is_rail_row(a_row) && is_rail_row(pl.r1));
            let mirror_r2 = (!is_rail_row(b_row) && !is_rail_row(pl.r2) && b_row + pl.r2 == 9)
                || (is_rail_row(b_row) && is_rail_row(pl.r2));
            if mirror_r1 && mirror_r2 {
                b += pair_bonus * 0.25;
            }
            if b > best_bonus {
                best_bonus = b;
            }
        } else if ha == pha && hb == phb {
            // 2. Thẳng hàng trên cùng nửa bo: Cùng hàng lỗ cắm và song song hướng chân
            let same_rows = a_row == pl.r1 && b_row == pl.r2;
            let same_vec = (a_col as i32 - b_col as i32) == (pl.c1 as i32 - pl.c2 as i32);
            if same_rows && same_vec {
                let b = pair_bonus * 0.85;
                if b > best_bonus {
                    best_bonus = b;
                }
            }
        }
    }
    best_bonus
}

/// Đặt toàn bộ linh kiện rời 2 chân (R, LED, C, CC) lên mặt bo (`placeSpecialComponents` trong main.html)
#[allow(clippy::too_many_arguments)]
pub fn place_special_components(
    use_feedback: bool,
    seed_offset: u32,
    opts: &SpecialOptions,
    snap_ic: &IcSnapshot,
    board: &mut BoardState,
    layout: &mut BoardLayout,
    ctx: &mut DiscreteContext,
    fps: &[Footprint],
    ordered_comps: &[CompSpec],
    all_comps: &[CompSpec],
    parsed: &ParsedNetlist,
    col_hist: &[f32; 65],
    row_hist: &[f32; 10],
    weights: &CostWeights,
    unplaced: &mut usize,
    errors: &mut Vec<String>,
) -> FxHashSet<NetId> {
    if use_feedback || seed_offset > 0 {
        snap_ic.restore(board, ctx, layout, unplaced, errors);
    }
    let p_cfg = get_persona(opts.persona);
    let ortho_w = opts.ortho_weight.unwrap_or(p_cfg.ortho_w);
    let dyn_br_pen = opts.dyn_br_pen.unwrap_or(p_cfg.br_pen);
    let base_seed = opts.r_seed.unwrap_or(if opts.sr != 0 { opts.sr } else { 1 });
    let mut r_rng = Mulberry32::new(base_seed.wrapping_add(997).wrapping_add(seed_offset));

    let (total_net_pins, mut cross_half_nets) = compute_net_stats(parsed, ctx);
    let series_partner = find_series_partners(ordered_comps, ctx, &total_net_pins);
    let r_comps = build_special_order(ordered_comps, ctx, &series_partner, &opts.r_order, &p_cfg);

    let mut placed_special_ids = ctx.locked_decoupling_ids.clone();
    let mut net_center: FxHashMap<NetId, f64> = FxHashMap::default();
    update_net_centers(ctx, &mut net_center);

    apply_frozen_specials(
        &r_comps, opts, &series_partner, board, layout, ctx,
        &mut placed_special_ids, all_comps, parsed, errors,
    );
    update_net_centers(ctx, &mut net_center);

    let sig_share_bonus = (ortho_w + 18.0).max(30.0);

    for c in &r_comps {
        if placed_special_ids.contains(&c.cid) {
            continue;
        }
        let sp = SpecialSpan::for_type(c.comp_type);
        let (min_sq, max_sq) = (sp.min * sp.min, sp.max * sp.max);
        let n1 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 1 }).copied();
        let n2 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 2 }).copied();

        if matches!(c.comp_type, CompType::CC | CompType::C)
            && matches!((n1, n2), (Some(NetId::VCC), Some(NetId::GND)) | (Some(NetId::GND), Some(NetId::VCC)))
            && ctx.locked_ic_caps.len() >= layout.ics.len()
            && !layout.ics.is_empty()
        {
            placed_special_ids.insert(c.cid);
            continue;
        }
        if (n1.is_some() && n1 == n2) || ctx.invalid_comp_ids.contains(&c.cid) {
            *unplaced += 1;
            continue;
        }

        let holes_a = valid_holes(board, ctx, n1, c.cid, 1, c.comp_type);
        let holes_b = valid_holes(board, ctx, n2, c.cid, 2, c.comp_type);
        let la1 = lookahead_info(c.cid, n1, &r_comps, &placed_special_ids, ctx);
        let la2 = lookahead_info(c.cid, n2, &r_comps, &placed_special_ids, ctx);

        let raw_t1 = n1.and_then(|n| net_center.get(&n).copied())
            .or_else(|| la1.and_then(|l| l.net).and_then(|n| net_center.get(&n).copied()));
        let raw_t2 = n2.and_then(|n| net_center.get(&n).copied())
            .or_else(|| la2.and_then(|l| l.net).and_then(|n| net_center.get(&n).copied()));
        let c_target1 = raw_t1.or(raw_t2);
        let c_target2 = raw_t2.or(raw_t1);
        let is_series_chain = series_partner.contains_key(&c.cid);

        let sym_partners: Vec<PlacedSpecial> = if c.comp_type == CompType::LED && !parsed.sym_led_groups.is_empty() {
            let mut sp_list = Vec::new();
            for grp in &parsed.sym_led_groups {
                if grp.contains(&c.cid) {
                    for &pid in grp {
                        if pid != c.cid {
                            if let Some(pl) = layout.leds.iter().find(|l| l.cid == pid) {
                                sp_list.push(pl.clone());
                            }
                        }
                    }
                }
            }
            sp_list
        } else {
            Vec::new()
        };

        let (prep_a, prep_b) = {
            let check_bridge_cost = |col: u8, h: u8, net_opt: Option<NetId>, b_ref: &BoardState| -> f64 {
                let is_pwr = net_opt.is_some_and(NetId::is_power);
                if !is_pwr && net_opt.is_some_and(|n| b_ref.col_net[h as usize][col as usize] == n.0) {
                    return 0.0;
                }
                let groove_free = !b_ref.is_occupied(col, 4) && !b_ref.is_occupied(col, 5) && v_seg_free(fps, col, 4, 5);
                if !groove_free {
                    return -4.5;
                }
                let other_tag = b_ref.col_net[(1 - h) as usize][col as usize];
                let is_series_node = !is_pwr && net_opt.and_then(|n| total_net_pins.get(&n)).copied().unwrap_or(0) <= 2;
                if other_tag == 0 {
                    return if is_pwr { dyn_br_pen - 2.0 } else if is_series_node { dyn_br_pen } else { (dyn_br_pen * 0.62).round() };
                }
                if !is_pwr && net_opt.is_some_and(|n| other_tag == n.0) {
                    return -6.0;
                }
                if cross_half_nets.contains(&NetId(other_tag)) { 8.0 } else { -11.0 }
            };

            let prep_list = |h_list: &[(u8, i8)], net_opt: Option<NetId>, c_target: Option<f64>, la: Option<Lookahead>| -> Vec<PrepHole> {
                let is_pwr = net_opt.is_some_and(NetId::is_power);
                let net_masks = net_opt.filter(|n| !n.is_power()).and_then(|n| ctx.pres.get(&n)).copied();
                let net_set_size = net_masks.map_or(0, |m| (m[0].count_ones() + m[1].count_ones()) as usize);
                let tot_pins = net_opt.and_then(|n| total_net_pins.get(&n)).copied().unwrap_or(0);
                let placed_pins = net_opt.filter(|n| !n.is_power()).and_then(|n| ctx.holes.get(&n)).map_or(0, Vec::len);
                let is_two_pin_series = !is_pwr && tot_pins == 2 && placed_pins == 1;
                let dist_weight = if is_series_chain { 0.68 } else { 0.26 };
                let mut col_cache = [f64::NAN; 128];

                h_list.iter().map(|&(col, row)| {
                    let is_rail = is_rail_row(row);
                    if is_rail {
                        let dist = c_target.map_or(0.0, |t| (col as f64 - t).abs() * 0.26);
                        return PrepHole {
                            col, row, half: 2, is_rail: true, is_shared: false, is_pwr: true,
                            vy: vis_y(row), base: weights.spec_rail_base_bonus + dist + 0.003 * (col as f64),
                        };
                    }
                    let h = half_of(row);
                    let cache_idx = (h as usize) * 64 + (col as usize);
                    let is_shared = net_masks.is_some_and(|m| (m[h as usize] & (1u64 << col)) != 0);
                    let mut cc = col_cache[cache_idx];
                    if cc.is_nan() {
                        let free_in_strip = board.free_hole_count(col, h);
                        let cur_tag = board.col_net[h as usize][col as usize];
                        let is_ic_pwr_shared = is_pwr && Some(base_net_of(cur_tag)) == net_opt;
                        let is_aux_shared = is_pwr && net_opt.is_some_and(|n| cur_tag == pwr_aux_tag(n, h));
                        let all_in_strip = is_shared && net_set_size == 1 && (placed_pins + 1 == tot_pins) && !is_pwr && la.is_none();
                        let must_wire_later = !all_in_strip && (is_pwr || tot_pins > 1 || la.is_some());
                        let starve_pen = if free_in_strip <= 1 && must_wire_later { weights.spec_starve_pen } else { 0.0 };
                        let split_pen = if is_two_pin_series && !is_shared { weights.spec_split_series_pen } else { 0.0 };
                        let share = if is_shared {
                            sig_share_bonus
                        } else if is_ic_pwr_shared {
                            18.0
                        } else if is_aux_shared {
                            6.0
                        } else if is_pwr && net_opt.is_some_and(|n| can_straight_rail(board, fps, col, h, n)) {
                            p_cfg.rail_share
                        } else {
                            0.0
                        };
                        let dist = if !is_shared { c_target.map_or(0.0, |t| (col as f64 - t).abs() * dist_weight) } else { 0.0 };
                        let look = if !is_shared && la.is_some_and(|l| can_partner_ortho_share(col, h, l, board, fps, ctx)) {
                            let l_net = la.and_then(|l| l.net);
                            if l_net.is_some_and(|n| n.is_power() && can_direct_plug_rail(board, fps, col, h, n)) {
                                16.0
                            } else if p_cfg.led_first && c.comp_type == CompType::LED {
                                18.0
                            } else {
                                7.0
                            }
                        } else {
                            0.0
                        };
                        // Hạng mục #4: Khi led_first = true, phạt +150.0 nếu LED đặt cùng nửa bo nhưng cách chân neo IC của R dưới 3 ô (< 3)
                        let r_led_lock_pen = if p_cfg.led_first && c.comp_type == CompType::LED {
                            if let Some(l_net) = la.and_then(|l| l.net).filter(|n| !n.is_power()) {
                                if let Some(m) = ctx.pres.get(&l_net).map(|ms| ms[h as usize]) {
                                    let max_d = la.map_or(13, |l| l.sp.max.floor() as i32);
                                    let too_close = (1u8..=63).any(|tc| (m & (1u64 << tc)) != 0 && (tc as i32 - col as i32).abs() < 3);
                                    let has_valid = (1u8..=63).any(|tc| {
                                        let d = (tc as i32 - col as i32).abs();
                                        (m & (1u64 << tc)) != 0 && d >= 3 && d <= max_d
                                    });
                                    if too_close && !has_valid { 150.0 } else { 0.0 }
                                } else {
                                    0.0
                                }
                            } else {
                                0.0
                            }
                        } else {
                            0.0
                        };
                        let br_pen = if is_shared || is_aux_shared || is_ic_pwr_shared { 0.0 } else { check_bridge_cost(col, h, net_opt, board) };
                        let is_evicted = (opts.evict_cols_mask & (1u64 << col)) != 0;
                        let fb_col_pen = if !is_shared {
                            (if use_feedback { (col_hist[col as usize] as f64) * 2.5 } else { 0.0 })
                                + (if is_evicted { weights.spec_evict_col_pen } else { 0.0 })
                        } else {
                            0.0
                        };
                        cc = -share - look + dist + br_pen + fb_col_pen + split_pen + starve_pen + r_led_lock_pen + 0.003 * (col as f64);
                        col_cache[cache_idx] = cc;
                    }
                    let groove_pen = if row == 4 || row == 5 { weights.spec_groove_row_pen } else { 0.0 };
                    let strip_tag = board.col_net[h as usize][col as usize];
                    let is_pwr_strip = is_pwr || is_pwr_tag(strip_tag);
                    let rail_hole_pen = if row == 0 || row == 9 { if is_pwr_strip { 4.5 } else { 1.5 } } else { 0.0 };
                    let fb_row_pen = if use_feedback { (row_hist[row as usize] as f64) * 3.0 } else { 0.0 };
                    PrepHole {
                        col, row, half: h, is_rail: false, is_shared, is_pwr,
                        vy: vis_y(row), base: cc + groove_pen + rail_hole_pen + fb_row_pen,
                    }
                }).collect()
            };
            (prep_list(&holes_a, n1, c_target1, la1), prep_list(&holes_b, n2, c_target2, la2))
        };

        let mut b_by_col: [Vec<PrepHole>; 65] = core::array::from_fn(|_| Vec::new());
        for b in prep_b {
            b_by_col[b.col as usize].push(b);
        }

        let placed_specials: Vec<([f64; 2], [f64; 2], u8, u8)> = layout.rs.iter()
            .chain(layout.leds.iter())
            .chain(layout.cs.iter().filter(|x| !x.locked))
            .map(|pl| (
                [pl.c1 as f64, vis_y(pl.r1)],
                [pl.c2 as f64, vis_y(pl.r2)],
                pl.c1.min(pl.c2).saturating_sub(1),
                pl.c1.max(pl.c2).saturating_add(1),
            ))
            .collect();

        let (mut best_pair, mut bs) = (None, 1e9_f64);
        let max_span_i = sp.max.floor() as i32;

        for a in &prep_a {
            let c_min_b = (a.col as i32 - max_span_i).max(1) as u8;
            let c_max_b = (a.col as i32 + max_span_i).min(63) as u8;
            for c_b in c_min_b..=c_max_b {
                for b in &b_by_col[c_b as usize] {
                    if (a.is_rail && b.is_rail) || (!a.is_rail && !b.is_rail && a.col == b.col && a.half == b.half) {
                        continue;
                    }
                    let one_rail = a.is_rail != b.is_rail;
                    // Hạng mục #1: Khi 1 chân ở thanh nguồn (one_rail), tính khoảng cách dọc qua vis_y
                    let dx = (a.col as f64) - (b.col as f64);
                    let dy = if one_rail {
                        (a.vy - b.vy).abs()
                    } else {
                        (a.row - b.row).abs() as f64
                    };
                    let d_sq = dx * dx + dy * dy;
                    if d_sq < min_sq || d_sq > max_sq {
                        continue;
                    }
                    let mut outer_rail_pen = 0.0;
                    if a.col == b.col {
                        if (a.row == -2 || b.row == -2) && board.is_occupied(a.col, -1) { continue; }
                        if (a.row == 11 || b.row == 11) && board.is_occupied(a.col, 10) { continue; }
                        if (one_rail && matches!(a.row, -2 | 11)) || (one_rail && matches!(b.row, -2 | 11)) {
                            outer_rail_pen = 2.0;
                        }
                    }
                    let d = d_sq.sqrt();
                    let mut edge_snap_adj = 0.0;
                    if one_rail {
                        let bd = if a.is_rail { b } else { a };
                        if !is_pwr_tag(board.col_net[bd.half as usize][bd.col as usize]) {
                            edge_snap_adj = if bd.row == 0 || bd.row == 9 { -5.5 } else if bd.row == 1 || bd.row == 8 { -1.5 } else { 3.5 };
                        }
                    } else if c.comp_type == CompType::R && (a.is_pwr != b.is_pwr) {
                        // Hạng mục #4: Chỉ phạt +120.0 vượt rãnh E-F đối với điện trở nối nguồn; cho phép trở tín hiệu nối tiếp bước qua rãnh
                        if a.col != b.col { edge_snap_adj += 35.0; }
                        if (a.row <= 4) != (b.row <= 4) { edge_snap_adj += 120.0; }
                    }

                    // Hạng mục #4: Mở ngưỡng thưởng both_anchored lên toàn dải d <= sp.max (13.5, thưởng -38.0)
                    let both_anchored = a.is_shared && b.is_shared && !one_rail;
                    let span_pen = if both_anchored && d <= sp.max {
                        (d - 3.0).max(0.0) * 0.12 - 38.0
                    } else {
                        (d - sp.pref).abs() * if is_series_chain && d > 8.0 { 1.15 } else { 0.38 }
                    };
                    let is_ortho = a.col == b.col || a.row == b.row;
                    let horiz_edge_pen = if !one_rail && a.row == b.row && (a.row == 0 || a.row == 9) { 10.0 } else { 0.0 };
                    let sym_bonus = eval_sym_led_bonus(a.col, a.row, b.col, b.row, &sym_partners, weights.sym_led_pair_bonus);
                    let jit = r_rng.next_f64() * if use_feedback { 1.4 } else { 0.75 };
                    let sc = span_pen + a.base + b.base + outer_rail_pen + edge_snap_adj + horiz_edge_pen
                        - (if is_ortho { ortho_w } else { 0.0 }) - sym_bonus + jit;
                    if sc >= bs {
                        continue;
                    }
                    let (p1, p2) = ([a.col as f64, a.vy], [b.col as f64, b.vy]);
                    let (seg_min_c, seg_max_c) = (a.col.min(b.col), a.col.max(b.col));
                    let colliding = placed_specials.iter().any(|&(pl1, pl2, min_c, max_c)| {
                        seg_max_c >= min_c && seg_min_c <= max_c && seg_dist_sq(p1, p2, pl1, pl2) < weights.spec_body_clearance_sq
                    });
                    if !colliding {
                        bs = sc;
                        best_pair = Some(((a.col, a.row), (b.col, b.row)));
                    }
                }
            }
        }

        let Some((a_pt, b_pt)) = best_pair else {
            errors.push(format!("{}: không có 2 lỗ trống thỏa khoảng cách {}-{} hoặc bị đè lên linh kiện khác", c.id, sp.min, sp.max));
            *unplaced += 1;
            continue;
        };
        board.occupy(a_pt.0, a_pt.1);
        board.occupy(b_pt.0, b_pt.1);
        if a_pt.0 == b_pt.0 {
            if a_pt.1 == -2 || b_pt.1 == -2 { board.occupy(a_pt.0, -1); }
            if a_pt.1 == 11 || b_pt.1 == 11 { board.occupy(a_pt.0, 10); }
        }
        if n1.is_none() && !is_rail_row(a_pt.1) {
            board.col_net[half_of(a_pt.1) as usize][a_pt.0 as usize] = nc_tag(c.cid, 1);
        }
        if n2.is_none() && !is_rail_row(b_pt.1) {
            board.col_net[half_of(b_pt.1) as usize][b_pt.0 as usize] = nc_tag(c.cid, 2);
        }
        let placed = PlacedSpecial {
            cid: c.cid, id: c.id.clone(), comp_type: c.comp_type,
            c1: a_pt.0, r1: a_pt.1, c2: b_pt.0, r2: b_pt.1, locked: false, ic_id: None,
        };
        match c.comp_type {
            CompType::R => layout.rs.push(placed),
            CompType::LED => layout.leds.push(placed),
            _ => layout.cs.push(placed),
        }
        placed_special_ids.insert(c.cid);
        ctx.add_pin(board, c.cid, 1, a_pt.0, a_pt.1, all_comps, parsed, errors);
        ctx.add_pin(board, c.cid, 2, b_pt.0, b_pt.1, all_comps, parsed, errors);
        update_net_centers(ctx, &mut net_center);
    }

    for n in &parsed.nets {
        if !n.id.is_power() {
            if let Some(h_list) = ctx.holes.get(&n.id) {
                if h_list.iter().any(|h| h.row <= 4) && h_list.iter().any(|h| h.row >= 5) {
                    cross_half_nets.insert(n.id);
                }
            }
        }
    }
    cross_half_nets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discrete_place::decoupling::setup_macro_and_decoupling;
    use crate::macro_place::topology::build_topology_cache;
    use crate::model::circuit::PlacedIc;
    use crate::parser::parse_netlist;

    #[test]
    fn test_pullup_ortho_row0_and_symmetric_leds() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_other(CompId(1), "R1", CompType::R),
            CompSpec::new_other(CompId(2), "R2", CompType::R),
            CompSpec::new_other(CompId(3), "R3", CompType::R),
            CompSpec::new_other(CompId(4), "L1", CompType::LED),
            CompSpec::new_other(CompId(5), "L2", CompType::LED),
        ];
        // R1 là điện trở kéo nguồn nối từ U1:13 (cột 11, nửa trên) lên VCC
        // L1 (nửa trên, qua R2 nối U1:12 ở cột 12) và L2 (nửa dưới, qua R3 nối U1:3 ở cột 12) đối xứng gương #SYM_LED: L1=L2
        let txt = "
            #SYM_LED: L1=L2
            VCC U1:14; GND U1:7
            VCC -> R1 -> U1:13
            U1:12 -> R2 -> L1 -> GND
            U1:3 -> R3 -> L2 -> GND
        ";
        let parsed = parse_netlist(txt, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        let weights = CostWeights::default();

        let mut layout = BoardLayout::default();
        layout.ics.push(PlacedIc {
            cid: CompId(0), id: "U1".into(), pins: 14, rot: 0, c_min: 10, c_max: 16, w: 7,
        });
        let (mut board, mut fps) = (BoardState::new(false), Vec::new());
        layout.block_space(&mut board, &mut fps);

        let (mut errors, mut unplaced) = (Vec::new(), 0usize);
        let mut ctx = setup_macro_and_decoupling(&mut board, &mut layout, &parsed, &topo, &comps, &weights, &mut errors);
        let snap_ic = IcSnapshot::capture(&board, &ctx, unplaced, errors.len());
        let (col_hist, row_hist) = ([0.0f32; 65], [0.0f32; 10]);
        let opts = SpecialOptions { persona: 2, sr: 1, ..Default::default() };

        place_special_components(
            false, 0, &opts, &snap_ic, &mut board, &mut layout, &mut ctx,
            &fps, &comps, &comps, &parsed, &col_hist, &row_hist, &weights, &mut unplaced, &mut errors,
        );

        assert_eq!(unplaced, 0);
        assert!(errors.is_empty(), "Errors: {:?}", errors);

        // Hạng mục #1: Trở kéo nguồn R1 đứng thẳng góc 90° từ lỗ Hàng A (row = 0) lên thẳng Thanh Nguồn (row = -1)
        let r1 = layout.rs.iter().find(|r| r.id == "R1").unwrap();
        assert_eq!(r1.c1, r1.c2);
        assert_eq!((r1.r1.min(r1.r2), r1.r1.max(r1.r2)), (-1, 0));

        // Hạng mục #10: L1 và L2 đặt đối xứng gương qua rãnh E-F (cùng cột, 1 con ở nửa trên, 1 con ở nửa dưới)
        let l1 = layout.leds.iter().find(|l| l.id == "L1").unwrap();
        let l2 = layout.leds.iter().find(|l| l.id == "L2").unwrap();
        assert_eq!((l1.c1, l1.c2), (l2.c1, l2.c2));
        assert_ne!(half_of(l1.r1), half_of(l2.r1));
    }
}
