use crate::model::bitboard::{track_index, BoardState};
use crate::model::geometry::Footprint;
use crate::model::ids::{CompId, CompType, NetId, PinRef};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

/// Thông số khai báo của 1 linh kiện trong danh sách `components` (main.html)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompSpec {
    pub cid: CompId,
    pub id: String,
    pub comp_type: CompType,
    pub pins: u8,
    pub rot: u8,
    pub custom_pwr: bool,
    pub vcc_pin: Option<u8>,
    pub gnd_pin: Option<u8>,
}

impl CompSpec {
    pub fn new_ic(cid: CompId, id: impl Into<String>, pins: u8, rot: u8) -> Self {
        Self {
            cid,
            id: id.into(),
            comp_type: CompType::IC,
            pins,
            rot,
            custom_pwr: false,
            vcc_pin: Some(pins),
            gnd_pin: Some(pins / 2),
        }
    }

    pub fn new_other(cid: CompId, id: impl Into<String>, comp_type: CompType) -> Self {
        let pins = match comp_type {
            CompType::SW => 3,
            _ => 2,
        };
        Self {
            cid,
            id: id.into(),
            comp_type,
            pins,
            rot: 0,
            custom_pwr: false,
            vcc_pin: None,
            gnd_pin: None,
        }
    }

    /// Khớp 100% hàm `getIcPwrPins(c)` trong main.html
    #[inline]
    pub fn get_ic_pwr_pins(&self) -> (Option<u8>, Option<u8>) {
        if self.comp_type != CompType::IC {
            return (None, None);
        }
        if self.custom_pwr {
            let v = self.vcc_pin.filter(|&p| p >= 1 && p <= self.pins);
            let g = self.gnd_pin.filter(|&p| p >= 1 && p <= self.pins);
            return (v, g);
        }
        (Some(self.pins), Some(self.pins / 2))
    }
}

/// IC đã đặt trên rãnh giữa E-F (Hàng 4 & 5)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedIc {
    pub cid: CompId,
    pub id: String,
    pub pins: u8,
    pub rot: u8,
    pub c_min: u8,
    pub c_max: u8,
    pub w: u8,
}

/// Công tắc gạt 3 chân (SW) đã đặt tại hàng 0 hoặc hàng 9
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedSw {
    pub cid: CompId,
    pub id: String,
    pub c_min: u8,
    pub c_max: u8,
    pub row: i8,
    pub flip: u8,
}

/// Nút bấm 2 chân (BTN2) đã đặt trên mặt bo (Hỗ trợ 3 TH cắm: bend = 0, -1, +1)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedBtn {
    pub cid: CompId,
    pub id: String,
    pub comp_type: CompType,
    pub c_min: u8,
    pub c_max: u8,
    pub row_top: i8,
    pub row_bot: i8,
    pub flip: u8,
    #[serde(default)]
    pub bend: i8,
}

impl PlacedBtn {
    /// Hàng lỗ cắm thực tế của 2 chân BTN2: Hàng B (row = 1) ở nửa trên, Hàng I (row = 8) ở nửa dưới
    #[inline(always)]
    pub const fn pin_row(&self) -> i8 {
        if self.row_top == 7 && self.row_bot == 8 {
            8
        } else {
            self.row_top
        }
    }
}

/// Linh kiện rời 2 chân (R, LED, C, CC) đã đặt trên mặt bo
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedSpecial {
    pub cid: CompId,
    pub id: String,
    pub comp_type: CompType,
    pub c1: u8,
    pub r1: i8,
    pub c2: u8,
    pub r2: i8,
    pub locked: bool,
    pub ic_id: Option<CompId>,
}

/// Đoạn dây nối trên Breadboard
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireSeg {
    pub net: NetId,
    pub net_name: String,
    pub cells: [[i8; 2]; 2],
    pub lane: i8,
    pub trk: Option<usize>,
}

/// Bố cục toàn bộ linh kiện trên mặt bo (Tương đương đối tượng `L` trong buildBoard)
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardLayout {
    pub ics: Vec<PlacedIc>,
    pub sws: Vec<PlacedSw>,
    pub btns: Vec<PlacedBtn>,
    pub rs: Vec<PlacedSpecial>,
    pub leds: Vec<PlacedSpecial>,
    pub cs: Vec<PlacedSpecial>,
}

impl BoardLayout {
    /// Ánh xạ chân `pin` (1..=pins) của linh kiện `cid` sang lỗ cắm `(col, row)` (`pinHole` trong main.html)
    #[inline]
    pub fn pin_hole(&self, cid: CompId, pin: u8) -> Option<(u8, i8)> {
        if let Some(ic) = self.ics.iter().find(|x| x.cid == cid) {
            let h = ic.pins / 2;
            let (col, row) = if ic.rot == 0 {
                if pin <= h {
                    (ic.c_min + pin - 1, 5)
                } else {
                    (ic.c_min + (ic.pins - pin), 4)
                }
            } else if pin <= h {
                (ic.c_max - pin + 1, 4)
            } else {
                (ic.c_min + pin - h - 1, 5)
            };
            return Some((col, row));
        }
        if let Some(sw) = self.sws.iter().find(|x| x.cid == cid) {
            let col = if sw.flip != 0 {
                sw.c_max - pin + 1
            } else {
                sw.c_min + pin - 1
            };
            return Some((col, sw.row));
        }
        if let Some(b) = self.btns.iter().find(|x| x.cid == cid) {
            let c1 = if b.flip != 0 { b.c_max } else { b.c_min };
            let c2 = if b.flip != 0 { b.c_min } else { b.c_max };
            let r_pin = b.pin_row();
            return Some(if pin == 1 { (c1, r_pin) } else { (c2, r_pin) });
        }
        None
    }

    /// Chặn không gian của IC, SW (1 TH), BTN2 (3 TH: bend = 0, -1, +1), R/LED/C và 6 dây nguồn mặc định
    pub fn block_space(&self, board: &mut BoardState, fps: &mut Vec<Footprint>) {
        board.occupy_default_power_holes();
        for ic in &self.ics {
            fps.push(Footprint::for_ic(ic.c_min, ic.c_max));
            for c in ic.c_min..=ic.c_max {
                board.occupy(c, 4);
                board.occupy(c, 5);
            }
            if let Some(t4) = track_index(4, 0) {
                board.occupy_segment(t4, ic.c_min, ic.c_max);
            }
            if let Some(t5) = track_index(5, 0) {
                board.occupy_segment(t5, ic.c_min, ic.c_max);
            }
        }
        for sw in &self.sws {
            fps.push(Footprint::for_sw(sw.c_min, sw.c_max, sw.row));
            for c in sw.c_min..=sw.c_max {
                board.occupy(c, sw.row);
            }
            for lane in [0, -1, 1] {
                if let Some(tid) = track_index(sw.row, lane) {
                    board.occupy_segment(tid, sw.c_min, sw.c_max);
                }
            }
        }
        for sp in self.rs.iter().chain(self.leds.iter()).chain(self.cs.iter()) {
            board.occupy(sp.c1, sp.r1);
            board.occupy(sp.c2, sp.r2);
        }
        for btn in &self.btns {
            let r_pin = btn.pin_row();
            let r_up = r_pin - 1;
            let r_dn = r_pin + 1;
            let c_mid = u8::midpoint(btn.c_min, btn.c_max);
            fps.push(Footprint::for_btn_bent(btn.c_min, btn.c_max, r_pin, btn.bend));

            // Cả 3 TH của BTN2 đều chiếm cứng 3 ô trên hàng chân cắm (VD: (B,1), (B,2), (B,3)) và 3 làn AB, B, BC
            for c in btn.c_min..=btn.c_max {
                board.occupy(c, r_pin);
            }
            for lane in [-1, 0, 1] {
                if let Some(tid) = track_index(r_pin, lane) {
                    board.occupy_segment(tid, btn.c_min, btn.c_max);
                }
            }

            match btn.bend {
                -1 => {
                    // TH2 (bẻ nhích lên về phía A / r_up): Khóa thêm ô giữa (A,2), khóa làn A (0), nhả hoàn toàn hàng C!
                    if r_up >= 0 {
                        board.occupy(c_mid, r_up);
                        if let Some(tid_up) = track_index(r_up, 0) {
                            board.occupy_segment(tid_up, btn.c_min, btn.c_max);
                        }
                    }
                }
                1 => {
                    // TH3 (bẻ nhích xuống về phía C / r_dn): Khóa thêm ô giữa (C,2), khóa làn C (0), nhả hoàn toàn hàng A!
                    if r_dn <= 9 {
                        board.occupy(c_mid, r_dn);
                        if let Some(tid_dn) = track_index(r_dn, 0) {
                            board.occupy_segment(tid_dn, btn.c_min, btn.c_max);
                        }
                    }
                }
                _ => {
                    // TH1 (cắm thẳng, bend = 0): Mở lỗ cắm trên A và C cho dây dọc, nhưng chặn đủ 5 làn A, AB, B, BC, C
                    if r_up >= 0 {
                        if let Some(tid_up) = track_index(r_up, 0) {
                            board.occupy_segment(tid_up, btn.c_min, btn.c_max);
                        }
                    }
                    if r_dn <= 9 {
                        if let Some(tid_dn) = track_index(r_dn, 0) {
                            board.occupy_segment(tid_dn, btn.c_min, btn.c_max);
                        }
                    }
                }
            }
        }
    }
}

/// Một mạng điện sau khi phân tích từ Netlist
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedNet {
    pub id: NetId,
    pub name: String,
    pub pins: Vec<PinRef>,
    pub pin_strs: Vec<String>,
    pub is_star: bool,
}

/// Kết quả trả về của `parse_netlist` (Khớp 100% đối tượng `parsed` trong main.html + `sym_led_groups`)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ParsedNetlist {
    pub errors: Vec<String>,
    pub warns: Vec<String>,
    pub nets: Vec<ParsedNet>,
    pub net_of: FxHashMap<PinRef, NetId>,
    pub cap_ic_hint: FxHashMap<CompId, CompId>,
    #[serde(default)]
    pub sym_led_groups: Vec<Vec<CompId>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_btn2_three_bend_modes_and_sw_block_space() {
        // TH1: bend = 0 tại (B, 1..=3) -> Khóa lỗ (B,1..3), mở lỗ (A,1..3) & (C,1..3), chặn 5 làn A, AB, B, BC, C
        let mut l0 = BoardLayout::default();
        l0.btns.push(PlacedBtn {
            cid: CompId(1), id: "B1".into(), comp_type: CompType::BTN2,
            c_min: 1, c_max: 3, row_top: 1, row_bot: 2, flip: 0, bend: 0,
        });
        let (mut b0, mut fps0) = (BoardState::new(false), Vec::new());
        l0.block_space(&mut b0, &mut fps0);
        assert!(b0.is_occupied(1, 1) && b0.is_occupied(2, 1) && b0.is_occupied(3, 1));
        assert!(!b0.is_occupied(2, 0) && !b0.is_occupied(2, 2));
        assert!(!b0.is_segment_free(track_index(0, 0).unwrap(), 1, 3));
        assert!(!b0.is_segment_free(track_index(2, 0).unwrap(), 1, 3));

        // TH2: bend = -1 (nhích lên A) -> Khóa thêm lỗ (A,2), mở lỗ (A,1)/(A,3) cho dây dọc, nhả hoàn toàn làn C!
        let mut l_up = BoardLayout::default();
        l_up.btns.push(PlacedBtn { bend: -1, ..l0.btns[0].clone() });
        let (mut b_up, mut fps_up) = (BoardState::new(false), Vec::new());
        l_up.block_space(&mut b_up, &mut fps_up);
        assert!(b_up.is_occupied(2, 0) && !b_up.is_occupied(1, 0) && !b_up.is_occupied(3, 0));
        assert!(!b_up.is_segment_free(track_index(0, 0).unwrap(), 1, 3));
        assert!(b_up.is_segment_free(track_index(2, 0).unwrap(), 1, 3));

        // TH3: bend = +1 (nhích xuống C) -> Khóa thêm lỗ (C,2), mở lỗ (C,1)/(C,3) cho dây dọc, nhả hoàn toàn làn A!
        let mut l_dn = BoardLayout::default();
        l_dn.btns.push(PlacedBtn { bend: 1, ..l0.btns[0].clone() });
        let (mut b_dn, mut fps_dn) = (BoardState::new(false), Vec::new());
        l_dn.block_space(&mut b_dn, &mut fps_dn);
        assert!(b_dn.is_occupied(2, 2) && !b_dn.is_occupied(1, 2) && !b_dn.is_occupied(3, 2));
        assert!(b_dn.is_segment_free(track_index(0, 0).unwrap(), 1, 3));
        assert!(!b_dn.is_segment_free(track_index(2, 0).unwrap(), 1, 3));
    }
}
