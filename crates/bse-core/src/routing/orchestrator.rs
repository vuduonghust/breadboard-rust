use crate::config::personas::PersonaConfig;
use crate::config::weights::CostWeights;
use crate::discrete_place::decoupling::{is_pwr_tied_tag, pwr_direct_tag, DiscreteContext};
use crate::macro_place::cost::pick_best_cross_pair;
use crate::model::bitboard::{half_of, rail_ok, BoardState};
use crate::model::circuit::{BoardLayout, ParsedNetlist};
use crate::model::geometry::{v_seg_free, Footprint};
use crate::model::ids::{Mulberry32, NetId};
use crate::routing::h_route::{is_potential_bridge_col, FastWire, MovableSpecPin, PassWires};
use crate::routing::rail::{rail_connect, PwrSegTracker};
use crate::routing::steiner::{connect_steiner, strip_idx, StripUf};
use rustc_hash::{FxHashMap, FxHashSet};

/// Một nhiệm vụ nối dây đơn lẻ trong danh sách `items` của `runRoutingEngine`
#[derive(Clone, Copy, Debug)]
pub struct RouteItem {
    pub net: NetId,
    pub tag: u16,
    pub rail: bool,
    pub cross: bool,
    pub a: (u8, i8),
    pub b: (u8, i8),
    pub star_hub: Option<(u8, u8)>,
    pub pri: f64,
}

/// Kết quả định tuyến của 1 Pass (`bestPass` trong main.html)
#[derive(Clone, Debug, Default)]
pub struct RoutingPassResult {
    pub wires: Vec<FastWire>,
    pub w_len: f64,
    pub failed: Vec<usize>,
    pub failed_items: Vec<RouteItem>,
    pub spec_pins: Vec<MovableSpecPin>,
    pub errs: Vec<String>,
    pub fail_span: i32,
    pub item_spans: Vec<Option<(u8, u8)>>,
}

impl RoutingPassResult {
    /// Đồng bộ tọa độ các chân linh kiện rời (R, LED, C) đã trượt 1 hàng trong Pass tốt nhất về `BoardLayout` (#5)
    pub fn apply_spec_pins(&self, layout: &mut BoardLayout) {
        if self.spec_pins.is_empty() {
            return;
        }
        for s in layout.cs.iter_mut().chain(&mut layout.leds).chain(&mut layout.rs) {
            if let Some(mp) = self.spec_pins.iter().find(|p| p.cid == s.cid) {
                s.r1 = mp.r1;
                s.r2 = mp.r2;
            }
        }
    }
}

fn push_steiner_net_items(
    net_id: NetId,
    reps: &[(u8, i8)],
    boost_nets: &FxHashSet<NetId>,
    p_cfg: &PersonaConfig,
    layout: &BoardLayout,
    board: &BoardState,
    weights: &CostWeights,
    items: &mut Vec<RouteItem>,
) {
    let pre_boost = if boost_nets.contains(&net_id) { -420.0 } else { 0.0 };
    let mut top_r: Vec<(u8, i8)> = reps.iter().copied().filter(|h| h.1 <= 4).collect();
    let mut bot_r: Vec<(u8, i8)> = reps.iter().copied().filter(|h| h.1 >= 5).collect();
    top_r.sort_by_key(|h| h.0);
    bot_r.sort_by_key(|h| h.0);

    let fanout_bonus = if p_cfg.short_first && reps.len() >= 3 { 16.0 } else { 0.0 };
    for i in 1..top_r.len() {
        let d = (top_r[i].0 as i32 - top_r[i - 1].0 as i32).abs() as f64;
        let h_pri = if p_cfg.short_first { -95.0 + d * 0.75 - fanout_bonus } else { -90.0 - d };
        items.push(RouteItem {
            net: net_id,
            tag: net_id.0,
            rail: false,
            cross: false,
            a: top_r[i - 1],
            b: top_r[i],
            star_hub: None,
            pri: h_pri + pre_boost,
        });
    }
    for i in 1..bot_r.len() {
        let d = (bot_r[i].0 as i32 - bot_r[i - 1].0 as i32).abs() as f64;
        let h_pri = if p_cfg.short_first { -95.0 + d * 0.75 - fanout_bonus } else { -90.0 - d };
        items.push(RouteItem {
            net: net_id,
            tag: net_id.0,
            rail: false,
            cross: false,
            a: bot_r[i - 1],
            b: bot_r[i],
            star_hub: None,
            pri: h_pri + pre_boost,
        });
    }
    if !top_r.is_empty() && !bot_r.is_empty() {
        let top_arr: Vec<[i8; 2]> = top_r.iter().map(|&(c, r)| [c as i8, r]).collect();
        let bot_arr: Vec<[i8; 2]> = bot_r.iter().map(|&(c, r)| [c as i8, r]).collect();
        let (best_t, best_b, min_d) =
            pick_best_cross_pair(&top_arr, &bot_arr, &layout.ics, board, net_id.0, weights);
        items.push(RouteItem {
            net: net_id,
            tag: net_id.0,
            rail: false,
            cross: true,
            a: (best_t[0] as u8, best_t[1]),
            b: (best_b[0] as u8, best_b[1]),
            star_hub: None,
            pri: p_cfg.cross_pri - (min_d as f64) * 0.35 + pre_boost,
        });
    }
}

fn try_setup_star_hub(
    net_id: NetId,
    reps: &[(u8, i8)],
    board: &mut BoardState,
    fps: &[Footprint],
    items: &mut Vec<RouteItem>,
    extra_bridges: &mut Vec<((u8, u8), (u8, u8))>,
    star_bridge_wires: &mut Vec<FastWire>,
) -> bool {
    let mut top_p: Vec<(u8, i8)> = reps.iter().copied().filter(|h| h.1 <= 4).collect();
    let mut bot_p: Vec<(u8, i8)> = reps.iter().copied().filter(|h| h.1 >= 5).collect();
    top_p.sort_by_key(|h| h.0);
    bot_p.sort_by_key(|h| h.0);

    let mut used_hub_cols = [0u64; 2];
    let pick_hub_col = |board: &BoardState,
                        used: &[u64; 2],
                        h: u8,
                        target_c: f64,
                        need_free: u8,
                        need_bridge: bool,
                        need_bot_free: u8|
     -> Option<u8> {
        let (mut best_c, mut best_d) = (None, 1e9_f64);
        for c in 1u8..=63 {
            if (used[h as usize] & (1u64 << c)) != 0
                || board.col_net[h as usize][c as usize] != 0
                || board.free_hole_count(c, h) < need_free
            {
                continue;
            }
            if need_bridge
                && (board.col_net[(1 - h) as usize][c as usize] != 0
                    || board.free_hole_count(c, 1 - h) < need_bot_free
                    || board.is_occupied(c, 4)
                    || board.is_occupied(c, 5)
                    || !v_seg_free(fps, c, 4, 5))
            {
                continue;
            }
            let br_pen = if !need_bridge && is_potential_bridge_col(board, fps, c, 0, 1) {
                14.0
            } else {
                0.0
            };
            let d = (c as f64 - target_c).abs() + br_pen;
            if d < best_d {
                best_d = d;
                best_c = Some(c);
            }
        }
        best_c
    };

    let build_half_hubs = |pts: &[(u8, i8)],
                           h: u8,
                           need_bridge_first: bool,
                           first_bot_free: u8,
                           used: &mut [u64; 2]|
     -> Option<Vec<(u8, u8, Vec<(u8, i8)>)>> {
        if pts.is_empty() {
            return Some(Vec::new());
        }
        let chunks: Vec<&[(u8, i8)]> = pts.chunks(3).collect();
        let mut hubs = Vec::with_capacity(chunks.len());
        for (idx, &ch) in chunks.iter().enumerate() {
            let avg: f64 = ch.iter().map(|p| p.0 as f64).sum::<f64>() / (ch.len() as f64);
            let need_free = 4.min(
                ch.len()
                    + usize::from(idx == 0 && need_bridge_first)
                    + usize::from(chunks.len() > 1),
            ) as u8;
            let hc = pick_hub_col(
                board,
                used,
                h,
                avg,
                need_free,
                idx == 0 && need_bridge_first,
                first_bot_free,
            )?;
            used[h as usize] |= 1u64 << hc;
            hubs.push((hc, h, ch.to_vec()));
        }
        Some(hubs)
    };

    let need_br = !top_p.is_empty() && !bot_p.is_empty();
    let first_bot_len = if need_br { bot_p.len().min(3) } else { 0 };
    let rest_bot_p = if need_br { &bot_p[first_bot_len..] } else { &bot_p[..] };
    let need_bot_free = if need_br {
        4.min(1 + first_bot_len + usize::from(!rest_bot_p.is_empty())) as u8
    } else {
        2
    };

    let Some(top_hubs) = build_half_hubs(&top_p, 0, need_br, need_bot_free, &mut used_hub_cols)
    else {
        return false;
    };
    let bot_hubs = if need_br && !top_hubs.is_empty() {
        let bc = top_hubs[0].0;
        used_hub_cols[1] |= 1u64 << bc;
        let Some(mut extra) = build_half_hubs(rest_bot_p, 1, false, 2, &mut used_hub_cols) else {
            return false;
        };
        let mut v = vec![(bc, 1u8, bot_p[..first_bot_len].to_vec())];
        v.append(&mut extra);
        v
    } else {
        let Some(v) = build_half_hubs(&bot_p, 1, false, 2, &mut used_hub_cols) else {
            return false;
        };
        v
    };

    if need_br && !top_hubs.is_empty() {
        let bc = top_hubs[0].0;
        board.col_net[0][bc as usize] = net_id.0;
        board.col_net[1][bc as usize] = net_id.0;
        board.occupy(bc, 4);
        board.occupy(bc, 5);
        extra_bridges.push(((bc, 0), (bc, 1)));
        star_bridge_wires.push(FastWire::new_v(net_id, bc, 4, bc, 5));
    }
    for (hc, hh, ch) in top_hubs.iter().chain(bot_hubs.iter()) {
        board.col_net[*hh as usize][*hc as usize] = net_id.0;
        let hub_row = if *hh != 0 { 7 } else { 2 };
        for &r_pt in ch {
            items.push(RouteItem {
                net: net_id,
                tag: net_id.0,
                rail: false,
                cross: false,
                a: (*hc, hub_row),
                b: r_pt,
                star_hub: Some((*hc, *hh)),
                pri: -150.0 - (r_pt.0 as f64 - *hc as f64).abs(),
            });
        }
    }
    for i in 1..top_hubs.len() {
        items.push(RouteItem {
            net: net_id,
            tag: net_id.0,
            rail: false,
            cross: false,
            a: (top_hubs[i - 1].0, 2),
            b: (top_hubs[i].0, 2),
            star_hub: Some((top_hubs[i - 1].0, 0)),
            pri: -160.0,
        });
    }
    for i in 1..bot_hubs.len() {
        items.push(RouteItem {
            net: net_id,
            tag: net_id.0,
            rail: false,
            cross: false,
            a: (bot_hubs[i - 1].0, 7),
            b: (bot_hubs[i].0, 7),
            star_hub: Some((bot_hubs[i - 1].0, 1)),
            pri: -160.0,
        });
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn execute_single_pass(
    order: &[usize],
    items: &[RouteItem],
    flip_rev: bool,
    cand_lim: usize,
    s0: &BoardState,
    board: &mut BoardState,
    layout: &BoardLayout,
    fps: &[Footprint],
    ctx: &DiscreteContext,
    extra_bridges: &[((u8, u8), (u8, u8))],
    star_bridge_wires: &[FastWire],
    parsed: &ParsedNetlist,
    p_cfg: &PersonaConfig,
    col_hist: &mut [f32; 65],
    row_hist: &mut [f32; 10],
    weights: &CostWeights,
) -> RoutingPassResult {
    *board = *s0;
    if flip_rev {
        board.rev = !s0.rev;
    }
    for c in 1..=63 {
        col_hist[c] *= 0.92;
    }
    for r in 0..10 {
        row_hist[r] *= 0.92;
    }

    let mut uf = StripUf::new();
    for &(a, b) in ctx.btn_bridges.iter().chain(extra_bridges.iter()) {
        uf.union(strip_idx(a.0, a.1), strip_idx(b.0, b.1));
    }

    let mut pass_err = Vec::new();
    let mut pass_wires = PassWires::with_layout(p_cfg.swap, layout);
    pass_wires.wires.extend_from_slice(star_bridge_wires);
    let mut pwr_segs = PwrSegTracker::new();
    let mut failed = Vec::new();
    let mut failed_items = Vec::new();
    let mut item_spans = vec![None; items.len()];
    let mut fail_span = 0i32;
    let mut rem_cross = order.iter().filter(|&&idx| items[idx].cross).count() as i32;

    for &idx in order {
        let it = items[idx];
        let before = pass_err.len();
        let res = if it.rail {
            rail_connect(
                board, &mut pass_wires, &mut pwr_segs, fps, &mut uf,
                it.net, it.tag, &[it.a], rem_cross, col_hist, row_hist, weights, &mut pass_err,
            )
        } else {
            match connect_steiner(
                board, &mut pass_wires, fps, &mut uf, it.tag, it.a, it.b,
                cand_lim, it.star_hub, rem_cross, col_hist, row_hist, weights,
            ) {
                Some(w) => w,
                None => {
                    let net_name = parsed
                        .nets
                        .iter()
                        .find(|n| n.id == it.net)
                        .map_or("?", |n| n.name.as_str());
                    pass_err.push(format!(
                        "Net {net_name}: không đi được dây từ cột {} tới cột {}",
                        it.a.0, it.b.0
                    ));
                    Vec::new()
                }
            }
        };
        if it.cross {
            rem_cross -= 1;
        }
        if pass_err.len() > before {
            failed.push(idx);
            failed_items.push(it);
            let (ca, cb) = (it.a.0, if it.rail { it.a.0 } else { it.b.0 });
            let lo = ca.min(cb).saturating_sub(3).max(1);
            let hi = (ca.max(cb) + 3).min(63);
            let span = if !it.rail {
                (ca as i32 - cb as i32).abs()
                    + if half_of(it.a.1) != half_of(it.b.1) { 12 } else { 4 }
            } else {
                10
            };
            fail_span += span;
            for c in lo..=hi {
                col_hist[c as usize] += if c == ca || c == cb { 3.5 } else { 1.8 };
            }
            if (0..10).contains(&it.a.1) {
                row_hist[it.a.1 as usize] += 1.0;
            }
            if !it.rail && (0..10).contains(&it.b.1) {
                row_hist[it.b.1 as usize] += 1.0;
            }
        } else {
            if !res.is_empty() {
                let (mut min_c, mut max_c) = (63u8, 1u8);
                for seg in &res {
                    min_c = min_c.min(seg.c1).min(seg.c2);
                    max_c = max_c.max(seg.c1).max(seg.c2);
                }
                item_spans[idx] = Some((min_c, max_c));
            }
            pass_wires.wires.extend(res);
            pass_wires.commit_swaps();
        }
    }

    let w_len: f64 = pass_wires
        .wires
        .iter()
        .map(|w| (w.c2 as f64 - w.c1 as f64).hypot(w.r2 as f64 - w.r1 as f64))
        .sum();

    RoutingPassResult {
        wires: pass_wires.wires,
        w_len,
        failed,
        failed_items,
        spec_pins: pass_wires.spec_pins,
        errs: pass_err,
        fail_span,
        item_spans,
    }
}

/// Động cơ Đi dây Đa lượt có Phản hồi Lịch sử Nghẽn (`runRoutingEngine` trong main.html)
#[allow(clippy::too_many_arguments)]
pub fn run_routing_engine(
    pass_budget: usize,
    use_star_hub: bool,
    net_ord: usize,
    boost_nets: &FxHashSet<NetId>,
    p_cfg: &PersonaConfig,
    rng: &mut Option<Mulberry32>,
    board: &mut BoardState,
    layout: &BoardLayout,
    ctx: &DiscreteContext,
    fps: &[Footprint],
    parsed: &ParsedNetlist,
    col_hist: &mut [f32; 65],
    row_hist: &mut [f32; 10],
    weights: &CostWeights,
) -> RoutingPassResult {
    let base_board = *board;
    let mut items = Vec::with_capacity(64);
    let mut items_no_star = Vec::with_capacity(64);
    let mut star_bridge_wires = Vec::new();
    let mut extra_bridges = Vec::new();
    let mut has_star_hub_active = false;

    for net in &parsed.nets {
        let Some(h_list) = ctx.holes.get(&net.id) else { continue };
        if net.id.is_power() {
            let mut seen_strip = [0u64; 2];
            for h in h_list {
                let half = half_of(h.row);
                let bit = 1u64 << h.col;
                if (seen_strip[half as usize] & bit) != 0 {
                    continue;
                }
                seen_strip[half as usize] |= bit;
                let strip_tag = board.col_net[half as usize][h.col as usize];
                let is_aux_ic_pwr = is_pwr_tied_tag(h.tag) || is_pwr_tied_tag(strip_tag);
                let t = if is_aux_ic_pwr {
                    if is_pwr_tied_tag(strip_tag) { strip_tag } else { h.tag }
                } else {
                    pwr_direct_tag(net.id, h.col, half)
                };
                let blocked =
                    board.is_occupied(h.col, if h.row <= 4 { 0 } else { 9 }) || !rail_ok(h.col);
                // Hạng mục #6: Chân nguồn phụ trên thân IC (như U2:4, U2:10) nếu bị chặn lỗ thẳng
                // thì hạ ưu tiên xuống sau dây vượt rãnh để không cướp cầu rãnh E-F quanh U2!
                let pri = if !blocked {
                    -155.0
                } else if is_aux_ic_pwr {
                    p_cfg.cross_pri + 18.0
                } else {
                    -185.0
                };
                let pwr_item = RouteItem {
                    net: net.id,
                    tag: t,
                    rail: true,
                    cross: false,
                    a: (h.col, h.row),
                    b: (h.col, h.row),
                    star_hub: None,
                    pri,
                };
                items.push(pwr_item);
                items_no_star.push(pwr_item);
            }
            continue;
        }

        let mut seen_strip = [0u64; 2];
        let mut reps = Vec::with_capacity(h_list.len());
        for h in h_list {
            let half = half_of(h.row);
            let bit = 1u64 << h.col;
            if (seen_strip[half as usize] & bit) == 0 {
                seen_strip[half as usize] |= bit;
                reps.push((h.col, h.row));
            }
        }
        if reps.len() < 2 {
            continue;
        }

        push_steiner_net_items(
            net.id, &reps, boost_nets, p_cfg, layout, &base_board, weights, &mut items_no_star,
        );

        if use_star_hub
            && net.is_star
            && try_setup_star_hub(
                net.id,
                &reps,
                board,
                fps,
                &mut items,
                &mut extra_bridges,
                &mut star_bridge_wires,
            )
        {
            has_star_hub_active = true;
            continue;
        }

        push_steiner_net_items(
            net.id, &reps, boost_nets, p_cfg, layout, board, weights, &mut items,
        );
    }

    let mut s0 = *board;
    let sort_by = |items_ref: &[RouteItem], rng_opt: &mut Option<Mulberry32>, jit: f64| -> Vec<usize> {
        let mut keyed: Vec<(f64, usize)> = items_ref
            .iter()
            .enumerate()
            .map(|(i, it)| {
                let noise = rng_opt.as_mut().map_or(0.0, |r| r.next_f64() * jit);
                (it.pri + noise, i)
            })
            .collect();
        keyed.sort_by(|x, y| x.0.total_cmp(&y.0));
        keyed.into_iter().map(|x| x.1).collect()
    };

    let mut order = sort_by(&items, rng, (net_ord as f64) * 4.0);
    let mut best_pass: Option<RoutingPassResult> = None;
    let mut no_imp = 0usize;
    let mut polish_left = 2i32;
    let half_budget = (pass_budget / 2).max(2);

    for ps in 0..pass_budget.max(1) {
        // Hạng mục #9: Nếu ở nửa sau Pass (ps >= pass_budget / 2) vẫn còn lỗi vượt rãnh/thiếu cột trống,
        // cho phép mạng *CLK/*RESET_n hạ cấp sang nối chuỗi Steiner trực tiếp để trả lại 2–4 cột cầu rãnh!
        if has_star_hub_active
            && ps >= half_budget
            && best_pass.as_ref().is_some_and(|bp| !bp.errs.is_empty())
        {
            has_star_hub_active = false;
            let mut net_min_pri: FxHashMap<NetId, f64> = FxHashMap::default();
            for it in &items {
                let e = net_min_pri.entry(it.net).or_insert(it.pri);
                if it.pri < *e {
                    *e = it.pri;
                }
            }
            for it in &mut items_no_star {
                if let Some(&mp) = net_min_pri.get(&it.net) {
                    if mp < -200.0 {
                        it.pri = mp;
                    }
                }
            }
            items = items_no_star.clone();
            extra_bridges.clear();
            star_bridge_wires.clear();
            s0 = base_board;
            order = sort_by(&items, rng, 6.0 + (ps as f64) * 2.5);
            no_imp = 0;
        }

        let is_near_solved = best_pass
            .as_ref()
            .is_some_and(|bp| !bp.errs.is_empty() && bp.errs.len() <= 2);
        let flip = ps > 0
            && (ps % 3 == 2
                || (best_pass.as_ref().is_some_and(|bp| bp.errs.is_empty()) && ps % 2 == 1));
        let cand_lim = if is_near_solved {
            if p_cfg.id == 4 { 56 } else { 48 }
        } else if ps == 0 {
            20
        } else {
            26
        };

        let res = execute_single_pass(
            &order, &items, flip, cand_lim, &s0, board, layout, fps, ctx,
            &extra_bridges, &star_bridge_wires, parsed, p_cfg, col_hist, row_hist, weights,
        );

        let is_better = match &best_pass {
            None => true,
            Some(bp) => {
                res.errs.len() < bp.errs.len()
                    || (res.errs.len() == bp.errs.len()
                        && !res.errs.is_empty()
                        && res.fail_span < bp.fail_span)
                    || (res.errs.is_empty()
                        && bp.errs.is_empty()
                        && (res.w_len + (res.wires.len() as f64) * 3.5)
                            < (bp.w_len + (bp.wires.len() as f64) * 3.5))
            }
        };
        if is_better {
            best_pass = Some(res.clone());
            no_imp = 0;
        } else {
            no_imp += 1;
        }

        let bp = best_pass.as_ref().unwrap();
        if bp.errs.is_empty() {
            if polish_left <= 0 {
                break;
            }
            polish_left -= 1;
            order = sort_by(&items, rng, 5.0 + (ps as f64) * 2.0);
            continue;
        }
        // Hạng mục #9: Nới phễu cắt tỉa sớm Pass 0–6 cho mạch 6 IC
        let errs = bp.errs.len();
        if (ps == 0 && errs > 14)
            || (ps >= 3 && errs > 7)
            || (ps >= 6 && errs > 4)
            || no_imp >= (if is_near_solved { 14 } else { 7 })
        {
            break;
        }

        let mut fail_cols_mask = 0u64;
        let mut fail_ranges = Vec::with_capacity(res.failed.len());
        let mut tied_tags_boost: FxHashMap<u16, f64> = FxHashMap::default();

        for &f_idx in &res.failed {
            let it = &mut items[f_idx];
            let (c1, c2) = (it.a.0, if it.rail { it.a.0 } else { it.b.0 });
            fail_cols_mask |= (1u64 << c1) | (1u64 << c2);
            fail_ranges.push((c1.min(c2).saturating_sub(1), c1.max(c2).saturating_add(1)));
            it.pri = it.pri.min(-195.0) - weights.route_failed_boost_step * ((ps + 1) as f64);
            if it.rail && is_pwr_tied_tag(it.tag) {
                tied_tags_boost.insert(it.tag, it.pri);
            }
        }

        for (idx, it) in items.iter_mut().enumerate() {
            if !it.rail {
                if let Some(&boost_p) = tied_tags_boost.get(&it.tag) {
                    let d = (it.b.0 as i32 - it.a.0 as i32).abs() as f64;
                    it.pri = it.pri.min(boost_p - 30.0 - d);
                    continue;
                }
            }
            if res.failed.contains(&idx) {
                continue;
            }
            if (fail_cols_mask & (1u64 << it.a.0)) != 0
                || (!it.rail && (fail_cols_mask & (1u64 << it.b.0)) != 0)
            {
                it.pri += 42.0;
                continue;
            }
            if let Some((s_min, s_max)) = res.item_spans[idx] {
                if fail_ranges.iter().any(|&(f1, f2)| s_min.max(f1) <= s_max.min(f2)) {
                    it.pri += 24.0;
                }
            }
        }
        order = sort_by(&items, rng, 7.0 + (ps as f64) * 3.5);
    }

    board.rev = base_board.rev;
    best_pass.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::personas::get_persona;
    use crate::discrete_place::{place_special_components, setup_macro_and_decoupling, IcSnapshot, SpecialOptions};
    use crate::macro_place::topology::build_topology_cache;
    use crate::model::circuit::{CompSpec, PlacedIc};
    use crate::model::ids::{CompId, CompType};
    use crate::parser::parse_netlist;

    #[test]
    fn test_run_routing_engine_routes_full_circuit_zero_errors() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_ic(CompId(1), "U2", 14, 0),
            CompSpec::new_other(CompId(2), "CC1", CompType::CC),
            CompSpec::new_other(CompId(3), "R1", CompType::R),
            CompSpec::new_other(CompId(4), "L1", CompType::LED),
        ];
        let txt = "
            VCC U1:14 U2:14; GND U1:7 U2:7
            VCC -> CC1 -> GND
            *CLK: U1:1 -> U2:2 -> U2:12
            U1:3 -> R1 -> L1 -> GND
        ";
        let parsed = parse_netlist(txt, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        let weights = CostWeights::default();

        let mut layout = BoardLayout::default();
        layout.ics.push(PlacedIc { cid: CompId(0), id: "U1".into(), pins: 14, rot: 0, c_min: 10, c_max: 16, w: 7 });
        layout.ics.push(PlacedIc { cid: CompId(1), id: "U2".into(), pins: 14, rot: 0, c_min: 20, c_max: 26, w: 7 });

        let mut board = BoardState::new(false);
        let mut fps = Vec::new();
        layout.block_space(&mut board, &mut fps);

        let mut errors = Vec::new();
        let mut unplaced = 0usize;
        let mut ctx = setup_macro_and_decoupling(&mut board, &mut layout, &parsed, &topo, &comps, &weights, &mut errors);
        let snap_ic = IcSnapshot::capture(&board, &ctx, unplaced, errors.len());

        let mut col_hist = [0.0f32; 65];
        let mut row_hist = [0.0f32; 10];
        let spec_opts = SpecialOptions { persona: 2, sr: 1, ..Default::default() };

        place_special_components(
            false, 0, &spec_opts, &snap_ic, &mut board, &mut layout, &mut ctx,
            &fps, &comps, &comps, &parsed, &col_hist, &row_hist, &weights, &mut unplaced, &mut errors,
        );

        let mut rng = Some(Mulberry32::new(1));
        let res = run_routing_engine(
            10, true, 0, &FxHashSet::default(), &get_persona(2), &mut rng,
            &mut board, &layout, &ctx, &fps, &parsed, &mut col_hist, &mut row_hist, &weights,
        );
        assert!(res.errs.is_empty(), "Routing errors: {:?}", res.errs);
        assert!(!res.wires.is_empty());
    }

    #[test]
    fn test_star_hub_downgrade_and_aux_ic_pwr_priority() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_ic(CompId(1), "U2", 14, 0),
        ];
        // U2:4 nối GND (chân nguồn phụ trên thân IC tại cột 21, rơi vào cột nghỉ hoặc bị chặn hàng 9)
        // *CLK có 4 chân nửa trên + 1 chân nửa dưới (Star-Hub sẽ chiếm 2 cột trống làm Hub),
        // khi chỉ còn 2 cột cầu rãnh trống trên toàn bo thì nửa sau Pass sẽ tự động hạ cấp *CLK sang Steiner để nhường cầu rãnh cho SIG1!
        let txt = "
            VCC U1:14 U2:14; GND U1:7 U2:7 U2:4
            *CLK: U1:14 -> U1:13 -> U1:12 -> U1:11 -> U2:1
            SIG1: U1:10 -> U2:2
        ";
        let parsed = parse_netlist(txt, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        let weights = CostWeights::default();

        let mut layout = BoardLayout::default();
        layout.ics.push(PlacedIc { cid: CompId(0), id: "U1".into(), pins: 14, rot: 0, c_min: 10, c_max: 16, w: 7 });
        layout.ics.push(PlacedIc { cid: CompId(1), id: "U2".into(), pins: 14, rot: 0, c_min: 18, c_max: 24, w: 7 });

        let mut board = BoardState::new(false);
        let mut fps = Vec::new();
        layout.block_space(&mut board, &mut fps);
        let mut errors = Vec::new();
        let ctx = setup_macro_and_decoupling(&mut board, &mut layout, &parsed, &topo, &comps, &weights, &mut errors);

        // Khóa các cột trống khác trên bo, chỉ chừa đúng 2 cột trống (cột 9 và cột 17) làm cầu rãnh E-F
        for c in 1u8..=63 {
            if c < 9 || c > 24 {
                board.col_net[0][c as usize] = 99;
                board.col_net[1][c as usize] = 99;
            }
        }
        // Chặn lỗ thẳng hàng 9 tại cột 21 (chân phụ U2:4) để kiểm tra phân tầng ưu tiên #6
        board.occupy(21, 9);

        let mut col_hist = [0.0f32; 65];
        let mut row_hist = [0.0f32; 10];
        let mut rng = Some(Mulberry32::new(7));
        let res = run_routing_engine(
            8, true, 0, &FxHashSet::default(), &get_persona(2), &mut rng,
            &mut board, &layout, &ctx, &fps, &parsed, &mut col_hist, &mut row_hist, &weights,
        );
        assert!(res.errs.is_empty(), "Star-Hub downgrade should rescue routing to 0 errors: {:?}", res.errs);
    }
}
