/// Tạo mặt nạ bit u64 cho đoạn cột [c1, c2] (1 <= c <= 63)
#[inline(always)]
pub const fn span_mask(c1: u8, c2: u8) -> u64 {
    let lo = if c1 < c2 { c1 } else { c2 };
    let hi = if c1 > c2 { c1 } else { c2 };
    let len = (hi - lo + 1) as u32;
    let ones = if len >= 64 { !0u64 } else { (1u64 << len) - 1 };
    ones << lo
}

/// Kiểm tra cột nguồn hợp lệ (10 nhóm x 5 lỗ từ cột 3..=61, nghỉ tại các cột 8, 14, 20, 26, 32, 38, 44, 50, 56)
#[inline(always)]
pub const fn rail_ok(col: u8) -> bool {
    col >= 3 && col <= 61 && (col - 3) % 6 != 5
}

/// Kiểm tra hàng có thuộc 4 thanh nguồn (-2, -1, 10, 11) hay không
#[inline(always)]
pub const fn is_rail_row(row: i8) -> bool {
    row < 0 || row > 9
}

/// Trả về nửa mặt bo: 0 = Nửa trên (hàng 0..=4), 1 = Nửa dưới (hàng 5..=9)
#[inline(always)]
pub const fn half_of(row: i8) -> u8 {
    if row <= 4 { 0 } else { 1 }
}

/// Kiểm tra lỗ cắm hợp lệ trên toàn mặt Breadboard (Khớp 100% `BreadboardRenderer.validHole`)
#[inline(always)]
pub const fn valid_hole(col: u8, row: i8) -> bool {
    if col < 1 || col > 63 || row < -2 || row > 11 {
        return false;
    }
    if is_rail_row(row) {
        rail_ok(col)
    } else {
        true
    }
}

/// Ánh xạ (row, lane) sang chỉ số mảng `track_masks` (0..19):
/// - lane = 0 (tâm lỗ): row 0..=9 -> index 0..=9
/// - lane = ±1 (rãnh phụ giữa 2 hàng): gap_{r}_{r+1} -> index 10 + r (với r in 0..=3 hoặc 5..=8)
#[inline(always)]
pub const fn track_index(row: i8, lane: i8) -> Option<usize> {
    if row < 0 || row > 9 {
        return None;
    }
    match lane {
        0 => Some(row as usize),
        1 => {
            if row == 4 || row == 9 {
                None
            } else {
                Some(10 + (row as usize))
            }
        }
        -1 => {
            if row == 0 || row == 5 {
                None
            } else {
                Some(10 + ((row - 1) as usize))
            }
        }
        _ => None,
    }
}

/// 12 lỗ cắm mặc định bị chiếm bởi 6 dây nhảy nguồn cố định trên Breadboard
pub const DEFAULT_POWER_HOLES: [(u8, i8); 12] = [
    (3, -1), (3, 11), (61, -2), (61, 10),
    (31, -1), (33, -1), (31, 11), (33, 11),
    (31, -2), (33, -2), (31, 10), (33, 10),
];

/// Trạng thái mặt bo siêu tốc trên Stack (~432 bytes) - Thay thế `BreadboardRouter` + Undo Log
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardState {
    /// 14 hàng lỗ cắm: từ row = -2 (index 0) đến row = 11 (index 13)
    pub occupied_holes: [u64; 14],
    /// 10 làn tâm lỗ (0..10) + 9 vị trí rãnh phụ (10..19)
    pub track_masks: [u64; 19],
    /// Mạng điện hiện tại của từng dải 5 lỗ: col_net[half][col] (0 = trống)
    pub col_net: [[u16; 64]; 2],
    /// Cờ đảo thứ tự ưu tiên hàng (tương đương `R.rev` trong JS)
    pub rev: bool,
}

impl BoardState {
    #[inline(always)]
    pub const fn new(rev: bool) -> Self {
        Self {
            occupied_holes: [0u64; 14],
            track_masks: [0u64; 19],
            col_net: [[0u16; 64]; 2],
            rev,
        }
    }

    /// Khóa 12 lỗ cắm của 6 dây nguồn mặc định (đầu hàm `blockSpace` trong main.html)
    #[inline]
    pub fn occupy_default_power_holes(&mut self) {
        let mut i = 0;
        while i < DEFAULT_POWER_HOLES.len() {
            let (c, r) = DEFAULT_POWER_HOLES[i];
            self.occupy(c, r);
            i += 1;
        }
    }

    #[inline(always)]
    pub const fn is_occupied(&self, col: u8, row: i8) -> bool {
        let idx = (row + 2) as usize;
        (self.occupied_holes[idx] & (1u64 << col)) != 0
    }

    #[inline(always)]
    pub fn occupy(&mut self, col: u8, row: i8) -> bool {
        let idx = (row + 2) as usize;
        let bit = 1u64 << col;
        if (self.occupied_holes[idx] & bit) != 0 {
            return false;
        }
        self.occupied_holes[idx] |= bit;
        true
    }

    #[inline(always)]
    pub fn unoccupy(&mut self, col: u8, row: i8) -> bool {
        let idx = (row + 2) as usize;
        let bit = 1u64 << col;
        if (self.occupied_holes[idx] & bit) == 0 {
            return false;
        }
        self.occupied_holes[idx] &= !bit;
        true
    }

    #[inline(always)]
    pub const fn is_segment_free(&self, track_id: usize, c1: u8, c2: u8) -> bool {
        (self.track_masks[track_id] & span_mask(c1, c2)) == 0
    }

    #[inline(always)]
    pub fn occupy_segment(&mut self, track_id: usize, c1: u8, c2: u8) {
        self.track_masks[track_id] |= span_mask(c1, c2);
    }

    #[inline(always)]
    pub fn unoccupy_segment(&mut self, track_id: usize, c1: u8, c2: u8) {
        self.track_masks[track_id] &= !span_mask(c1, c2);
    }

    /// Tìm làn tâm lỗ (0) hoặc rãnh phụ (±1) còn trống tại hàng `row` cho đoạn `[c1, c2]`
    /// Khớp chính xác 100% thứ tự ưu tiên của `BreadboardRouter.getAvailableTrackForRow`
    #[inline]
    pub fn get_available_track_for_row(&self, row: i8, c1: u8, c2: u8) -> Option<(i8, usize)> {
        let pref_lane = if row == 4 { -1 } else { 1 };
        let lanes = [0, pref_lane, -pref_lane];
        let mask = span_mask(c1, c2);
        for lane in lanes {
            if let Some(tid) = track_index(row, lane) {
                if (self.track_masks[tid] & mask) == 0 {
                    return Some((lane, tid));
                }
            }
        }
        None
    }

    /// Đếm số lỗ trống trong 1 dải 5 lỗ tại cột `col` thuộc nửa `half` (0 hoặc 1)
    #[inline(always)]
    pub const fn free_hole_count(&self, col: u8, half: u8) -> u8 {
        let bit = 1u64 << col;
        let start = if half == 0 { 2 } else { 7 }; // row 0 -> idx 2, row 5 -> idx 7
        let mut cnt = 0;
        let mut r_idx = start;
        while r_idx < start + 5 {
            if (self.occupied_holes[r_idx] & bit) == 0 {
                cnt += 1;
            }
            r_idx += 1;
        }
        cnt
    }

    #[inline(always)]
    pub const fn has_free_hole(&self, col: u8, half: u8) -> bool {
        self.free_hole_count(col, half) > 0
    }
}
