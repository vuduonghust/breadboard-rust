use crate::config::weights::CostWeights;
use crate::model::bitboard::{half_of, rail_ok, BoardState};
use crate::model::geometry::{seg_dist_sq, seg_hits_fp, vis_y, Footprint};
use crate::model::ids::NetId;
use crate::routing::bridge_pair::route_same_half;
use crate::routing::h_route::{is_potential_bridge_col, FastWire, PassWires};
use crate::routing::steiner::{strip_idx, StripUf};

/// Bộ theo dõi các đoạn dây nguồn đã đặt trên Stack (Thay thế `R.pwrSegs` trong main.html)
#[derive(Clone, Copy, Debug)]
pub struct PwrSegTracker {
    segs: [(u8, i8, u8, i8); 64],
    pub len: usize,
}

impl Default for PwrSegTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl PwrSegTracker {
    #[inline(always)]
    pub const fn new() -> Self {
        Self { segs: [(0, 0, 0, 0); 64], len: 0 }
    }

    #[inline(always)]
    pub fn push(&mut self, c1: u8, r1: i8, c2: u8, r2: i8) {
        if self.len < self.segs.len() {
            self.segs[self.len] = (c1, r1, c2, r2);
            self.len += 1;
        }
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &[(u8, i8, u8, i8)] {
        &self.segs[..self.len]
    }
}

#[inline]
fn can_place_pwr_wire(
    board: &BoardState,
    pwr_segs: &PwrSegTracker,
    fps: &[Footprint],
    cd: u8,
    r_strip: i8,
    c: u8,
    up: bool,
    rr: i8,
) -> bool {
    if board.is_occupied(cd, r_strip) || board.is_occupied(c, rr) {
        return false;
    }
    if seg_hits_fp(fps, cd as f64, r_strip as f64, c as f64, rr as f64) {
        return false;
    }
    if cd == c {
        if rr == -2 && board.is_occupied(c, -1) {
            return false;
        }
        if rr == 11 && board.is_occupied(c, 10) {
            return false;
        }
    }
    let p1 = [cd as f64, vis_y(r_strip)];
    let p2 = [c as f64, vis_y(rr)];
    let def_y1 = vis_y(if up { -1 } else { 10 });
    let def_y2 = vis_y(if up { -2 } else { 11 });
    if seg_dist_sq(p1, p2, [31.0, def_y1], [33.0, def_y1]) < 0.20
        || seg_dist_sq(p1, p2, [31.0, def_y2], [33.0, def_y2]) < 0.20
    {
        return false;
    }
    for &(s0, s1, s2, s3) in pwr_segs.as_slice() {
        if seg_dist_sq(p1, p2, [s0 as f64, vis_y(s1)], [s2 as f64, vis_y(s3)]) < 0.12 {
            return false;
        }
    }
    true
}

/// Định tuyến Hình Sao trực tiếp từ dải cột chân nguồn lên thanh nguồn VCC/GND (`railConnect`)
#[allow(clippy::too_many_arguments)]
pub fn rail_connect(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    pwr_segs: &mut PwrSegTracker,
    fps: &[Footprint],
    uf: &mut StripUf,
    net: NetId,
    tag: u16,
    group: &[(u8, i8)],
    rem_cross: i32,
    col_hist: &[f32; 65],
    row_hist: &[f32; 10],
    weights: &CostWeights,
    pass_err: &mut Vec<String>,
) -> Vec<FastWire> {
    let rail_root = uf.alloc_virtual_root();
    let mut out_wires = Vec::new();

    let mut comp_groups: Vec<(u8, Vec<(u8, i8)>)> = Vec::new();
    for &pt in group {
        let root = uf.find(strip_idx(pt.0, half_of(pt.1)));
        if let Some(g) = comp_groups.iter_mut().find(|(r, _)| *r == root) {
            g.1.push(pt);
        } else {
            comp_groups.push((root, vec![pt]));
        }
    }

    let mut rcols = [0u8; 50];
    let mut rcols_len = 0usize;
    for c in 3u8..=61 {
        if rail_ok(c) {
            rcols[rcols_len] = c;
            rcols_len += 1;
        }
    }
    let rcols = &rcols[..rcols_len];

    for (root, pts) in comp_groups {
        if uf.find(root) == uf.find(rail_root) {
            continue;
        }
        let up = pts[0].1 <= 4;
        let h = if up { 0u8 } else { 1u8 };
        let is_vcc = net == NetId::VCC;
        let rr = if up {
            if is_vcc { -1i8 } else { -2i8 }
        } else if is_vcc {
            11i8
        } else {
            10i8
        };
        let strip_rows = if up { [0i8, 1, 2, 3] } else { [9i8, 8, 7, 6] };
        let mut own_cols = Vec::with_capacity(pts.len());
        let mut own_mask = 0u64;
        for &(c, _) in &pts {
            own_cols.push(c);
            own_mask |= 1u64 << c;
        }

        let mut try_cand_list =
            |board: &mut BoardState, col_list: &[u8], max_delta_c: i32| -> Option<Vec<FastWire>> {
                let mut pairs = Vec::with_capacity(col_list.len() * 12);
                for &cd in col_list {
                    let is_own = (own_mask & (1u64 << cd)) != 0;
                    if !is_own && board.col_net[h as usize][cd as usize] != 0 {
                        continue;
                    }
                    let step_dist = if is_own {
                        0
                    } else {
                        own_cols
                            .iter()
                            .map(|&oc| (cd as i32 - oc as i32).abs())
                            .min()
                            .unwrap_or(0)
                    };
                    for (r_idx, &r_strip) in strip_rows.iter().enumerate() {
                        if !is_own && r_idx > 1 {
                            continue;
                        }
                        if board.is_occupied(cd, r_strip) {
                            continue;
                        }
                        for &c in rcols {
                            let dc = (c as i32 - cd as i32).abs();
                            if dc > max_delta_c {
                                continue;
                            }
                            let pwr_len = (dc as f64).hypot((vis_y(rr) - vis_y(r_strip)).abs());
                            let br_pen = if is_own {
                                0.0
                            } else if is_potential_bridge_col(board, fps, cd, 0, rem_cross) {
                                24.0
                            } else {
                                4.0
                            };
                            let cost = pwr_len
                                + (r_idx as f64) * 1.2
                                + (step_dist as f64) * 2.5
                                + br_pen;
                            pairs.push((cost, cd, r_strip, c));
                        }
                    }
                }
                pairs.sort_by(|a, b| a.0.total_cmp(&b.0));

                let mut failed_cd_mask = 0u64;
                for &(_, cd, r_strip, c) in &pairs {
                    if (failed_cd_mask & (1u64 << cd)) != 0
                        || !can_place_pwr_wire(board, pwr_segs, fps, cd, r_strip, c, up, rr)
                    {
                        continue;
                    }
                    let snap_b = *board;
                    let snap_w = pass_wires.snapshot_swaps();
                    board.occupy(cd, r_strip);
                    let is_own = (own_mask & (1u64 << cd)) != 0;
                    let mut pre = Vec::new();
                    if !is_own {
                        let src_c = *own_cols
                            .iter()
                            .min_by_key(|&&oc| (oc as i32 - cd as i32).abs())
                            .unwrap();
                        let Some(routed) = route_same_half(
                            board, pass_wires, fps, tag, src_c, cd, h, true,
                            rem_cross, col_hist, row_hist, weights,
                        ) else {
                            failed_cd_mask |= 1u64 << cd;
                            *board = snap_b;
                            pass_wires.restore_swaps(snap_w);
                            continue;
                        };
                        if !can_place_pwr_wire(board, pwr_segs, fps, cd, r_strip, c, up, rr) {
                            *board = snap_b;
                            pass_wires.restore_swaps(snap_w);
                            continue;
                        }
                        board.col_net[h as usize][cd as usize] = tag;
                        pre = routed;
                    }
                    board.occupy(c, rr);
                    if cd == c {
                        if rr == -2 {
                            board.occupy(c, -1);
                        }
                        if rr == 11 {
                            board.occupy(c, 10);
                        }
                    }
                    pwr_segs.push(cd, r_strip, c, rr);
                    uf.union(root, rail_root);
                    if !is_own {
                        uf.union(strip_idx(cd, h), rail_root);
                    }
                    pre.push(FastWire::new_v(net, cd, r_strip, c, rr));
                    return Some(pre);
                }
                None
            };

        let mut res = try_cand_list(board, &own_cols, 3);
        if res.is_none() {
            let mut empty_cols = Vec::with_capacity(32);
            for c in 1u8..=63 {
                if (own_mask & (1u64 << c)) == 0 && board.col_net[h as usize][c as usize] == 0 {
                    empty_cols.push(c);
                }
            }
            empty_cols.sort_by_key(|&c| {
                let d = own_cols.iter().map(|&oc| (c as i32 - oc as i32).abs()).min().unwrap_or(0);
                let br = if is_potential_bridge_col(board, fps, c, 0, rem_cross) { 18 } else { 0 };
                d + br
            });
            let slice_len = empty_cols.len().min(12);
            res = try_cand_list(board, &empty_cols[..slice_len], 2);
        }
        match res {
            Some(w) => out_wires.extend(w),
            None => {
                let net_str = if net == NetId::VCC { "VCC" } else { "GND" };
                pass_err.push(format!(
                    "{net_str}: không kéo được dây nguồn từ cột {} mà không bị cắt đè",
                    own_cols[0]
                ));
            }
        }
    }
    out_wires
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::steiner::connect_steiner;

    #[test]
    fn test_connect_steiner_and_rail_connect() {
        let mut board = BoardState::new(false);
        board.occupy_default_power_holes();
        let mut pass_wires = PassWires::new(true);
        let mut pwr_segs = PwrSegTracker::new();
        let mut uf = StripUf::new();
        let fps = Vec::new();
        let col_hist = [0.0f32; 65];
        let row_hist = [0.0f32; 10];
        let weights = CostWeights::default();

        // 1. Kiểm tra connect_steiner cho 3 chân cùng mạng tag=3 trên 2 nửa bo
        board.col_net[0][10] = 3;
        board.col_net[0][18] = 3;
        board.col_net[1][15] = 3;
        let w1 = connect_steiner(
            &mut board, &mut pass_wires, &fps, &mut uf, 3, (10, 4), (18, 4),
            18, None, 1, &col_hist, &row_hist, &weights,
        ).unwrap();
        assert!(!w1.is_empty());
        let w2 = connect_steiner(
            &mut board, &mut pass_wires, &fps, &mut uf, 3, (10, 4), (15, 5),
            18, None, 1, &col_hist, &row_hist, &weights,
        ).unwrap();
        assert!(!w2.is_empty());
        assert_eq!(uf.find(strip_idx(18, 0)), uf.find(strip_idx(15, 1)));

        // 2. Kiểm tra rail_connect nhảy thẳng từ cột 10 (nửa trên) lên thanh VCC (hàng -1)
        board.col_net[0][10] = 1;
        let mut pass_err = Vec::new();
        let pw = rail_connect(
            &mut board, &mut pass_wires, &mut pwr_segs, &fps, &mut uf,
            NetId::VCC, 1, &[(10, 4)], 0, &col_hist, &row_hist, &weights, &mut pass_err,
        );
        assert!(pass_err.is_empty());
        assert_eq!(pw.len(), 1);
        assert_eq!(pw[0].r2, -1);
    }
}
