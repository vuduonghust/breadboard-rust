use serde::{Deserialize, Serialize};

/// Cấu hình chiến thuật cho từng Quần đảo (Island) và Bác sĩ Vi phẫu (P5:Surgeon)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PersonaConfig {
    pub id: u8,
    pub name: &'static str,
    pub br_pen: f64,
    pub aux_share: f64,
    pub rail_share: f64,
    pub ortho_w: f64,
    pub cross_pri: f64,
    pub short_first: bool,
    pub led_first: bool,
    pub pack_io: bool,
    pub hot_gap: bool,
    pub interleave: bool,
    pub passes: usize,
    pub swap: bool,
    pub fb: usize,
}

/// 5 Personas khớp chính xác 100% với mảng `PERSONAS` trong main.html
pub const PERSONAS: [PersonaConfig; 5] = [
    // Đảo 0: Tinh hoa Bản 4.0 (Dây ngang dài đi trước, R/C tự nhiên thoáng)
    PersonaConfig {
        id: 0,
        name: "I1:Natural4.0",
        br_pen: 16.0,
        aux_share: 14.0,
        rail_share: 8.0,
        ortho_w: 6.5,
        cross_pri: -30.0,
        short_first: false,
        led_first: false,
        pack_io: false,
        hot_gap: false,
        interleave: false,
        passes: 20,
        swap: true,
        fb: 1,
    },
    // Đảo 1: Xen kẽ cụm BTN/SW quanh U1 & Mở khe cầu theo mật độ chân IC nóng
    PersonaConfig {
        id: 1,
        name: "I2:InterleaveIO",
        br_pen: 28.0,
        aux_share: 19.0,
        rail_share: 5.0,
        ortho_w: 8.0,
        cross_pri: -44.0,
        short_first: false,
        led_first: false,
        pack_io: false,
        hot_gap: true,
        interleave: true,
        passes: 22,
        swap: true,
        fb: 1,
    },
    // Đảo 2: Bảo vệ cầu rãnh E-F tối đa & Neo LED vào thanh nguồn trước R
    PersonaConfig {
        id: 2,
        name: "I3:BridgeLED",
        br_pen: 36.0,
        aux_share: 22.0,
        rail_share: 3.0,
        ortho_w: 11.0,
        cross_pri: -52.0,
        short_first: false,
        led_first: true,
        pack_io: true,
        hot_gap: false,
        interleave: false,
        passes: 22,
        swap: true,
        fb: 1,
    },
    // Đảo 3: Ép thẳng đứng R/LED/C ngay từ đầu (Ortho=22) & Ưu tiên khóa dây ngắn/Fanout trước
    PersonaConfig {
        id: 3,
        name: "I4:OrthoShort",
        br_pen: 32.0,
        aux_share: 20.0,
        rail_share: 5.5,
        ortho_w: 22.0,
        cross_pri: -58.0,
        short_first: true,
        led_first: false,
        pack_io: false,
        hot_gap: true,
        interleave: false,
        passes: 22,
        swap: true,
        fb: 2,
    },
    // P4 (P5:Surgeon): Bác sĩ Vi phẫu (Giữ nguyên R/LED/C tốt, chỉ nhổ R/C tại cột lỗi)
    PersonaConfig {
        id: 4,
        name: "P5:Surgeon",
        br_pen: 35.0,
        aux_share: 22.0,
        rail_share: 3.5,
        ortho_w: 7.5,
        cross_pri: -42.0,
        short_first: false,
        led_first: true,
        pack_io: false,
        hot_gap: true,
        interleave: false,
        passes: 32,
        swap: true,
        fb: 2,
    },
];

#[inline(always)]
pub const fn get_persona(idx: usize) -> PersonaConfig {
    if idx < PERSONAS.len() {
        PERSONAS[idx]
    } else {
        PERSONAS[2]
    }
}
