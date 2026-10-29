use crate::model::circuit::{BoardLayout, CompSpec};
use crate::model::ids::{CompId, CompType};
use crate::routing::h_route::FastWire;
use rustc_hash::FxHashMap;

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x00000100000001b3;

#[inline(always)]
const fn mix_u64(mut h: u64, val: u64) -> u64 {
    h ^= val;
    h = h.wrapping_mul(FNV_PRIME);
    h ^ (h >> 32)
}

/// Mã hóa góc bẻ chân `BTN2` (`bend: -1, 0, +1`) thành `u8` (`0, 1, 2`)
#[inline(always)]
pub const fn encode_btn_bend(bend: i8) -> u8 {
    match bend {
        -1 => 0,
        0 => 1,
        1 => 2,
        _ => 1,
    }
}

/// Giải mã `u8` (`0, 1, 2`) về góc bẻ chân `BTN2` (`bend: -1, 0, +1`)
#[inline(always)]
pub const fn decode_btn_bend(code: u8) -> i8 {
    match code {
        0 => -1,
        1 => 0,
        2 => 1,
        _ => 0,
    }
}

/// Băm 64-bit bố cục Đại cục IC/SW/BTN (`layoutSig` trong main.html - bao gồm cả `b.bend` của BTN2)
pub fn layout_sig(layout: &BoardLayout) -> u64 {
    let mut h = FNV_OFFSET;
    for ic in &layout.ics {
        let v = ((ic.cid.0 as u64) << 24)
            | ((ic.rot as u64) << 16)
            | ((ic.c_min as u64) << 8)
            | (ic.c_max as u64);
        h = mix_u64(h, v);
    }
    for sw in &layout.sws {
        let v = ((sw.cid.0 as u64) << 24)
            | ((sw.flip as u64) << 16)
            | (((sw.row as u8) as u64) << 8)
            | (sw.c_min as u64);
        h = mix_u64(h, v);
    }
    for b in &layout.btns {
        let bend_bits = (encode_btn_bend(b.bend) & 0x3) as u64;
        let v = ((b.cid.0 as u64) << 24)
            | (bend_bits << 18)
            | ((b.flip as u64) << 16)
            | (((b.row_top as u8) as u64) << 8)
            | (b.c_min as u64);
        h = mix_u64(h, v);
    }
    h
}

/// Băm 64-bit toàn bộ phương án (`solSig` trong main.html) để lọc trùng O(1)
pub fn sol_sig(layout: &BoardLayout, wires: &[FastWire], err_count: usize) -> u64 {
    let mut h = layout_sig(layout);
    for sp in layout.rs.iter().chain(&layout.leds).chain(&layout.cs) {
        let v = ((sp.cid.0 as u64) << 32)
            | ((sp.c1 as u64) << 24)
            | (((sp.r1 as u8) as u64) << 16)
            | ((sp.c2 as u64) << 8)
            | ((sp.r2 as u8) as u64);
        h = mix_u64(h, v);
    }
    for w in wires {
        let v = ((w.net.0 as u64) << 40)
            | ((w.c1 as u64) << 32)
            | (((w.r1 as u8) as u64) << 24)
            | ((w.c2 as u64) << 16)
            | (((w.r2 as u8) as u64) << 8)
            | ((w.lane as u8) as u64);
        h = mix_u64(h, v);
    }
    mix_u64(h, err_count as u64)
}

/// Khóa định danh vùng trũng Đại cục (`TabuBasin.keyOf` - mở rộng cho cả SW và BTN2 `is_io()` + `btn_bend`)
pub fn tabu_key(
    order: &[usize],
    rot: &FxHashMap<CompId, u8>,
    half: &FxHashMap<CompId, u8>,
    comps: &[CompSpec],
) -> u64 {
    if order.is_empty() {
        return 0;
    }
    let mut h = FNV_OFFSET;
    for &ci in order {
        if let Some(c) = comps.get(ci) {
            if !c.comp_type.is_special() {
                h = mix_u64(h, (c.cid.0 as u64) + 1);
            }
        }
    }
    for c in comps {
        if c.comp_type == CompType::IC {
            let r = u64::from(rot.get(&c.cid).copied().unwrap_or(0) != 0);
            h = mix_u64(h, ((c.cid.0 as u64) << 8) | r);
        } else if c.comp_type.is_io() {
            let def_half = u8::from(c.comp_type == CompType::SW);
            let hf = u64::from(half.get(&c.cid).copied().unwrap_or(def_half) != 0);
            let bend_bits = if c.comp_type == CompType::BTN2 {
                let def_bend = if hf == 0 { 0u8 } else { 2u8 };
                (rot.get(&c.cid).copied().unwrap_or(def_bend) & 0x3) as u64
            } else {
                0u64
            };
            h = mix_u64(h, ((c.cid.0 as u64) << 8) | (hf << 1) | (bend_bits << 2));
        }
    }
    h
}

/// Khóa định danh bàn giao ca mổ Vi phẫu (`macroSig` của `handoverSeen` trong main.html)
pub fn macro_handover_sig(
    order: &[usize],
    rot: &FxHashMap<CompId, u8>,
    half: &FxHashMap<CompId, u8>,
    gaps: &[i32],
    err_count: usize,
    comps: &[CompSpec],
) -> u64 {
    let mut h = tabu_key(order, rot, half, comps);
    for &g in gaps {
        h = mix_u64(h, (g as u64).wrapping_add(100));
    }
    mix_u64(h, err_count as u64)
}
