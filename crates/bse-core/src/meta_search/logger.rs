use crate::meta_search::builder::{BoardSolution, CandidateOpts, SearchPhase};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct ErrorTypeCounts {
    pub rled_unplaced: u32,
    pub rail_blocked: u32,
    pub sig_congested: u32,
    pub short_circuit: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct MilestoneSnap {
    pub it: u32,
    pub t_ms: u64,
    pub phase: String,
    pub score: i64,
    pub unplaced: usize,
    pub err_count: usize,
    pub warn_count: usize,
    pub ortho_count: usize,
    pub wire_count: usize,
    pub wire_len: i32,
    pub max_c: u8,
    pub ic_layout: String,
    pub sw_layout: String,
    pub btn_layout: String,
    pub bridges: Vec<u8>,
    pub gaps: Vec<i32>,
    pub errors: Vec<String>,
}

/// Hệ thống lưu Nhật ký & Chẩn đoán nguyên nhân thất bại (`SearchLogger` trong main.html)[cite: 4]
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchLogger {
    pub total_nets: usize,
    pub iterations: u32,
    pub unique_configs: u32,
    pub duplicates: u32,
    pub elapsed_ms: u64,
    pub phase_counts: [u32; 6],
    pub err_types: ErrorTypeCounts,
    pub net_fails: FxHashMap<String, u32>,
    pub comp_fails: FxHashMap<String, u32>,
    pub col_hotspots: FxHashMap<u8, u32>,
    pub milestones: Vec<MilestoneSnap>,
    pub best_snap: Option<MilestoneSnap>,
}

impl SearchLogger {
    pub fn new(total_nets: usize) -> Self {
        Self { total_nets, ..Default::default() }
    }

    pub fn reset(&mut self, fresh: bool, total_nets: usize) {
        if fresh || self.iterations == 0 {
            *self = Self::new(total_nets);
        }
    }

    pub fn record(
        &mut self,
        sol: &BoardSolution,
        opts: &CandidateOpts,
        it: u32,
        elapsed_ms: u64,
        is_dup: bool,
    ) {
        self.iterations = it;
        self.elapsed_ms = elapsed_ms;
        if is_dup {
            self.duplicates += 1;
        } else {
            self.unique_configs += 1;
        }
        let p_idx = match opts.phase {
            SearchPhase::Init => 0,
            SearchPhase::P1Unblock => 1,
            SearchPhase::P1Bridge => 2,
            SearchPhase::P2Ortho => 3,
            SearchPhase::P3Compact => 4,
            SearchPhase::Diversify => 5,
        };
        self.phase_counts[p_idx] += 1;

        for e in &sol.errors {
            if e.contains("không có 2 lỗ trống") {
                self.err_types.rled_unplaced += 1;
                if let Some((cid, _)) = e.split_once(':') {
                    *self.comp_fails.entry(cid.trim().to_string()).or_insert(0) += 1;
                }
            } else if e.contains("dây nguồn") || e.contains("dây rail") {
                self.err_types.rail_blocked += 1;
                if let Some((n, _)) = e.split_once(':') {
                    *self.net_fails.entry(n.trim().to_string()).or_insert(0) += 1;
                }
            } else if e.contains("ngắn mạch") || e.contains("Ngắn mạch") {
                self.err_types.short_circuit += 1;
            } else {
                self.err_types.sig_congested += 1;
                if let Some(rest) = e.strip_prefix("Net ") {
                    if let Some((n, _)) = rest.split_once(':') {
                        let net_name = n.trim().to_string();
                        *self.net_fails.entry(net_name.clone()).or_insert(0) += 1;
                        for (cid, nets) in &sol.comp_nets {
                            if nets.contains(&net_name) {
                                *self.comp_fails.entry(cid.clone()).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
            if let Some(pos) = e.find("cột ") {
                let after = &e[pos + "cột ".len()..];
                let num_s: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(col) = num_s.parse::<u8>() {
                    *self.col_hotspots.entry(col).or_insert(0) += 1;
                }
            }
        }

        if self.best_snap.as_ref().is_none_or(|b| (sol.score.round() as i64) < b.score) {
            let ic_layout = sol.layout.ics.iter()
                .map(|x| format!("{}{}[{}-{}]", x.id, if x.rot != 0 { "(R)" } else { "" }, x.c_min, x.c_max))
                .collect::<Vec<_>>().join(" ");
            let sw_layout = sol.layout.sws.iter()
                .map(|x| format!("{}[{}-{}]", x.id, x.c_min, x.c_max))
                .collect::<Vec<_>>().join(" ");
            let btn_layout = sol.layout.btns.iter()
                .map(|x| format!("{}({})[{}-{}]", x.id, if x.row_top <= 4 { "U" } else { "L" }, x.c_min, x.c_max))
                .collect::<Vec<_>>().join(" ");
            let bridges: Vec<u8> = sol.fast_wires.iter()
                .filter(|w| w.c1 == w.c2 && w.r1.min(w.r2) == 4 && w.r1.max(w.r2) == 5)
                .map(|w| w.c1)
                .collect();

            let snap = MilestoneSnap {
                it,
                t_ms: elapsed_ms,
                phase: opts.phase.as_str().to_string(),
                score: sol.score.round() as i64,
                unplaced: sol.unplaced,
                err_count: sol.errors.len(),
                warn_count: sol.warns.len(),
                ortho_count: sol.ortho_count,
                wire_count: sol.wires.len(),
                wire_len: sol.len,
                max_c: sol.max_c,
                ic_layout,
                sw_layout,
                btn_layout,
                bridges,
                gaps: opts.gaps.clone().unwrap_or_default(),
                errors: sol.errors.clone(),
            };
            self.best_snap = Some(snap.clone());
            self.milestones.push(snap);
            if self.milestones.len() > 120 {
                let remove_cnt = self.milestones.len() - 120;
                self.milestones.drain(1..=remove_cnt);
            }
        }
    }

    fn top_entries_str(map: &FxHashMap<String, u32>, k: usize) -> String {
        if map.is_empty() {
            return "Không có".into();
        }
        let mut v: Vec<(&String, &u32)> = map.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        v.into_iter().take(k).map(|(key, val)| format!("{key} ({val})")).collect::<Vec<_>>().join(", ")
    }

    pub fn build_text_report(&self) -> String {
        let b = self.best_snap.clone().unwrap_or_default();
        let rate = if self.elapsed_ms > 0 {
            (self.iterations as f64) * 1000.0 / (self.elapsed_ms as f64)
        } else {
            0.0
        };
        let pc = &self.phase_counts;
        let et = &self.err_types;
        let mut hot_v: Vec<(&u8, &u32)> = self.col_hotspots.iter().collect();
        hot_v.sort_by(|a, b| b.1.cmp(a.1));
        let hot_str = if hot_v.is_empty() {
            "Không có".into()
        } else {
            hot_v.into_iter().take(10).map(|(c, v)| format!("Cột {c} ({v})")).collect::<Vec<_>>().join(", ")
        };
        let gaps_s = b.gaps.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
        let br_s = b.bridges.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");

        let mut lines = vec![
            "=== BÁO CÁO PHÂN TÍCH TÌM KIẾM BREADBOARD AUTO-ROUTER ===".into(),
            format!("Tổng cấu hình thử : {} ({} duy nhất, {} trùng lặp)", self.iterations, self.unique_configs, self.duplicates),
            format!("Thời gian chạy    : {:.2}s (Tốc độ: {:.1} cấu hình/giây)", (self.elapsed_ms as f64) / 1000.0, rate),
            format!("Phân bổ pha tìm   : Khởi tạo={}, Gỡ lỗi={}, Mở khe cầu={}, Nắn thẳng R/LED={}, Thu gọn={}, Đa dạng hóa={}", pc[0], pc[1], pc[2], pc[3], pc[4], pc[5]),
            String::new(),
            format!("--- 1. KẾT QUẢ TỐT NHẤT HIỆN TẠI (Vòng #{} - {}ms - Pha: {}) ---", b.it, b.t_ms, b.phase),
            format!("- Điểm mục tiêu   : {} (Thiếu: {}, Lỗi: {}, Cảnh báo: {})", b.score, b.unplaced, b.err_count, b.warn_count),
            format!("- Thẩm mỹ & Dây   : {} R/LED thẳng hàng, {} đoạn dây, Dài {} ô, Rộng {}/63 cột", b.ortho_count, b.wire_count, b.wire_len, b.max_c),
            format!("- Bố cục IC       : {}", if b.ic_layout.is_empty() { "Trống" } else { &b.ic_layout }),
            format!("- Bố cục BTN      : {}", if b.btn_layout.is_empty() { "Trống" } else { &b.btn_layout }),
            format!("- Bố cục SW       : {}", if b.sw_layout.is_empty() { "Trống" } else { &b.sw_layout }),
            format!("- Mảng khe hở     : [{gaps_s}]"),
            format!("- Cột cầu rãnh E-F: Đã dùng {} cột [{br_s}]", b.bridges.len()),
            format!("- Lỗi còn tồn đọng ({}):", b.errors.len()),
        ];
        if b.errors.is_empty() {
            lines.push("   ✓ 0 lỗi (Hoàn hảo)".into());
        } else {
            for e in &b.errors {
                lines.push(format!("   ✕ {e}"));
            }
        }
        lines.extend([
            String::new(),
            "--- 2. THỐNG KÊ CHẨN ĐOÁN NGUYÊN NHÂN THẤT BẠI TRÊN TOÀN QUÁ TRÌNH ---".into(),
            format!("- Phân loại lỗi   : Nghẽn tín hiệu/Cầu rãnh={}, Kẹt dây nguồn Rail={}, Hết chỗ cắm R/LED={}, Ngắn mạch={}", et.sig_congested, et.rail_blocked, et.rled_unplaced, et.short_circuit),
            format!("- Top Net kẹt nhất: {}", Self::top_entries_str(&self.net_fails, 10)),
            format!("- Top Linh kiện   : {}", Self::top_entries_str(&self.comp_fails, 10)),
            format!("- Điểm nóng (Cột) : {hot_str}"),
            String::new(),
            "--- 3. LỊCH SỬ CỘT MỐC TIẾN HÓA (MILESTONES) ---".into(),
        ]);
        for (i, m) in self.milestones.iter().enumerate() {
            lines.push(format!(
                "  #{} [Vòng {} - {}ms - {}] -> Lỗi: {}, Thiếu: {}, R/LED thẳng: {}, Dài: {}, Rộng: {}c, IC: {}",
                i + 1, m.it, m.t_ms, m.phase, m.err_count, m.unplaced, m.ortho_count, m.wire_len, m.max_c, m.ic_layout
            ));
        }
        lines.join("\n")
    }
}
