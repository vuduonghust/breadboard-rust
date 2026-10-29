use crate::config::personas::get_persona;
use crate::config::weights::CostWeights;
use crate::macro_place::cost::compute_bridge_demand;
use crate::macro_place::topology::TopologyCache;
use crate::model::circuit::{
    BoardLayout, CompSpec, ParsedNetlist, PlacedBtn, PlacedIc, PlacedSw,
};
use crate::model::ids::{CompId, CompType, Mulberry32, NetId, PinRef};
use rustc_hash::FxHashMap;

/// Tùy chọn cấu hình truyền vào bộ đóng gói `place_components` (tương đương `opts` trong JS)
#[derive(Clone, Debug, Default)]
pub struct PlaceOptions {
    pub persona: usize,
    pub sr: Option<u32>,
    pub gap: Option<i32>,
    pub gaps: Option<Vec<i32>>,
    pub compact: bool,
    pub lock_gaps: bool,
    pub rot: FxHashMap<CompId, u8>,
    pub half: FxHashMap<CompId, u8>,
    pub io_flip: FxHashMap<CompId, u8>,
    pub btn_bend: FxHashMap<CompId, i8>,
    pub comp_heat: FxHashMap<CompId, f64>,
}

/// Kết quả đóng gói linh kiện đại cục (IC, SW, BTN) trên 63 cột Breadboard
#[derive(Clone, Debug)]
pub struct PackResult {
    pub layout: BoardLayout,
    pub miss: Vec<CompId>,
    pub gaps: Vec<i32>,
}

#[derive(Clone, Copy, Debug)]
enum PackKind {
    Ic { pins: u8, rot: u8 },
    Sw { row: i8, flip: u8 },
    Btn { comp_type: CompType, row_top: i8, row_bot: i8, flip: u8 },
}

#[derive(Clone, Copy, Debug)]
struct PackCandidate {
    w: usize,
    u: bool,
    l: bool,
    kind: PackKind,
}

/// Tự động chọn góc bẻ nhích chân `bend` (-1, 0, +1) cho BTN2 nếu chưa được chỉ định:
/// - Nửa trên (`row_top == 1`): chọn `bend = -1` (nhích lên hàng A) để giải phóng hoàn toàn làn ngang hàng C (row=2)
/// - Nửa dưới (`row_top == 7`): chọn `bend = +1` (nhích xuống hàng J) để giải phóng hoàn toàn làn ngang hàng H (row=7)
#[inline(always)]
fn resolve_btn_bend(cid: CompId, row_top: i8, btn_bend_map: &FxHashMap<CompId, i8>) -> i8 {
    if let Some(&b) = btn_bend_map.get(&cid) {
        return b.clamp(-1, 1);
    }
    if row_top <= 4 { -1 } else { 1 }
}

/// Bộ đóng gói Linh kiện Đại cục & Phân bổ Khe Cầu Rãnh (`BreadboardPlacer.placeComponents`)
pub fn place_components(
    ordered_comps: &[CompSpec],
    all_comps: &[CompSpec],
    opts: &mut PlaceOptions,
    parsed_topo: Option<(&ParsedNetlist, &TopologyCache)>,
    weights: &CostWeights,
) -> PackResult {
    let items: Vec<CompSpec> = ordered_comps
        .iter()
        .filter(|c| !c.comp_type.is_special())
        .cloned()
        .collect();
    let n = items.len();
    if n == 0 {
        opts.gaps = Some(Vec::new());
        return PackResult {
            layout: BoardLayout::default(),
            miss: Vec::new(),
            gaps: Vec::new(),
        };
    }

    let p_cfg = get_persona(opts.persona);
    let mut rng = opts.sr.map(|s| Mulberry32::new(s.wrapping_add(7)));

    let rot_of = |c: &CompSpec| -> u8 { opts.rot.get(&c.cid).copied().unwrap_or(c.rot) };
    let flip_of = |c: &CompSpec| -> u8 {
        u8::from(opts.io_flip.get(&c.cid).copied().unwrap_or(0) != 0)
    };

    // Tra cứu mạng nguồn (VCC/GND) của chân linh kiện để cho phép SW/BTN2 gối 1 cột nguồn vào mép IC
    let net_of_pin = |cid: CompId, pin: u8| -> Option<NetId> {
        parsed_topo.and_then(|(parsed, _)| parsed.net_of.get(&PinRef { comp: cid, pin }).copied())
    };

    let io_pwr_ends = |c: &CompSpec, fl: u8| -> (Option<NetId>, Option<NetId>) {
        if c.comp_type == CompType::SW {
            let n1 = net_of_pin(c.cid, 1).filter(|n| n.is_power());
            let n3 = net_of_pin(c.cid, 3).filter(|n| n.is_power());
            if fl != 0 { (n3, n1) } else { (n1, n3) }
        } else if c.comp_type == CompType::BTN2 {
            let n1 = net_of_pin(c.cid, 1).filter(|n| n.is_power());
            let n2 = net_of_pin(c.cid, 2).filter(|n| n.is_power());
            if fl != 0 { (n2, n1) } else { (n1, n2) }
        } else {
            (None, None)
        }
    };

    let opts_of = |c: &CompSpec| -> Vec<PackCandidate> {
        match c.comp_type {
            CompType::IC => {
                let w = (c.pins / 2) as usize;
                vec![PackCandidate {
                    w,
                    u: true,
                    l: true,
                    kind: PackKind::Ic { pins: c.pins, rot: rot_of(c) },
                }]
            }
            CompType::SW => {
                let fl = flip_of(c);
                let mut v = vec![
                    PackCandidate { w: 3, u: false, l: true, kind: PackKind::Sw { row: 9, flip: fl } },
                    PackCandidate { w: 3, u: true, l: false, kind: PackKind::Sw { row: 0, flip: fl } },
                ];
                if parsed_topo.is_some() {
                    let alt_fl = 1 - fl;
                    v.push(PackCandidate { w: 3, u: false, l: true, kind: PackKind::Sw { row: 9, flip: alt_fl } });
                    v.push(PackCandidate { w: 3, u: true, l: false, kind: PackKind::Sw { row: 0, flip: alt_fl } });
                }
                v
            }
            _ => {
                let fl = flip_of(c);
                vec![
                    PackCandidate {
                        w: 3,
                        u: true,
                        l: false,
                        kind: PackKind::Btn { comp_type: c.comp_type, row_top: 1, row_bot: 2, flip: fl },
                    },
                    PackCandidate {
                        w: 3,
                        u: false,
                        l: true,
                        kind: PackKind::Btn { comp_type: c.comp_type, row_top: 7, row_bot: 8, flip: fl },
                    },
                ]
            }
        }
    };

    // BẮT BUỘC VẬT LÝ: Giữa 2 IC luôn phải cách nhau ít nhất 1 cột trống (min_gap_for >= 1)
    let min_gap_for = |idx: usize| -> i32 {
        if idx + 1 >= n {
            0
        } else if items[idx].comp_type == CompType::IC {
            1
        } else {
            0
        }
    };

    let coloc_weight = weights.packer_coloc_bonus as i32;

    let mut try_pack = |gap_arr: &[i32]| -> (BoardLayout, Vec<CompId>) {
        let mut u_occ = [false; 70];
        let mut l_occ = [false; 70];
        let mut u_comp = [false; 70];
        let mut l_comp = [false; 70];
        // Lưu mạng nguồn (VCC/GND) tại các cột mép của IC/SW để cho phép gối chung 1 cột nguồn
        let mut u_edge_pwr: [Option<NetId>; 70] = [None; 70];
        let mut l_edge_pwr: [Option<NetId>; 70] = [None; 70];
        let mut miss = Vec::new();
        let mut out = BoardLayout::default();

        for (idx, c) in items.iter().enumerate() {
            let mut os = opts_of(c);
            let pref = opts
                .half
                .get(&c.cid)
                .copied()
                .or_else(|| (c.comp_type == CompType::SW).then_some(1));

            if pref.is_none() {
                if let Some(r) = rng.as_mut() {
                    if os.len() >= 2 && r.next_f64() < 0.5 {
                        os.swap(0, 1);
                    }
                }
            }

            let is_pref = |o: &PackCandidate, p: u8| -> bool {
                o.u != o.l && if p != 0 { o.l } else { o.u }
            };

            let groups: Vec<Vec<PackCandidate>> = match pref {
                None => vec![os],
                Some(p) => {
                    let g1: Vec<PackCandidate> = os.iter().copied().filter(|o| is_pref(o, p)).collect();
                    let g2: Vec<PackCandidate> = os.iter().copied().filter(|o| !is_pref(o, p)).collect();
                    vec![g1, g2]
                }
            };

            let mut best: Option<(usize, PackCandidate, i32, usize)> = None;
            for gs in &groups {
                for &o in gs {
                    if o.w == 0 || o.w > 63 {
                        continue;
                    }
                    let (u_left_p, u_right_p, l_left_p, l_right_p, is_alt_flip) = match o.kind {
                        PackKind::Sw { row, flip } => {
                            let (lp, rp) = io_pwr_ends(c, flip);
                            let alt = flip != flip_of(c);
                            if row == 0 {
                                (lp, rp, None, None, alt)
                            } else {
                                (None, None, lp, rp, alt)
                            }
                        }
                        PackKind::Ic { pins, rot } => {
                            let (pv, pg) = c.get_ic_pwr_pins();
                            let mut ul = None;
                            let mut ur = None;
                            let mut ll = None;
                            let mut lr = None;
                            let h_w = pins / 2;
                            for p_opt in [pv, pg] {
                                if let Some(p) = p_opt {
                                    let p_net = net_of_pin(c.cid, p).filter(|n| n.is_power());
                                    let (off, pr) = if rot == 0 {
                                        if p <= h_w { (p - 1, 5i8) } else { (pins - p, 4i8) }
                                    } else if p <= h_w {
                                        (h_w - p, 4i8)
                                    } else {
                                        (p - h_w - 1, 5i8)
                                    };
                                    if off == 0 {
                                        if pr == 4 { ul = p_net; } else { ll = p_net; }
                                    } else if (off as usize) + 1 == o.w {
                                        if pr == 4 { ur = p_net; } else { lr = p_net; }
                                    }
                                }
                            }
                            (ul, ur, ll, lr, false)
                        }
                        _ => (None, None, None, None, false),
                    };

                    let is_sw = matches!(o.kind, PackKind::Sw { .. });

                    for s in 1..=(63 - o.w + 1) {
                        let mut ok = true;
                        let mut pwr_overlap_bonus = 0i32;
                        for x in s..(s + o.w) {
                            let is_left = x == s;
                            let is_right = x + 1 == s + o.w;
                            // Với SW: kiểm tra thân thực tế u_comp/l_comp để không bị khoảng trống g của IC chặn oan
                            let blocked_u = if is_sw { u_comp[x] } else { u_occ[x] };
                            let blocked_l = if is_sw { l_comp[x] } else { l_occ[x] };

                            if o.u && blocked_u {
                                let share_u = (is_left && u_left_p.is_some() && u_left_p == u_edge_pwr[x])
                                    || (is_right && u_right_p.is_some() && u_right_p == u_edge_pwr[x]);
                                if share_u && !(o.l && l_comp[x]) {
                                    pwr_overlap_bonus += 26;
                                } else {
                                    ok = false;
                                    break;
                                }
                            }
                            if o.l && blocked_l {
                                let share_l = (is_left && l_left_p.is_some() && l_left_p == l_edge_pwr[x])
                                    || (is_right && l_right_p.is_some() && l_right_p == l_edge_pwr[x]);
                                if share_l && !(o.u && u_comp[x]) {
                                    pwr_overlap_bonus += 26;
                                } else {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if !ok || (is_alt_flip && pwr_overlap_bonus == 0) {
                            continue;
                        }
                        let mut co_loc = 0usize;
                        if o.u != o.l {
                            for x in s..(s + o.w) {
                                if (o.u && l_comp[x] && !u_comp[x]) || (o.l && u_comp[x] && !l_comp[x]) {
                                    co_loc += 1;
                                }
                            }
                        }
                        let half_cnt = (i32::from(o.u) + i32::from(o.l)) * 4;
                        let rank = (s as i32) - (co_loc as i32) * coloc_weight - pwr_overlap_bonus + half_cnt;
                        if best.is_none_or(|b| rank < b.2) {
                            best = Some((s, o, rank, co_loc));
                        }
                    }
                }
                if best.is_some() {
                    break;
                }
            }

            let Some((best_s, best_o, _, best_coloc)) = best else {
                miss.push(c.cid);
                continue;
            };

            let c_min = best_s as u8;
            let c_max = (best_s + best_o.w - 1) as u8;
            match best_o.kind {
                PackKind::Ic { pins, rot } => {
                    out.ics.push(PlacedIc {
                        cid: c.cid,
                        id: c.id.clone(),
                        pins,
                        rot,
                        c_min,
                        c_max,
                        w: best_o.w as u8,
                    });
                    let (pv, pg) = c.get_ic_pwr_pins();
                    for p_opt in [pv, pg] {
                        if let Some(p) = p_opt {
                            let h_w = pins / 2;
                            let (pc, pr) = if rot == 0 {
                                if p <= h_w { (c_min + p - 1, 5i8) } else { (c_min + (pins - p), 4i8) }
                            } else if p <= h_w {
                                (c_max - p + 1, 4i8)
                            } else {
                                (c_min + p - h_w - 1, 5i8)
                            };
                            if pc == c_min || pc == c_max {
                                let p_net = net_of_pin(c.cid, p).filter(|n| n.is_power());
                                if pr == 4 {
                                    u_edge_pwr[pc as usize] = p_net;
                                } else {
                                    l_edge_pwr[pc as usize] = p_net;
                                }
                            }
                        }
                    }
                }
                PackKind::Sw { row, flip } => {
                    out.sws.push(PlacedSw {
                        cid: c.cid,
                        id: c.id.clone(),
                        c_min,
                        c_max,
                        row,
                        flip,
                    });
                    
                    // NGĂN SW GỐI CỘT NGUỒN VỚI NHAU:
                    // Chúng ta đánh dấu None cho cả 2 mép, để không có SW nào đặt sau
                    // tưởng lầm là còn chỗ và đè lên SW hiện tại.
                    if row == 0 {
                        u_edge_pwr[c_min as usize] = None;
                        u_edge_pwr[c_max as usize] = None;
                    } else {
                        l_edge_pwr[c_min as usize] = None;
                        l_edge_pwr[c_max as usize] = None;
                    }
                }
                PackKind::Btn { comp_type, row_top, row_bot, flip } => {
                    let bend = resolve_btn_bend(c.cid, row_top, &opts.btn_bend);
                    out.btns.push(PlacedBtn {
                        cid: c.cid,
                        id: c.id.clone(),
                        comp_type,
                        c_min,
                        c_max,
                        row_top,
                        row_bot,
                        flip,
                        bend,
                    });
                }
            }

            let raw_g = gap_arr.get(idx).copied().unwrap_or(1);
            let g = min_gap_for(idx).max(raw_g) as usize;
            for x in best_s..(best_s + best_o.w) {
                if best_o.u {
                    u_comp[x] = true;
                }
                if best_o.l {
                    l_comp[x] = true;
                }
            }
            let occ_end = 68.min(best_s + best_o.w + g);
            for x in best_s..occ_end {
                if best_o.u {
                    u_occ[x] = true;
                }
                if best_o.l {
                    l_occ[x] = true;
                }
            }
            if best_coloc == best_o.w {
                let sync_end = 68.min(best_s + best_o.w + g.max(1));
                for x in best_s..sync_end {
                    if !u_comp[x] && !l_comp[x] {
                        u_occ[x] = true;
                        l_occ[x] = true;
                    }
                }
            }
        }
        (out, miss)
    };

    let base_gap = opts.gap.map_or(2, |g| g.max(1));
    let br_demand = if let Some((parsed, topo)) = parsed_topo {
        compute_bridge_demand(&items, parsed, topo, &opts.rot, &opts.half, all_comps, weights)
    } else {
        FxHashMap::default()
    };

    let mut gap_arr: Vec<i32> = if let Some(existing) = opts.gaps.as_ref().filter(|g| g.len() == n)
    {
        existing
            .iter()
            .enumerate()
            .map(|(idx, &g)| min_gap_for(idx).max(g))
            .collect()
    } else {
        items
            .iter()
            .enumerate()
            .map(|(idx, c)| {
                if c.comp_type == CompType::IC {
                    base_gap.max(1)
                } else if p_cfg.pack_io {
                    0
                } else {
                    i32::from(idx % 2 == 1)
                }
            })
            .collect()
    };

    let mut best = try_pack(&gap_arr);
    let mut guard = 110i32;
    while !best.1.is_empty() && guard > 0 {
        guard -= 1;
        let mut max_score = -1e9_f64;
        let mut max_idx = None;
        for i in 0..n {
            let floor_g = min_gap_for(i);
            if gap_arr[i] <= floor_g {
                continue;
            }
            let is_ic = items[i].comp_type == CompType::IC;
            let d_br = br_demand.get(&items[i].cid).copied().unwrap_or(0.0);
            let sc = (gap_arr[i] as f64) + (if !is_ic { 4.0 } else { 0.0 }) - d_br * 0.18;
            if sc > max_score {
                max_score = sc;
                max_idx = Some(i);
            }
        }
        let Some(idx) = max_idx else { break };
        gap_arr[idx] -= 1;
        let r = try_pack(&gap_arr);
        if r.1.len() <= best.1.len() {
            best = r;
        }
    }

    if best.1.is_empty() && !opts.compact && !opts.lock_gaps && n > 1 {
        let ic_indices: Vec<usize> = (0..(n - 1))
            .filter(|&i| items[i].comp_type == CompType::IC)
            .collect();
        let mid = if !ic_indices.is_empty() {
            ic_indices[ic_indices.len() / 2] as f64
        } else {
            ((n - 1) as f64) / 2.0
        };

        let mut center_order: Vec<usize> = (0..(n - 1))
            .filter(|&i| !p_cfg.pack_io || items[i].comp_type == CompType::IC)
            .collect();
        center_order.sort_by(|&a, &b| {
            let (id_a, id_b) = (items[a].cid, items[b].cid);
            let ba = br_demand.get(&id_a).copied().unwrap_or(0.0) * 0.85
                + if p_cfg.hot_gap {
                    opts.comp_heat.get(&id_a).copied().unwrap_or(0.0) * 0.25
                } else {
                    0.0
                };
            let bb = br_demand.get(&id_b).copied().unwrap_or(0.0) * 0.85
                + if p_cfg.hot_gap {
                    opts.comp_heat.get(&id_b).copied().unwrap_or(0.0) * 0.25
                } else {
                    0.0
                };
            let sa = ((a as f64) - mid).abs()
                - (if items[a].comp_type == CompType::IC { 2.5 } else { 0.0 })
                - ba;
            let sb = ((b as f64) - mid).abs()
                - (if items[b].comp_type == CompType::IC { 2.5 } else { 0.0 })
                - bb;
            sa.total_cmp(&sb)
        });

        for pass in 0..3 {
            for &i in &center_order {
                let is_ic = items[i].comp_type == CompType::IC;
                let target_max = match pass {
                    0 => if is_ic { 2 } else { 1 },
                    1 => if is_ic { 3 } else { 1 },
                    _ => if is_ic { 4 } else { 1 },
                };
                if gap_arr[i] >= target_max {
                    continue;
                }
                gap_arr[i] += 1;
                let r = try_pack(&gap_arr);
                if r.1.is_empty() {
                    best = r;
                } else {
                    gap_arr[i] -= 1;
                }
            }
        }
    }

    for btn in &best.0.btns {
        opts.btn_bend.entry(btn.cid).or_insert(btn.bend);
    }
    opts.gaps = Some(gap_arr.clone());
    PackResult {
        layout: best.0,
        miss: best.1,
        gaps: gap_arr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macro_place::topology::build_topology_cache;
    use crate::model::bitboard::{track_index, BoardState};
    use crate::parser::parse_netlist;

    #[test]
    fn test_breadboard_placer_ic_gap_and_coloc() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_ic(CompId(1), "U2", 14, 0),
            CompSpec::new_other(CompId(2), "B1", CompType::BTN2),
            CompSpec::new_other(CompId(3), "S1", CompType::SW),
        ];
        let txt = "VCC U1:14 U2:14; GND U1:7 U2:7; U1:1 -> U2:2; B1:1 -> U1:3; S1:2 -> U2:4";
        let parsed = parse_netlist(txt, &comps);
        let topo = build_topology_cache(&parsed, &comps);
        let weights = CostWeights::default();

        let mut opts = PlaceOptions {
            persona: 2,
            ..Default::default()
        };
        opts.half.insert(CompId(2), 0);
        opts.half.insert(CompId(3), 1);

        let res = place_components(
            &comps,
            &comps,
            &mut opts,
            Some((&parsed, &topo)),
            &weights,
        );
        assert!(res.miss.is_empty());
        assert_eq!(res.layout.ics.len(), 2);
        assert!(res.layout.ics[1].c_min > res.layout.ics[0].c_max + 1);
        assert_eq!(res.layout.btns[0].c_min, res.layout.sws[0].c_min);
        assert_eq!(res.layout.btns[0].bend, -1);
        assert_eq!(opts.btn_bend.get(&CompId(2)), Some(&-1));
    }

    #[test]
    fn test_packer_auto_and_custom_btn_bend() {
        let comps = vec![
            CompSpec::new_other(CompId(0), "B1", CompType::BTN2),
            CompSpec::new_other(CompId(1), "B2", CompType::BTN2),
        ];
        let weights = CostWeights::default();
        let mut opts = PlaceOptions::default();
        opts.half.insert(CompId(0), 0);
        opts.half.insert(CompId(1), 1);

        let res = place_components(&comps, &comps, &mut opts, None, &weights);
        assert_eq!(res.layout.btns[0].bend, -1);
        assert_eq!(res.layout.btns[1].bend, 1);

        let (mut board, mut fps) = (BoardState::new(false), Vec::new());
        res.layout.block_space(&mut board, &mut fps);
        let b1 = &res.layout.btns[0];
        let b2 = &res.layout.btns[1];
        assert!(board.is_segment_free(track_index(2, 0).unwrap(), b1.c_min, b1.c_max));
        assert!(board.is_segment_free(track_index(7, 0).unwrap(), b2.c_min, b2.c_max));

        let mut custom_opts = PlaceOptions::default();
        custom_opts.half.insert(CompId(0), 0);
        custom_opts.btn_bend.insert(CompId(0), 1);
        let res_custom = place_components(&comps[..1], &comps[..1], &mut custom_opts, None, &weights);
        assert_eq!(res_custom.layout.btns[0].bend, 1);
    }
}
