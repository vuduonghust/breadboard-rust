use serde::{Deserialize, Serialize};

/// Tính khoảng cách Euclid siêu tốc bằng lệnh `f64.sqrt` phần cứng của CPU/WASM
#[inline(always)]
pub fn fast_hypot(dx: f64, dy: f64) -> f64 {
    (dx * dx + dy * dy).sqrt()
}

/// Khung giới hạn vật lý (Bounding Box) của IC, SW, BTN2
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Footprint {
    pub is_sw: bool,
    pub c0: f64,
    pub c1: f64,
    pub r0: f64,
    pub r1: f64,
}

impl Footprint {
    #[inline(always)]
    pub const fn for_ic(c_min: u8, c_max: u8) -> Self {
        Self {
            is_sw: false,
            c0: (c_min as f64) - 0.45,
            c1: (c_max as f64) + 0.45,
            r0: 3.7,
            r1: 5.3,
        }
    }

    #[inline(always)]
    pub const fn for_sw(c_min: u8, c_max: u8, row: i8) -> Self {
        Self {
            is_sw: true,
            c0: (c_min as f64) - 0.45,
            c1: (c_max as f64) + 0.45,
            r0: (row as f64) - 0.45,
            r1: (row as f64) + 0.45,
        }
    }

    #[inline(always)]
    pub const fn for_btn(c_min: u8, c_max: u8, row_top: i8, row_bot: i8) -> Self {
        Self {
            is_sw: false,
            c0: (c_min as f64) - 0.45,
            c1: (c_max as f64) + 0.45,
            r0: (row_top as f64) - 0.3,
            r1: (row_bot as f64) + 0.3,
        }
    }

    /// Footprint BTN2 theo 3 trường hợp cắm (#2: TH1 bend=0, TH2 bend=-1 nhích lên, TH3 bend=+1 nhích xuống)
    #[inline(always)]
    pub const fn for_btn_bent(c_min: u8, c_max: u8, row_pin: i8, bend: i8) -> Self {
        let (r0, r1) = match bend {
            -1 => ((row_pin as f64) - 1.15, (row_pin as f64) + 0.45),
            1 => ((row_pin as f64) - 0.45, (row_pin as f64) + 1.15),
            _ => ((row_pin as f64) - 1.10, (row_pin as f64) + 1.10),
        };
        Self {
            is_sw: false,
            c0: (c_min as f64) - 0.45,
            c1: (c_max as f64) + 0.45,
            r0,
            r1,
        }
    }
}

/// Tọa độ Y trực quan có tính khoảng cách rãnh nguồn AS/JS & rãnh E-F (`visY` trong main.html)
#[inline(always)]
pub const fn vis_y(row: i8) -> f64 {
    let mut y = row as i32;
    if row >= 0 {
        y += 2;
    }
    if row >= 5 {
        y += 2;
    }
    if row >= 10 {
        y += 2;
    }
    y as f64
}

/// Kiểm tra đoạn thẳng đứng tại cột `col` từ `r_a` đến `r_b` không đè lên thân IC/SW/BTN2 (`R.vSegFree`)
#[inline]
pub fn v_seg_free(fps: &[Footprint], col: u8, r_a: i8, r_b: i8) -> bool {
    let lo = (r_a.min(r_b) as f64) + 0.35;
    let hi = (r_a.max(r_b) as f64) - 0.35;
    if hi <= lo {
        return true;
    }
    let c = col as f64;
    let is_pwr_jump = r_a < 0 || r_a > 9 || r_b < 0 || r_b > 9;
    for f in fps {
        if is_pwr_jump && f.is_sw && c > f.c0 && c < f.c1 {
            continue;
        }
        if c > f.c0 && c < f.c1 && lo.max(f.r0) < hi.min(f.r1) {
            return false;
        }
    }
    true
}

/// Kiểm tra đoạn thẳng (x1, y1) -> (x2, y2) có cắt qua thân linh kiện nào không (`R.segHitsFp`)
/// Đề xuất #2: Cho phép dây nguồn từ lỗ sát SW (như hàng I=8 hoặc B=1) nhảy qua SW ở hàng J=9 / A=0 ra thẳng Rail!
#[inline]
pub fn seg_hits_fp(fps: &[Footprint], x1: f64, y1: f64, x2: f64, y2: f64) -> bool {
    if fps.is_empty() {
        return false;
    }
    let min_x = x1.min(x2);
    let max_x = x1.max(x2);
    let min_y = y1.min(y2);
    let max_y = y1.max(y2);
    let dx = x2 - x1;
    let dy = y2 - y1;
    let is_pwr_jump = y1 < 0.0 || y1 > 9.0 || y2 < 0.0 || y2 > 9.0;

    for f in fps {
        if max_x <= f.c0 || min_x >= f.c1 || max_y <= f.r0 || min_y >= f.r1 {
            continue;
        }
        if is_pwr_jump && f.is_sw && x1 > f.c0 && x1 < f.c1 {
            continue;
        }
        let mut t0 = 0.0_f64;
        let mut t1 = 1.0_f64;
        if dx == 0.0 {
            if x1 <= f.c0 || x1 >= f.c1 {
                continue;
            }
        } else {
            let inv_x = 1.0 / dx;
            let mut tx0 = (f.c0 - x1) * inv_x;
            let mut tx1 = (f.c1 - x1) * inv_x;
            if tx0 > tx1 {
                core::mem::swap(&mut tx0, &mut tx1);
            }
            if tx0 > t0 {
                t0 = tx0;
            }
            if tx1 < t1 {
                t1 = tx1;
            }
            if t0 > t1 {
                continue;
            }
        }
        if dy == 0.0 {
            if y1 <= f.r0 || y1 >= f.r1 {
                continue;
            }
        } else {
            let inv_y = 1.0 / dy;
            let mut ty0 = (f.r0 - y1) * inv_y;
            let mut ty1 = (f.r1 - y1) * inv_y;
            if ty0 > ty1 {
                core::mem::swap(&mut ty0, &mut ty1);
            }
            if ty0 > t0 {
                t0 = ty0;
            }
            if ty1 < t1 {
                t1 = ty1;
            }
            if t0 > t1 {
                continue;
            }
        }
        return true;
    }
    false
}

#[inline(always)]
pub fn pt_seg_dist_sq(px: f64, py: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let dx = x2 - x1;
    let dy = y2 - y1;
    let l2 = dx * dx + dy * dy;
    if l2 == 0.0 {
        return (px - x1) * (px - x1) + (py - y1) * (py - y1);
    }
    let t = (((px - x1) * dx + (py - y1) * dy) / l2).clamp(0.0, 1.0);
    let proj_x = px - (x1 + t * dx);
    let proj_y = py - (y1 + t * dy);
    proj_x * proj_x + proj_y * proj_y
}

#[inline(always)]
fn ccw(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (c[1] - a[1]) * (b[0] - a[0]) - (b[1] - a[1]) * (c[0] - a[0])
}

#[inline(always)]
pub fn segs_strictly_intersect(p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], p4: [f64; 2]) -> bool {
    let d1 = ccw(p3, p4, p1);
    let d2 = ccw(p3, p4, p2);
    let d3 = ccw(p1, p2, p3);
    let d4 = ccw(p1, p2, p4);
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

#[inline]
pub fn seg_dist_sq(p1: [f64; 2], p2: [f64; 2], p3: [f64; 2], p4: [f64; 2]) -> f64 {
    if segs_strictly_intersect(p1, p2, p3, p4) {
        return 0.0;
    }
    pt_seg_dist_sq(p1[0], p1[1], p3[0], p3[1], p4[0], p4[1])
        .min(pt_seg_dist_sq(p2[0], p2[1], p3[0], p3[1], p4[0], p4[1]))
        .min(pt_seg_dist_sq(p3[0], p3[1], p1[0], p1[1], p2[0], p2[1]))
        .min(pt_seg_dist_sq(p4[0], p4[1], p1[0], p1[1], p2[0], p2[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sw_power_jump_not_blocked_by_own_footprint() {
        let fps = vec![Footprint::for_sw(10, 12, 9)];
        // Dây nguồn từ hàng I (r=8) tại cột 10 nhảy qua hàng J (r=9) xuống thanh nguồn (r=10) không bị chặn!
        assert!(!seg_hits_fp(&fps, 10.0, 8.0, 10.0, 10.0));
        assert!(v_seg_free(&fps, 10, 8, 10));
    }
}
