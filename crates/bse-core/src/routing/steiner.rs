use crate::config::weights::CostWeights;
use crate::discrete_place::decoupling::{base_net_of, is_pwr_tag};
use crate::model::bitboard::{half_of, BoardState};
use crate::model::geometry::{v_seg_free, Footprint};
use crate::routing::bridge_pair::connect_pair;
use crate::routing::h_route::{
    get_available_track_with_btn_rule, is_potential_bridge_col, route_h, span_congestion,
    try_row_swap_h_ext, FastWire, PassWires,
};

/// Ánh xạ dải 5 lỗ `(col, half)` sang chỉ số phẳng `0..128` trên Stack
#[inline(always)]
pub const fn strip_idx(col: u8, half: u8) -> u8 {
    ((half & 1) << 6) | (col & 63)
}

/// Cấu trúc Union-Find siêu tốc trên Stack (258 bytes, `Copy`, không cấp phát Heap)
/// - `0..128`: Các dải 5 lỗ `(col, half)` trên mặt Breadboard
/// - `128..255`: Các nút gốc ảo cho từng nhánh nguồn `RAIL_${tag}`
#[derive(Clone, Copy, Debug)]
pub struct StripUf {
    parent: [u8; 256],
    next_virtual: u8,
}

impl Default for StripUf {
    fn default() -> Self {
        Self::new()
    }
}

impl StripUf {
    #[inline(always)]
    pub const fn new() -> Self {
        let mut parent = [0u8; 256];
        let mut i = 0;
        while i < 256 {
            parent[i] = i as u8;
            i += 1;
        }
        Self { parent, next_virtual: 128 }
    }

    #[inline(always)]
    pub fn alloc_virtual_root(&mut self) -> u8 {
        let r = self.next_virtual;
        self.next_virtual = self.next_virtual.saturating_add(1);
        r
    }

    #[inline(always)]
    pub fn find(&mut self, mut x: u8) -> u8 {
        while self.parent[x as usize] != x {
            let p = self.parent[x as usize];
            self.parent[x as usize] = self.parent[p as usize];
            x = p;
        }
        x
    }

    #[inline(always)]
    pub fn union(&mut self, x: u8, y: u8) {
        let rx = self.find(x);
        let ry = self.find(y);
        self.parent[rx as usize] = ry;
    }
}

/// Dò đường mê cung BFS 2D trên 126 dải đồng (63 cột x 2 nửa bo) làm phao cứu sinh cuối cùng cho 1–2 dây kẹt (#9)
#[allow(clippy::too_many_arguments)]
pub fn bfs_escape_route(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    fps: &[Footprint],
    uf: &mut StripUf,
    tag: u16,
    a: (u8, i8),
    b: (u8, i8),
    rem_cross: i32,
    row_hist: &[f32; 10],
    weights: &CostWeights,
) -> Option<Vec<FastWire>> {
    let ka = strip_idx(a.0, half_of(a.1));
    let kb = strip_idx(b.0, half_of(b.1));
    let (ra, rb) = (uf.find(ka), uf.find(kb));
    if ra == rb {
        return Some(Vec::new());
    }
    let (ha, hb) = (half_of(a.1), half_of(b.1));
    let is_cross_branch = ha != hb;
    let is_pwr = is_pwr_tag(tag) && tag >= 0x4000;
    let w_net = base_net_of(tag);

    let mut in_start = [false; 128];
    let mut in_target = [false; 128];
    let (mut has_start, mut has_target) = (false, false);

    for h in [0u8, 1u8] {
        for c in 1u8..=63 {
            if board.col_net[h as usize][c as usize] != tag || !board.has_free_hole(c, h) {
                continue;
            }
            let k = strip_idx(c, h);
            let rk = uf.find(k);
            if rk == ra || (is_cross_branch && h == ha) {
                in_start[k as usize] = true;
                has_start = true;
            } else if rk == rb || (is_cross_branch && h == hb) {
                in_target[k as usize] = true;
                has_target = true;
            }
        }
    }
    if !has_start || !has_target {
        return None;
    }

    let mut forbidden_mid = [0u64; 2];
    for _attempt in 0..4 {
        let mut dist = [u8::MAX; 128];
        let mut prev = [0u8; 128];
        let mut in_row = [-1i8; 128];
        let mut queue = [0u8; 128];
        let (mut q_head, mut q_tail) = (0usize, 0usize);

        for k in 1u8..128 {
            if in_start[k as usize] {
                dist[k as usize] = 0;
                queue[q_tail] = k;
                q_tail += 1;
            }
        }

        let mut found_end = None;
        while q_head < q_tail {
            let u = queue[q_head];
            q_head += 1;
            if in_target[u as usize] {
                found_end = Some(u);
                break;
            }
            let d = dist[u as usize];
            if d >= 4 {
                continue;
            }
            let uc = u & 63;
            let uh = (u >> 6) & 1;
            let u_in_r = in_row[u as usize];

            // 1. Nhảy dọc qua cầu rãnh E-F tại cùng cột uc
            let vh = 1 - uh;
            let v_br = strip_idx(uc, vh);
            let (r_u, r_v) = if uh == 0 { (4i8, 5i8) } else { (5i8, 4i8) };
            if dist[v_br as usize] == u8::MAX
                && u_in_r != r_u
                && !board.is_occupied(uc, 4)
                && !board.is_occupied(uc, 5)
                && v_seg_free(fps, uc, 4, 5)
            {
                let cur_v = board.col_net[vh as usize][uc as usize];
                let v_is_tgt = in_target[v_br as usize];
                let v_ok = if v_is_tgt {
                    true
                } else if (forbidden_mid[vh as usize] & (1u64 << uc)) != 0 {
                    false
                } else if is_pwr {
                    cur_v == 0 && board.free_hole_count(uc, vh) >= 2
                } else {
                    (cur_v == 0 || cur_v == tag) && board.free_hole_count(uc, vh) >= 2
                };
                if v_ok && q_tail < 128 {
                    dist[v_br as usize] = d + 1;
                    prev[v_br as usize] = u;
                    in_row[v_br as usize] = r_v;
                    queue[q_tail] = v_br;
                    q_tail += 1;
                }
            }

            // 2. Nhảy ngang trên cùng nửa bo uh (ưu tiên khoảng cách gần & né cột cầu rãnh khi rem_cross > 0)
            let rows = if uh == 0 { [3i8, 2, 1, 0, 4] } else { [6i8, 7, 8, 9, 5] };
            let mut cands = [(0u16, 0u8, 0i8); 64];
            let mut c_len = 0usize;
            for vc in 1u8..=63 {
                if vc == uc {
                    continue;
                }
                let v = strip_idx(vc, uh);
                if dist[v as usize] != u8::MAX {
                    continue;
                }
                let v_is_tgt = in_target[v as usize];
                let cur_v = board.col_net[uh as usize][vc as usize];
                if !v_is_tgt {
                    if (forbidden_mid[uh as usize] & (1u64 << vc)) != 0 {
                        continue;
                    }
                    if is_pwr {
                        if cur_v != 0 || board.free_hole_count(vc, uh) < 2 {
                            continue;
                        }
                    } else if (cur_v != 0 && cur_v != tag) || board.free_hole_count(vc, uh) < 2 {
                        continue;
                    }
                }
                let (c_min, c_max) = (uc.min(vc), uc.max(vc));
                let Some(&r_hit) = rows.iter().find(|&&r| {
                    r != u_in_r
                        && !board.is_occupied(uc, r)
                        && !board.is_occupied(vc, r)
                        && get_available_track_with_btn_rule(board, fps, r, c_min, c_max, 0).is_some()
                }) else {
                    continue;
                };
                let br_pen = if !v_is_tgt && is_potential_bridge_col(board, fps, vc, tag, rem_cross) {
                    20u16
                } else {
                    0u16
                };
                let pri = (uc.abs_diff(vc) as u16) + br_pen;
                cands[c_len] = (pri, v, r_hit);
                c_len += 1;
            }
            cands[..c_len].sort_unstable_by_key(|x| x.0);
            for &(_, v, r_hit) in &cands[..c_len] {
                if dist[v as usize] == u8::MAX && q_tail < 128 {
                    dist[v as usize] = d + 1;
                    prev[v as usize] = u;
                    in_row[v as usize] = r_hit;
                    queue[q_tail] = v;
                    q_tail += 1;
                }
            }
        }

        let Some(end_k) = found_end else {
            break;
        };
        let mut path = [0u8; 8];
        let mut p_len = 0usize;
        let mut cur = end_k;
        while cur != 0 && p_len < 8 {
            path[p_len] = cur;
            p_len += 1;
            if in_start[cur as usize] {
                break;
            }
            cur = prev[cur as usize];
        }
        path[..p_len].reverse();
        if p_len < 2 {
            break;
        }

        let snap_b = *board;
        let snap_w = pass_wires.snapshot_swaps();
        let mut out_wires = Vec::with_capacity(p_len);
        let mut ok = true;
        let mut fail_mid = path[1];

        for i in 0..(p_len - 1) {
            let (u, v) = (path[i], path[i + 1]);
            let (uc, uh) = (u & 63, (u >> 6) & 1);
            let (vc, vh) = (v & 63, (v >> 6) & 1);
            if uc == vc && uh != vh {
                if board.is_occupied(uc, 4) || board.is_occupied(uc, 5) || !v_seg_free(fps, uc, 4, 5) {
                    ok = false;
                    fail_mid = v;
                    break;
                }
                board.occupy(uc, 4);
                board.occupy(uc, 5);
                board.col_net[0][uc as usize] = tag;
                board.col_net[1][uc as usize] = tag;
                out_wires.push(FastWire::new_v(w_net, uc, 4, uc, 5));
            } else if uh == vh && uc != vc {
                let w_opt = route_h(board, fps, tag, uc, vc, uh, 0, rem_cross, row_hist, weights)
                    .or_else(|| try_row_swap_h_ext(board, pass_wires, fps, tag, uc, vc, uh, weights));
                if let Some(w) = w_opt {
                    board.col_net[uh as usize][uc as usize] = tag;
                    board.col_net[uh as usize][vc as usize] = tag;
                    out_wires.push(w);
                } else {
                    ok = false;
                    fail_mid = if i + 1 < p_len - 1 { v } else { u };
                    break;
                }
            } else {
                ok = false;
                break;
            }
        }

        if ok {
            let first_k = path[0];
            for &k in &path[1..p_len] {
                uf.union(first_k, k);
            }
            return Some(out_wires);
        }
        *board = snap_b;
        pass_wires.restore_swaps(snap_w);
        let (fc, fh) = (fail_mid & 63, (fail_mid >> 6) & 1);
        forbidden_mid[fh as usize] |= 1u64 << fc;
    }
    None
}

/// Nối hai cụm chân thuộc cùng một mạng theo cây Steiner (`connectSteiner` trong main.html)
#[allow(clippy::too_many_arguments)]
pub fn connect_steiner(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    fps: &[Footprint],
    uf: &mut StripUf,
    tag: u16,
    a: (u8, i8),
    b: (u8, i8),
    max_cand: usize,
    star_hub: Option<(u8, u8)>,
    rem_cross: i32,
    col_hist: &[f32; 65],
    row_hist: &[f32; 10],
    weights: &CostWeights,
) -> Option<Vec<FastWire>> {
    let ka = strip_idx(a.0, half_of(a.1));
    let kb = strip_idx(b.0, half_of(b.1));
    if uf.find(ka) == uf.find(kb) {
        return Some(Vec::new());
    }
    if let Some((hc, hh)) = star_hub {
        let hub_k = strip_idx(hc, hh);
        if let Some(res) = connect_pair(
            board, pass_wires, fps, tag, hc, hh, b.0, half_of(b.1),
            rem_cross, col_hist, row_hist, weights,
        ) {
            uf.union(hub_k, kb);
            return Some(res);
        }
        return bfs_escape_route(
            board, pass_wires, fps, uf, tag, (hc, if hh == 0 { 2 } else { 7 }), b,
            rem_cross, row_hist, weights,
        );
    }

    let (ha, hb) = (half_of(a.1), half_of(b.1));
    let is_cross_branch = ha != hb;
    if is_cross_branch {
        let mut top_roots = [0u128; 2];
        for c in 1u8..=63 {
            if board.col_net[0][c as usize] == tag {
                let r = uf.find(strip_idx(c, 0)) as usize;
                top_roots[r >> 7] |= 1u128 << (r & 127);
            }
        }
        for c in 1u8..=63 {
            if board.col_net[1][c as usize] == tag {
                let r = uf.find(strip_idx(c, 1)) as usize;
                if (top_roots[r >> 7] & (1u128 << (r & 127))) != 0 {
                    return Some(Vec::new());
                }
            }
        }
    }

    let try_connect_roots = |board: &mut BoardState,
                             pass_wires: &mut PassWires,
                             uf: &mut StripUf,
                             root_a: u8,
                             root_b: Option<u8>,
                             lim: usize,
                             exclude_root: Option<u8>|
     -> Option<Vec<FastWire>> {
        let mut comp_a = Vec::with_capacity(16);
        let mut comp_b = Vec::with_capacity(16);
        for h in [0u8, 1u8] {
            for c in 1u8..=63 {
                if board.col_net[h as usize][c as usize] != tag || !board.has_free_hole(c, h) {
                    continue;
                }
                let k = strip_idx(c, h);
                let rk = uf.find(k);
                if rk == root_a {
                    comp_a.push((c, h, k));
                } else if root_b.map_or(rk != root_a && Some(rk) != exclude_root, |rb| rk == rb) {
                    comp_b.push((c, h, k));
                } else if is_cross_branch && root_b.is_some() {
                    // Hạng mục #9: Mở khóa thu thập toàn bộ chân cùng mạng còn lỗ trống trên từng nửa bo khi is_cross_branch == true
                    if h == ha {
                        comp_a.push((c, h, k));
                    } else if h == hb {
                        comp_b.push((c, h, k));
                    }
                }
            }
        }
        if comp_a.is_empty() || comp_b.is_empty() {
            return None;
        }

        let mut pairs = Vec::with_capacity(comp_a.len() * comp_b.len());
        for &u in &comp_a {
            for &v in &comp_b {
                let same_col_bonus = if u.0 == v.0
                    && u.1 != v.1
                    && !board.is_occupied(u.0, 4)
                    && !board.is_occupied(u.0, 5)
                {
                    -45.0
                } else {
                    0.0
                };
                let main_cluster_bonus = (if uf.find(u.2) == root_a { -2.0 } else { 0.0 })
                    + (if root_b.is_some_and(|rb| uf.find(v.2) == rb) { -2.0 } else { 0.0 });
                let dist = (u.0 as i32 - v.0 as i32).abs() as f64
                    + (if u.1 != v.1 { 42.0 } else { 0.0 })
                    + same_col_bonus
                    + main_cluster_bonus
                    + span_congestion(col_hist, u.0, v.0) * 2.2
                    - (if u.2 == ka { 0.5 } else { 0.0 })
                    - (if v.2 == kb { 0.5 } else { 0.0 });
                pairs.push((dist, u, v));
            }
        }
        pairs.sort_by(|x, y| x.0.total_cmp(&y.0));

        for &(_, u, v) in pairs.iter().take(lim) {
            let mut before_mask = [0u64; 2];
            for h in [0usize, 1usize] {
                for c in 1u8..=63 {
                    if board.col_net[h][c as usize] == tag {
                        before_mask[h] |= 1u64 << c;
                    }
                }
            }
            if let Some(res) = connect_pair(
                board, pass_wires, fps, tag, u.0, u.1, v.0, v.1,
                rem_cross, col_hist, row_hist, weights,
            ) {
                uf.union(u.2, v.2);
                for h in [0u8, 1u8] {
                    for c in 1u8..=63 {
                        if board.col_net[h as usize][c as usize] == tag {
                            let k = strip_idx(c, h);
                            if (before_mask[h as usize] & (1u64 << c)) == 0 || k == u.2 || k == v.2
                            {
                                uf.union(u.2, k);
                            }
                        }
                    }
                }
                return Some(res);
            }
        }
        None
    };

    let (ra, rb) = (uf.find(ka), uf.find(kb));
    if let Some(res) = try_connect_roots(board, pass_wires, uf, ra, Some(rb), max_cand, None) {
        return Some(res);
    }

    for (start_k, end_k) in [(ka, kb), (kb, ka)] {
        let snap_b = *board;
        let snap_w = pass_wires.snapshot_swaps();
        let snap_uf = *uf;
        let r_start = uf.find(start_k);
        let r_end = uf.find(end_k);
        if let Some(mut step1) =
            try_connect_roots(board, pass_wires, uf, r_start, None, max_cand.min(18), Some(r_end))
        {
            if uf.find(ka) == uf.find(kb) {
                return Some(step1);
            }
            let (ra2, rb2) = (uf.find(ka), uf.find(kb));
            if let Some(step2) =
                try_connect_roots(board, pass_wires, uf, ra2, Some(rb2), max_cand, None)
            {
                if uf.find(ka) == uf.find(kb) {
                    step1.extend(step2);
                    return Some(step1);
                }
            }
        }
        *board = snap_b;
        pass_wires.restore_swaps(snap_w);
        *uf = snap_uf;
    }

    // Phao cứu sinh cuối cùng (#9): Dò đường mê cung đa nhịp trên 126 dải đồng
    bfs_escape_route(board, pass_wires, fps, uf, tag, a, b, rem_cross, row_hist, weights)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bfs_escape_route_solves_blocked_maze_and_unlocks_cross_branch_pins() {
        let mut board = BoardState::new(false);
        let mut pass_wires = PassWires::new(true);
        let mut uf = StripUf::new();
        let fps = vec![Footprint::for_ic(8, 26)];
        let row_hist = [0.0f32; 10];
        let weights = CostWeights::default();

        // Mạng tag=5 có (10, 4) (đã hết lỗ trống ở cột 10 nửa trên) và (12, 4) (chỉ còn đúng 1 lỗ trống ở hàng 2)
        // Nhờ nâng cấp #9, (12, 0) được mở khóa làm điểm xuất phát vượt rãnh sang (25, 1) dù chỉ còn 1 lỗ trống!
        board.col_net[0][10] = 5;
        board.col_net[0][12] = 5;
        board.col_net[1][25] = 5;
        for r in 0..=4 {
            board.occupy(10, r);
            if r != 2 {
                board.occupy(12, r);
            }
        }
        // Khóa rãnh E-F dưới chân IC (8..=26) để cột 6 là cột cầu rãnh E-F duy nhất trên toàn mặt bo
        for c in 8u8..=26 {
            board.occupy(c, 4);
            board.occupy(c, 5);
        }
        for c in 1u8..=63 {
            if c != 6 && c != 12 && c != 20 && c != 25 {
                board.col_net[0][c as usize] = 99;
                board.col_net[1][c as usize] = 99;
            }
        }
        // Trên nửa dưới: cột 6 chỉ trống hàng 6, cột 25 chỉ trống hàng 7 -> ép đi qua trung gian cột 20:
        // Đường đi: (12,0) --hàng 2--> (6,0) --cầu rãnh E-F (6,4..5)--> (6,1) --hàng 6--> (20,1) --hàng 7--> (25,1)
        for r in [7i8, 8, 9] {
            board.occupy(6, r);
        }
        for r in [6i8, 8, 9] {
            board.occupy(25, r);
        }

        let col_hist = [0.0f32; 65];
        let wires = connect_steiner(
            &mut board, &mut pass_wires, &fps, &mut uf, 5, (10, 4), (25, 5),
            20, None, 1, &col_hist, &row_hist, &weights,
        )
        .expect("Should find multi-hop path via bridge col 6");

        assert!(wires.len() >= 3);
        assert!(wires.iter().any(|w| w.c1 == 6 && w.c2 == 6 && w.r1 == 4 && w.r2 == 5));
        assert_eq!(uf.find(strip_idx(12, 0)), uf.find(strip_idx(25, 1)));
    }
}
