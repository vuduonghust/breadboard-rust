use crate::config::weights::CostWeights;
use crate::discrete_place::pin_swap::optimize_symmetric_pins;
use crate::macro_place::topology::TopologyCache;
use crate::model::bitboard::{half_of, is_rail_row, BoardState};
use crate::model::circuit::{BoardLayout, CompSpec, ParsedNetlist, PlacedSpecial};
use crate::model::ids::{CompId, CompType, NetId, PinRef};
use rustc_hash::{FxHashMap, FxHashSet};

#[inline(always)]
pub const fn pwr_aux_tag(net: NetId, half: u8) -> u16 {
    0x4000 | (((net.0 == 2) as u16) << 8) | ((half & 1) as u16)
}

#[inline(always)]
pub const fn pwr_tied_tag(net: NetId, cid: CompId, half: u8) -> u16 {
    0x5000 | (((net.0 == 2) as u16) << 9) | (((half & 1) as u16) << 8) | (cid.0 as u16)
}

#[inline(always)]
pub const fn pwr_direct_tag(net: NetId, col: u8, half: u8) -> u16 {
    0x6000 | (((net.0 == 2) as u16) << 9) | (((half & 1) as u16) << 8) | (col as u16)
}

#[inline(always)]
pub const fn pwr_main_tag(net: NetId, cid: CompId, pin: u8) -> u16 {
    0x8000 | (((net.0 == 2) as u16) << 13) | (((cid.0 & 0x7F) as u16) << 6) | ((pin & 0x3F) as u16)
}

#[inline(always)]
pub const fn nc_tag(cid: CompId, pin: u8) -> u16 {
    0xC000 | ((cid.0 as u16) << 6) | ((pin & 0x3F) as u16)
}

#[inline(always)]
pub const fn is_pwr_tag(tag: u16) -> bool {
    tag == 1 || tag == 2 || (tag >= 0x4000 && tag < 0xC000)
}

#[inline(always)]
pub const fn is_pwr_tied_tag(tag: u16) -> bool {
    tag >= 0x5000 && tag < 0x6000
}

#[inline(always)]
pub const fn base_net_of(tag: u16) -> NetId {
    if tag == 1 || tag == 2 {
        NetId(tag)
    } else if tag >= 0x4000 && tag < 0x5000 {
        if (tag & 0x0100) != 0 { NetId::GND } else { NetId::VCC }
    } else if tag >= 0x5000 && tag < 0x8000 {
        if (tag & 0x0200) != 0 { NetId::GND } else { NetId::VCC }
    } else if tag >= 0x8000 && tag < 0xC000 {
        if (tag & 0x2000) != 0 { NetId::GND } else { NetId::VCC }
    } else if tag >= 3 && tag < 0x4000 {
        NetId(tag)
    } else {
        NetId::NONE
    }
}

#[inline]
pub fn pwr_tag(
    net: NetId,
    cid: CompId,
    pin: u8,
    row: i8,
    comp_type: CompType,
    main_ic_pwr_pins: &FxHashSet<PinRef>,
) -> u16 {
    if !net.is_power() {
        return net.0;
    }
    if comp_type == CompType::IC {
        if main_ic_pwr_pins.contains(&PinRef { comp: cid, pin }) {
            return pwr_main_tag(net, cid, pin);
        }
        return pwr_tied_tag(net, cid, half_of(row));
    }
    pwr_aux_tag(net, half_of(row))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HolePin {
    pub col: u8,
    pub row: i8,
    pub cid: CompId,
    pub pin: u8,
    pub tag: u16,
}

#[derive(Clone, Debug)]
pub struct DiscreteContext {
    pub net_of: FxHashMap<PinRef, NetId>,
    pub col_own: [[Option<CompId>; 64]; 2],
    pub pres: FxHashMap<NetId, [u64; 2]>,
    pub holes: FxHashMap<NetId, Vec<HolePin>>,
    pub main_ic_pwr_pins: FxHashSet<PinRef>,
    pub invalid_comp_ids: FxHashSet<CompId>,
    pub btn_bridges: Vec<((u8, u8), (u8, u8))>,
    pub locked_ic_caps: Vec<PlacedSpecial>,
    pub locked_decoupling_ids: FxHashSet<CompId>,
}

impl Default for DiscreteContext {
    fn default() -> Self {
        Self {
            net_of: FxHashMap::default(),
            col_own: [[None; 64]; 2],
            pres: FxHashMap::default(),
            holes: FxHashMap::default(),
            main_ic_pwr_pins: FxHashSet::default(),
            invalid_comp_ids: FxHashSet::default(),
            btn_bridges: Vec::new(),
            locked_ic_caps: Vec::new(),
            locked_decoupling_ids: FxHashSet::default(),
        }
    }
}

impl DiscreteContext {
    pub fn add_pin(
        &mut self,
        board: &mut BoardState,
        cid: CompId,
        pin: u8,
        col: u8,
        row: i8,
        comps: &[CompSpec],
        parsed: &ParsedNetlist,
        errors: &mut Vec<String>,
    ) {
        let Some(&net) = self.net_of.get(&PinRef { comp: cid, pin }) else { return };
        if is_rail_row(row) {
            return;
        }
        let h = half_of(row) as usize;
        let cu = col as usize;
        let c_obj = comps.iter().find(|c| c.cid == cid);
        let c_type = c_obj.map_or(CompType::R, |c| c.comp_type);
        let tag = pwr_tag(net, cid, pin, row, c_type, &self.main_ic_pwr_pins);

        let cur_tag = board.col_net[h][cu];
        if cur_tag != 0 && cur_tag != tag {
            let prev_base = base_net_of(cur_tag);
            // Cho phép Tụ điện, Điện trở nguồn hoặc chân nguồn của SW/BTN2 dùng chung cột nguồn cùng mạng (VCC chung với VCC, GND chung với GND) với chân IC
            if !(net.is_power() && prev_base == net) {
                let comp_name = c_obj.map_or("?", |c| c.id.as_str());
                let net_name = parsed.nets.iter().find(|n| n.id == net).map_or("?", |n| n.name.as_str());
                let owner_name = self.col_own[h][cu]
                    .and_then(|oc| comps.iter().find(|x| x.cid == oc))
                    .map_or("nguồn", |x| x.id.as_str());
                errors.push(format!(
                    "{comp_name}:{pin} (net {net_name}) chung cột {col} với {owner_name} -> ngắn mạch hoặc vi phạm trung chuyển nguồn"
                ));
            }
        }
        if board.col_net[h][cu] == 0 {
            board.col_net[h][cu] = tag;
        }
        if self.col_own[h][cu].is_none() {
            self.col_own[h][cu] = Some(cid);
        }
        if !net.is_power() {
            self.pres.entry(net).or_insert([0u64; 2])[h] |= 1u64 << col;
        }
        let final_tag = board.col_net[h][cu];
        self.holes.entry(net).or_default().push(HolePin { col, row, cid, pin, tag: final_tag });
    }
}

fn split_id_num(s: &str) -> (&str, u32) {
    let idx = s.find(|c: char| c.is_ascii_digit()).unwrap_or(s.len());
    (&s[..idx], s[idx..].parse::<u32>().unwrap_or(0))
}

fn natural_id_cmp(a: &str, b: &str) -> core::cmp::Ordering {
    split_id_num(a).cmp(&split_id_num(b))
}

#[allow(clippy::too_many_arguments)]
fn lock_cap_across_ic(
    cap: &CompSpec,
    ic_cid: CompId,
    board: &mut BoardState,
    layout: &mut BoardLayout,
    ctx: &mut DiscreteContext,
    decoupled_ic_set: &mut FxHashSet<CompId>,
    comps: &[CompSpec],
    parsed: &ParsedNetlist,
    errors: &mut Vec<String>,
) {
    let Some(ic_comp) = comps.iter().find(|x| x.cid == ic_cid) else { return };
    let (Some(p_v), Some(p_g)) = ic_comp.get_ic_pwr_pins() else { return };
    let (Some(h_v), Some(h_g)) = (layout.pin_hole(ic_cid, p_v), layout.pin_hole(ic_cid, p_g)) else { return };
    let r_adj_v = if h_v.1 == 4 { 3 } else { 6 };
    let r_adj_g = if h_g.1 == 4 { 3 } else { 6 };
    if board.is_occupied(h_v.0, r_adj_v) || board.is_occupied(h_g.0, r_adj_g) {
        return;
    }
    let n1 = ctx.net_of.get(&PinRef { comp: cap.cid, pin: 1 }).copied();
    let n2 = ctx.net_of.get(&PinRef { comp: cap.cid, pin: 2 }).copied();
    let rev_pins = n1 == Some(NetId::GND) && n2 == Some(NetId::VCC);
    let (c1, r1) = if rev_pins { (h_g.0, r_adj_g) } else { (h_v.0, r_adj_v) };
    let (c2, r2) = if rev_pins { (h_v.0, r_adj_v) } else { (h_g.0, r_adj_g) };

    board.occupy(c1, r1);
    board.occupy(c2, r2);
    ctx.net_of.entry(PinRef { comp: cap.cid, pin: 1 }).or_insert(if rev_pins { NetId::GND } else { NetId::VCC });
    ctx.net_of.entry(PinRef { comp: cap.cid, pin: 2 }).or_insert(if rev_pins { NetId::VCC } else { NetId::GND });

    let pr_v = PinRef { comp: ic_cid, pin: p_v };
    if let std::collections::hash_map::Entry::Vacant(e) = ctx.net_of.entry(pr_v) {
        e.insert(NetId::VCC);
        board.col_net[half_of(h_v.1) as usize][h_v.0 as usize] = 0;
        ctx.add_pin(board, ic_cid, p_v, h_v.0, h_v.1, comps, parsed, errors);
    }
    let pr_g = PinRef { comp: ic_cid, pin: p_g };
    if let std::collections::hash_map::Entry::Vacant(e) = ctx.net_of.entry(pr_g) {
        e.insert(NetId::GND);
        board.col_net[half_of(h_g.1) as usize][h_g.0 as usize] = 0;
        ctx.add_pin(board, ic_cid, p_g, h_g.0, h_g.1, comps, parsed, errors);
    }

    let locked_obj = PlacedSpecial {
        cid: cap.cid,
        id: cap.id.clone(),
        comp_type: cap.comp_type,
        c1,
        r1,
        c2,
        r2,
        locked: true,
        ic_id: Some(ic_cid),
    };
    ctx.locked_ic_caps.push(locked_obj.clone());
    layout.cs.push(locked_obj);
    ctx.locked_decoupling_ids.insert(cap.cid);
    decoupled_ic_set.insert(ic_cid);
}

/// Thiết lập chân linh kiện Đại cục, đảo chân đối xứng BTN2/SW và Khóa cứng Tụ lọc nguồn IC (Hàng 3 & 6)
pub fn setup_macro_and_decoupling(
    board: &mut BoardState,
    layout: &mut BoardLayout,
    parsed: &ParsedNetlist,
    topo: &TopologyCache,
    comps: &[CompSpec],
    weights: &CostWeights,
    errors: &mut Vec<String>,
) -> DiscreteContext {
    let mut ctx = DiscreteContext {
        net_of: parsed.net_of.clone(),
        ..Default::default()
    };

    for err_msg in &parsed.errors {
        for c in comps.iter().filter(|c| c.comp_type.is_special()) {
            if err_msg.starts_with(&format!("{}:", c.id))
                || err_msg.starts_with(&format!("LED {} ", c.id))
                || err_msg.starts_with(&format!("Tụ phân cực {} ", c.id))
            {
                ctx.invalid_comp_ids.insert(c.cid);
            }
        }
    }

    let placed_ids: FxHashSet<CompId> = layout
        .ics.iter().map(|x| x.cid)
        .chain(layout.sws.iter().map(|x| x.cid))
        .chain(layout.btns.iter().map(|x| x.cid))
        .collect();

    for c in comps.iter().filter(|c| c.comp_type == CompType::IC) {
        let (p_v, p_g) = c.get_ic_pwr_pins();
        if c.custom_pwr {
            if p_v.is_none() || p_g.is_none() {
                errors.push(format!("{}: chân VCC/GND tùy chỉnh phải nằm trong khoảng 1..{}", c.id, c.pins));
            } else if p_v == p_g {
                errors.push(format!("{}: chân VCC và GND không được trùng nhau (chân {})", c.id, p_v.unwrap()));
            }
        }
        if let Some(pv) = p_v {
            let pr = PinRef { comp: c.cid, pin: pv };
            ctx.main_ic_pwr_pins.insert(pr);
            if let Some(&nv) = ctx.net_of.get(&pr) {
                if nv != NetId::VCC {
                    errors.push(format!("{}:{pv} được cấu hình là chân VCC nhưng trong Netlist lại nối vào mạng khác", c.id));
                }
            }
        }
        if let Some(pg) = p_g {
            let pr = PinRef { comp: c.cid, pin: pg };
            ctx.main_ic_pwr_pins.insert(pr);
            if let Some(&ng) = ctx.net_of.get(&pr) {
                if ng != NetId::GND {
                    errors.push(format!("{}:{pg} được cấu hình là chân GND nhưng trong Netlist lại nối vào mạng khác", c.id));
                }
            }
        }
    }

    optimize_symmetric_pins(layout, &ctx.net_of, parsed, topo, weights);

    for net in &parsed.nets {
        for &pr in &net.pins {
            if comps.iter().any(|c| c.cid == pr.comp && !c.comp_type.is_special()) {
                if let Some((col, row)) = layout.pin_hole(pr.comp, pr.pin) {
                    ctx.add_pin(board, pr.comp, pr.pin, col, row, comps, parsed, errors);
                }
            }
        }
    }

    for c in comps.iter().filter(|c| !c.comp_type.is_special() && placed_ids.contains(&c.cid)) {
        for p in 1..=c.pins {
            if ctx.net_of.contains_key(&PinRef { comp: c.cid, pin: p }) {
                continue;
            }
            if let Some((col, row)) = layout.pin_hole(c.cid, p) {
                let (h, cu) = (half_of(row) as usize, col as usize);
                if board.col_net[h][cu] == 0 {
                    board.col_net[h][cu] = nc_tag(c.cid, p);
                    ctx.col_own[h][cu] = Some(c.cid);
                }
            }
        }
    }

    if !layout.ics.is_empty() {
        let mut ic_by_natural = layout.ics.clone();
        ic_by_natural.sort_by(|a, b| natural_id_cmp(&a.id, &b.id));
        let mut decoupled_ic_set: FxHashSet<CompId> = FxHashSet::default();

        let mut cand_caps: Vec<&CompSpec> = comps
            .iter()
            .filter(|c| {
                if !matches!(c.comp_type, CompType::CC | CompType::C) || ctx.invalid_comp_ids.contains(&c.cid) {
                    return false;
                }
                let n1 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 1 }).copied();
                let n2 = ctx.net_of.get(&PinRef { comp: c.cid, pin: 2 }).copied();
                let is_vcc_gnd = matches!((n1, n2), (Some(NetId::VCC), Some(NetId::GND)) | (Some(NetId::GND), Some(NetId::VCC)));
                let is_unassigned_cc = c.comp_type == CompType::CC && n1.is_none() && n2.is_none();
                is_vcc_gnd || is_unassigned_cc
            })
            .collect();
        cand_caps.sort_by_key(|c| u8::from(c.comp_type != CompType::CC));

        for &cap in &cand_caps {
            if let Some(&target_ic) = parsed.cap_ic_hint.get(&cap.cid) {
                if !decoupled_ic_set.contains(&target_ic) && ic_by_natural.iter().any(|x| x.cid == target_ic) {
                    lock_cap_across_ic(cap, target_ic, board, layout, &mut ctx, &mut decoupled_ic_set, comps, parsed, errors);
                }
            }
        }
        for &cap in &cand_caps {
            if ctx.locked_decoupling_ids.contains(&cap.cid) {
                continue;
            }
            if let Some(ic_obj) = ic_by_natural.iter().find(|x| !decoupled_ic_set.contains(&x.cid)) {
                lock_cap_across_ic(cap, ic_obj.cid, board, layout, &mut ctx, &mut decoupled_ic_set, comps, parsed, errors);
            }
        }
    }
    ctx
}
