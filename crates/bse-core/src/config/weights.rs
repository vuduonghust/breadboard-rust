use crate::model::ids::CompType;
use serde::{Deserialize, Serialize};

/// Giới hạn sải chân (đơn vị: ô lỗ cắm) cho R, LED, C, CC
/// Đã giảm ~25% sải chân tối đa của R (18.0 -> 13.5) và LED (12.0 -> 9.0) theo Đề xuất #4
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpecialSpan {
    pub min: f64,
    pub max: f64,
    pub pref: f64,
}

pub const SPECIAL_R: SpecialSpan = SpecialSpan { min: 3.0, max: 13.5, pref: 4.5 };
pub const SPECIAL_LED: SpecialSpan = SpecialSpan { min: 1.0, max: 9.0, pref: 2.5 };
pub const SPECIAL_C: SpecialSpan = SpecialSpan { min: 1.0, max: 6.0, pref: 2.0 };
pub const SPECIAL_CC: SpecialSpan = SpecialSpan { min: 1.0, max: 6.0, pref: 2.0 };

impl SpecialSpan {
    #[inline(always)]
    pub const fn for_type(t: CompType) -> Self {
        match t {
            CompType::R => SPECIAL_R,
            CompType::LED => SPECIAL_LED,
            CompType::C => SPECIAL_C,
            CompType::CC => SPECIAL_CC,
            _ => SPECIAL_R,
        }
    }
}

/// Bộ Trọng Số Toán Học Tập Trung (Thay thế toàn bộ Magic Numbers rải rác trong 3 file JS)
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CostWeights {
    // --- 1. Trọng số Đại cục IC & Phân hoạch FM (ic-core-engine.js) ---
    pub fm_ic_pin_vote: f64,
    pub fm_sw_bot_bias: f64,
    pub fm_radj_factor: f64,
    pub fm_max_per_half_ratio: f64,
    pub p1_bridge_weight: f64,
    pub p1_rcross_pen: f64,
    pub p1_load_diff_weight: f64,
    pub cross_bridge_pen: f64,
    pub cross_span_weight: f64,
    pub cross_rcross_pen: f64,
    pub cross_rclose_pen: f64,
    pub cross_rdist_weight: f64,
    pub cross_io_imbalance_pen: f64,
    pub conn_two_pin_weight: f64,
    pub conn_multi_pin_weight: f64,
    pub conn_special_lookthrough: f64,
    pub bridge_demand_periph_scale: f64,
    pub bridge_demand_cross_net_add: f64,
    pub pick_cross_gap_dist_scale: f64,

    // --- 2. Trọng số Đóng gói & Đặt Linh Kiện Rời (discrete-engine.js & main.html) ---
    pub packer_coloc_bonus: f64,
    pub pin_swap_threshold: f64,
    pub spec_body_clearance_sq: f64,
    pub spec_rail_base_bonus: f64,
    pub spec_starve_pen: f64,
    pub spec_split_series_pen: f64,
    pub spec_groove_row_pen: f64,
    pub spec_evict_col_pen: f64,
    pub sym_led_pair_bonus: f64,

    // --- 3. Trọng số Động cơ Đi dây (main.html) ---
    pub route_groove_pen_cross: f64,
    pub route_groove_pen_done: f64,
    pub route_bridge_destroy_pen: f64,
    pub route_outer_row_pen: f64,
    pub route_gap_lane_pen: f64,
    pub route_failed_boost_step: f64,

    // --- 4. Trọng số Hậu kiểm B-VCG + Left-Edge (discrete-engine.js) ---
    pub bvcg_gap_lane_pen: f64,
    pub bvcg_outer_row_pen: f64,
    pub bvcg_groove_row_pen: f64,
    pub bvcg_inversion_pen: f64,
    pub bvcg_step_row_weight: f64,
}

impl Default for CostWeights {
    #[inline]
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl CostWeights {
    /// Hằng số mặc định đã tích hợp nâng cấp sải chân R/LED (#4) và thưởng LED đối xứng (#10)
    pub const DEFAULT: Self = Self {
        fm_ic_pin_vote: 1.2,
        fm_sw_bot_bias: 0.35,
        fm_radj_factor: 0.9,
        fm_max_per_half_ratio: 0.65,
        p1_bridge_weight: 6.5,
        p1_rcross_pen: 3.5,
        p1_load_diff_weight: 1.15,
        cross_bridge_pen: 6.0,
        cross_span_weight: 0.28,
        cross_rcross_pen: 3.5,
        cross_rclose_pen: 4.0,
        cross_rdist_weight: 0.16,
        cross_io_imbalance_pen: 3.5,
        conn_two_pin_weight: 4.5,
        conn_multi_pin_weight: 3.0,
        conn_special_lookthrough: 2.8,
        bridge_demand_periph_scale: 1.1,
        bridge_demand_cross_net_add: 2.6,
        pick_cross_gap_dist_scale: 0.65,

        packer_coloc_bonus: 24.0,
        pin_swap_threshold: 0.5,
        spec_body_clearance_sq: 0.72,
        spec_rail_base_bonus: -38.0,
        spec_starve_pen: 800.0,
        spec_split_series_pen: 85.0,
        spec_groove_row_pen: 7.5,
        spec_evict_col_pen: 48.0,
        sym_led_pair_bonus: 32.0,

        route_groove_pen_cross: 24.0,
        route_groove_pen_done: 8.0,
        route_bridge_destroy_pen: 22.0,
        route_outer_row_pen: 18.0,
        route_gap_lane_pen: 0.45,
        route_failed_boost_step: 1350.0,

        bvcg_gap_lane_pen: 2.2,
        bvcg_outer_row_pen: 9.0,
        bvcg_groove_row_pen: 7.0,
        bvcg_inversion_pen: 6.5,
        bvcg_step_row_weight: 0.8,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::personas::{get_persona, PERSONAS};

    #[test]
    fn test_personas_and_weights_upgraded_constants() {
        assert_eq!(PERSONAS.len(), 5);
        assert_eq!(get_persona(0).name, "I1:Natural4.0");
        assert_eq!(get_persona(4).name, "P5:Surgeon");
        assert_eq!(get_persona(4).passes, 32);
        assert_eq!(get_persona(99).name, "I3:BridgeLED");

        let w = CostWeights::default();
        assert_eq!(w.spec_starve_pen, 800.0);
        assert_eq!(w.sym_led_pair_bonus, 32.0);
        assert_eq!(SpecialSpan::for_type(CompType::R).max, 13.5);
        assert_eq!(SpecialSpan::for_type(CompType::LED).max, 9.0);
        assert_eq!(SpecialSpan::for_type(CompType::CC).max, 6.0);
    }
}
