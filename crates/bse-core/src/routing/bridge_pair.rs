use crate::config::weights::CostWeights;
use crate::discrete_place::decoupling::{base_net_of, is_pwr_tag};
use crate::model::bitboard::BoardState;
use crate::model::geometry::{v_seg_free, Footprint};
use crate::routing::h_route::{
    half_span_load, is_potential_bridge_col, route_h, try_row_swap_h, FastWire, PassWires,
};

/// Định tuyến cùng nửa mặt bo `h`: Đi thẳng -> Row Swap -> Dogleg 1 bước -> Dogleg 2 bước (`routeSameHalf`)
#[allow(clippy::too_many_arguments)]
pub fn route_same_half(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    fps: &[Footprint],
    tag: u16,
    c1: u8,
    c2: u8,
    h: u8,
    allow_swap: bool,
    rem_cross: i32,
    col_hist: &[f32; 65],
    row_hist: &[f32; 10],
    weights: &CostWeights,
) -> Option<Vec<FastWire>> {
    if c1 == c2 {
        return Some(Vec::new());
    }
    if let Some(w) = route_h(board, fps, tag, c1, c2, h, 0, rem_cross, row_hist, weights) {
        return Some(vec![w]);
    }
    if allow_swap {
        if let Some(w) = try_row_swap_h(board, pass_wires, tag, c1, c2, h) {
            return Some(vec![w]);
        }
    }

    let is_pwr = is_pwr_tag(tag) && tag >= 0x4000;
    let mut mids = [0u8; 64];
    let mut m_len = 0usize;
    for cm in 1u8..=63 {
        if cm == c1 || cm == c2 {
            continue;
        }
        let cur = board.col_net[h as usize][cm as usize];
        if is_pwr {
            if cur != 0 {
                continue;
            }
        } else if cur != 0 && cur != tag {
            continue;
        }
        if board.free_hole_count(cm, h) < 2 {
            continue;
        }
        mids[m_len] = cm;
        m_len += 1;
    }
    if m_len == 0 {
        return None;
    }

    let mid_slice = &mut mids[..m_len];
    mid_slice.sort_by(|&x, &y| {
        let same_x = if !is_pwr && board.col_net[h as usize][x as usize] == tag { -8.0 } else { 0.0 };
        let same_y = if !is_pwr && board.col_net[h as usize][y as usize] == tag { -8.0 } else { 0.0 };
        let br_x = if is_potential_bridge_col(board, fps, x, 0, rem_cross) {
            24.0
        } else if board.col_net[(1 - h) as usize][x as usize] == 0 && rem_cross > 0 {
            8.0
        } else {
            0.0
        };
        let br_y = if is_potential_bridge_col(board, fps, y, 0, rem_cross) {
            24.0
        } else if board.col_net[(1 - h) as usize][y as usize] == 0 && rem_cross > 0 {
            8.0
        } else {
            0.0
        };
        let cx = same_x + br_x + ((x as i32 - c1 as i32).abs() + (x as i32 - c2 as i32).abs()) as f64 + (col_hist[x as usize] as f64) * 4.0;
        let cy = same_y + br_y + ((y as i32 - c1 as i32).abs() + (y as i32 - c2 as i32).abs()) as f64 + (col_hist[y as usize] as f64) * 4.0;
        cx.total_cmp(&cy)
    });

    let max_mids = mid_slice.len().min(30);
    for (idx, &cm) in mid_slice[..max_mids].iter().enumerate() {
        let can_swap_mid = allow_swap && idx < 2;
        let mut forbid_mask = 0u16;
        for att in 0..2 {
            let snap_b = *board;
            let snap_w = pass_wires.snapshot_swaps();
            let w1 = route_h(board, fps, tag, c1, cm, h, forbid_mask, rem_cross, row_hist, weights)
                .or_else(|| if can_swap_mid && att == 1 { try_row_swap_h(board, pass_wires, tag, c1, cm, h) } else { None });
            let Some(w1) = w1 else {
                *board = snap_b;
                pass_wires.restore_swaps(snap_w);
                break;
            };
            let w2 = route_h(board, fps, tag, cm, c2, h, 0, rem_cross, row_hist, weights)
                .or_else(|| if can_swap_mid { try_row_swap_h(board, pass_wires, tag, cm, c2, h) } else { None });
            if let Some(w2) = w2 {
                board.col_net[h as usize][cm as usize] = tag;
                return Some(vec![w1, w2]);
            }
            forbid_mask |= 1u16 << w1.r1;
            *board = snap_b;
            pass_wires.restore_swaps(snap_w);
        }
    }

    let top_len = mid_slice.len().min(10);
    for &m1 in &mid_slice[..top_len] {
        let snap1 = *board;
        let Some(w1) = route_h(board, fps, tag, c1, m1, h, 0, rem_cross, row_hist, weights) else {
            *board = snap1;
            continue;
        };
        for &m2 in &mid_slice[..top_len] {
            if m1 == m2 {
                continue;
            }
            let snap2 = *board;
            if let Some(w2) = route_h(board, fps, tag, m1, m2, h, 0, rem_cross, row_hist, weights) {
                if let Some(w3) = route_h(board, fps, tag, m2, c2, h, 0, rem_cross, row_hist, weights) {
                    board.col_net[h as usize][m1 as usize] = tag;
                    board.col_net[h as usize][m2 as usize] = tag;
                    return Some(vec![w1, w2, w3]);
                }
            }
            *board = snap2;
        }
        *board = snap1;
    }
    None
}

/// Nối cặp cột `(c1, h1)` và `(c2, h2)` có san tải 2 nửa bo & cầu rãnh đôi (`connectPair` trong main.html)
#[allow(clippy::too_many_arguments)]
pub fn connect_pair(
    board: &mut BoardState,
    pass_wires: &mut PassWires,
    fps: &[Footprint],
    tag: u16,
    c1: u8,
    h1: u8,
    c2: u8,
    h2: u8,
    rem_cross: i32,
    col_hist: &[f32; 65],
    row_hist: &[f32; 10],
    weights: &CostWeights,
) -> Option<Vec<FastWire>> {
    let w_net = base_net_of(tag);
    if c1 == c2 && h1 == h2 {
        return Some(Vec::new());
    }
    if h1 == h2 {
        if let Some(sh) = route_same_half(board, pass_wires, fps, tag, c1, c2, h1, true, rem_cross, col_hist, row_hist, weights) {
            return Some(sh);
        }
        let oh = 1 - h1;
        let mut b_cols = Vec::with_capacity(32);
        for c in 1u8..=63 {
            if board.is_occupied(c, 4) || board.is_occupied(c, 5) || !v_seg_free(fps, c, 4, 5) {
                continue;
            }
            let (n0, n1) = (board.col_net[0][c as usize], board.col_net[1][c as usize]);
            if (n0 != 0 && n0 != tag) || (n1 != 0 && n1 != tag) || board.free_hole_count(c, oh) < 2 {
                continue;
            }
            b_cols.push(c);
        }
        let mut near1 = b_cols.clone();
        near1.sort_by(|&x, &y| {
            let cx = (x as i32 - c1 as i32).abs() as f64 + (col_hist[x as usize] as f64) * 2.5;
            let cy = (y as i32 - c1 as i32).abs() as f64 + (col_hist[y as usize] as f64) * 2.5;
            cx.total_cmp(&cy)
        });
        near1.truncate(10);

        let mut near2 = b_cols;
        near2.sort_by(|&x, &y| {
            let cx = (x as i32 - c2 as i32).abs() as f64 + (col_hist[x as usize] as f64) * 2.5;
            let cy = (y as i32 - c2 as i32).abs() as f64 + (col_hist[y as usize] as f64) * 2.5;
            cx.total_cmp(&cy)
        });
        near2.truncate(10);

        for (i1, &b1) in near1.iter().enumerate() {
            if b1 != c1 && board.free_hole_count(b1, h1) < 2 {
                continue;
            }
            let snap1_b = *board;
            let snap1_w = pass_wires.snapshot_swaps();
            board.occupy(b1, 4);
            board.occupy(b1, 5);
            board.col_net[0][b1 as usize] = tag;
            board.col_net[1][b1 as usize] = tag;
            let Some(w1) = route_same_half(board, pass_wires, fps, tag, c1, b1, h1, i1 < 2, rem_cross, col_hist, row_hist, weights) else {
                *board = snap1_b;
                pass_wires.restore_swaps(snap1_w);
                continue;
            };
            for (i2, &b2) in near2.iter().enumerate() {
                if b1 == b2
                    || board.is_occupied(b2, 4)
                    || board.is_occupied(b2, 5)
                    || (b2 != c2 && board.free_hole_count(b2, h1) < 2)
                    || board.free_hole_count(b2, oh) < 2
                {
                    continue;
                }
                let snap2_b = *board;
                let snap2_w = pass_wires.snapshot_swaps();
                board.occupy(b2, 4);
                board.occupy(b2, 5);
                board.col_net[0][b2 as usize] = tag;
                board.col_net[1][b2 as usize] = tag;
                if let Some(w_mid) = route_same_half(board, pass_wires, fps, tag, b1, b2, oh, i2 < 2, rem_cross, col_hist, row_hist, weights) {
                    if let Some(w2) = route_same_half(board, pass_wires, fps, tag, b2, c2, h1, i2 < 2, rem_cross, col_hist, row_hist, weights) {
                        let mut out = w1;
                        out.push(FastWire::new_v(w_net, b1, 4, b1, 5));
                        out.extend(w_mid);
                        out.push(FastWire::new_v(w_net, b2, 4, b2, 5));
                        out.extend(w2);
                        return Some(out);
                    }
                }
                *board = snap2_b;
                pass_wires.restore_swaps(snap2_w);
            }
            *board = snap1_b;
            pass_wires.restore_swaps(snap1_w);
        }
        return None;
    }

    let (c_top, c_bot) = if h1 == 0 { (c1, c2) } else { (c2, c1) };
    let mut cols = Vec::with_capacity(48);
    for c in 1u8..=63 {
        if board.is_occupied(c, 4) || board.is_occupied(c, 5) || !v_seg_free(fps, c, 4, 5) {
            continue;
        }
        let (n0, n1) = (board.col_net[0][c as usize], board.col_net[1][c as usize]);
        if (n0 != 0 && n0 != tag) || (n1 != 0 && n1 != tag) {
            continue;
        }
        cols.push(c);
    }
    cols.sort_by(|&x, &y| {
        let cost_of = |c: u8| -> f64 {
            let own = (if board.col_net[0][c as usize] == tag { -38.0 } else { 0.0 })
                + (if board.col_net[1][c as usize] == tag { -38.0 } else { 0.0 });
            let load_top = half_span_load(board, row_hist, 0, c_top, c);
            let load_bot = half_span_load(board, row_hist, 1, c, c_bot);
            own + ((c as i32 - c_top as i32).abs() + (c as i32 - c_bot as i32).abs()) as f64
                + (col_hist[c as usize] as f64) * 4.2
                + (load_top + load_bot) * 2.4
        };
        cost_of(x).total_cmp(&cost_of(y))
    });

    for (idx, &c) in cols.iter().enumerate() {
        if (c != c_top && board.free_hole_count(c, 0) < 2)
            || (c != c_bot && board.free_hole_count(c, 1) < 2)
        {
            continue;
        }
        let use_swap = idx < 5;
        let snap_b = *board;
        let snap_w = pass_wires.snapshot_swaps();
        board.occupy(c, 4);
        board.occupy(c, 5);
        board.col_net[0][c as usize] = tag;
        board.col_net[1][c as usize] = tag;
        if let Some(w_top) = route_same_half(board, pass_wires, fps, tag, c_top, c, 0, use_swap, rem_cross, col_hist, row_hist, weights) {
            if let Some(w_bot) = route_same_half(board, pass_wires, fps, tag, c, c_bot, 1, use_swap, rem_cross, col_hist, row_hist, weights) {
                let mut out = w_top;
                out.push(FastWire::new_v(w_net, c, 4, c, 5));
                out.extend(w_bot);
                return Some(out);
            }
        }
        *board = snap_b;
        pass_wires.restore_swaps(snap_w);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_same_half_dogleg_and_connect_pair_bridge() {
        let mut board = BoardState::new(false);
        let mut pass_wires = PassWires::new(true);
        let fps = vec![Footprint::for_ic(10, 16)];
        let col_hist = [0.0f32; 65];
        let row_hist = [0.0f32; 10];
        let weights = CostWeights::default();

        // Chặn lệch các hàng ở cột 5 và cột 20 để ép nhảy dogleg 2 đoạn qua cột trung gian
        for r in [0, 1, 2, 4] { board.occupy(5, r); }   // Cột 5 chỉ trống hàng 3
        for r in [0, 1, 3, 4] { board.occupy(20, r); }  // Cột 20 chỉ trống hàng 2
        let dogleg = route_same_half(
            &mut board, &mut pass_wires, &fps, 3, 5, 20, 0, true, 0, &col_hist, &row_hist, &weights,
        ).unwrap();
        assert_eq!(dogleg.len(), 2);

        // Nối vượt rãnh E-F từ cột 12 (nửa trên, bị IC 10..16 chắn) sang cột 14 (nửa dưới)
        let cross = connect_pair(
            &mut board, &mut pass_wires, &fps, 4, 12, 0, 14, 1, 1, &col_hist, &row_hist, &weights,
        ).unwrap();
        assert!(cross.iter().any(|w| w.r1 == 4 && w.r2 == 5 && (w.c1 < 10 || w.c1 > 16)));
    }
}
