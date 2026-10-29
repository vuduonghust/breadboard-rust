use serde::{Deserialize, Serialize};

/// ID số nguyên của Linh kiện (Thay thế chuỗi "U1", "R1", "L1"... trong vòng lặp nóng)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CompId(pub u8);

/// ID số nguyên của Mạng điện (0 = Trống, 1 = VCC, 2 = GND, >=3 = Các mạng tín hiệu/Tag nguồn)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NetId(pub u16);

impl NetId {
    pub const NONE: NetId = NetId(0);
    pub const VCC: NetId = NetId(1);
    pub const GND: NetId = NetId(2);

    #[inline(always)]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    #[inline(always)]
    pub const fn is_power(self) -> bool {
        self.0 == 1 || self.0 == 2
    }
}

/// Tham chiếu cực kỳ nhỏ gọn tới 1 chân linh kiện (2 bytes trên Stack)
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PinRef {
    pub comp: CompId,
    pub pin: u8, // Chân đánh số từ 1..=pins
}

/// Phân loại linh kiện trên Breadboard (Đã loại bỏ hoàn toàn BTN4)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompType {
    IC,   // Vi mạch DIP (8..40 chân chẵn)
    R,    // Điện trở (2 chân)
    LED,  // Đèn LED phân cực (Chân 1: Anode +, Chân 2: Cathode -)
    C,    // Tụ hóa phân cực (Chân 1: +, Chân 2: -)
    CC,   // Tụ gốm không phân cực (2 chân)
    SW,   // Công tắc gạt 3 chân
    BTN2, // Nút bấm 2 chân
}

impl CompType {
    #[inline(always)]
    pub const fn is_special(self) -> bool {
        matches!(self, CompType::R | CompType::LED | CompType::C | CompType::CC)
    }

    #[inline(always)]
    pub const fn is_io(self) -> bool {
        matches!(self, CompType::BTN2 | CompType::SW)
    }
}

/// Bộ sinh số giả ngẫu nhiên Mulberry32 khớp bit-for-bit với hàm `mulberry32(a)` trong main.html
#[derive(Clone, Copy, Debug)]
pub struct Mulberry32 {
    state: u32,
}

impl Mulberry32 {
    #[inline(always)]
    pub const fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    #[inline(always)]
    pub fn next_f64(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x6D2B79F5);
        let mut t = self.state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61)) ^ t;
        ((t ^ (t >> 14)) as f64) / 4294967296.0
    }

    #[inline(always)]
    pub fn next_usize(&mut self, max_exclusive: usize) -> usize {
        if max_exclusive <= 1 {
            return 0;
        }
        (self.next_f64() * (max_exclusive as f64)) as usize
    }

    #[inline]
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        let len = slice.len();
        for i in (1..len).rev() {
            let j = self.next_usize(i + 1);
            slice.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comp_type_and_mulberry32() {
        assert_eq!(NetId::default(), NetId::NONE);
        assert!(CompType::R.is_special());
        assert!(CompType::BTN2.is_io());
        let mut rng = Mulberry32::new(1);
        assert!((rng.next_f64() - 0.6270739405881613).abs() < 1e-15);
        assert!((rng.next_f64() - 0.002735721180215478).abs() < 1e-15);
    }
}
