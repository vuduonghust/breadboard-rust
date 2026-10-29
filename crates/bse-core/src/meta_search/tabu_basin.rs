use rustc_hash::FxHashMap;

#[derive(Clone, Copy, Debug)]
pub struct TabuRecord {
    pub visits: u32,
    pub best: f64,
    pub last_imp: u32,
    pub until: u32,
}

/// Sổ Đen Vùng Trũng (`TabuBasin` trong main.html) chống lặp vô tận trên khung IC bị kẹt
#[derive(Clone, Debug, Default)]
pub struct TabuBasin {
    pub map: FxHashMap<u64, TabuRecord>,
    pub ban_count: u32,
}

impl TabuBasin {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ghi nhận kết quả thăm vùng trũng `key`. Trả về `true` nếu vừa kích hoạt lệnh cấm Tabu mới.
    pub fn record(&mut self, key: u64, score: f64, cur_it: u32) -> bool {
        if key == 0 {
            return false;
        }
        let rec = self.map.entry(key).or_insert(TabuRecord {
            visits: 0,
            best: score,
            last_imp: cur_it,
            until: 0,
        });
        rec.visits += 1;
        if score < rec.best {
            rec.best = score;
            rec.last_imp = cur_it;
            rec.until = 0;
            return false;
        }
        if rec.visits >= 24 && cur_it.saturating_sub(rec.last_imp) >= 24 && cur_it > rec.until {
            rec.until = cur_it + 360;
            self.ban_count += 1;
            return true;
        }
        false
    }

    #[inline]
    pub fn is_tabu(&self, key: u64, cur_it: u32) -> bool {
        key != 0 && self.map.get(&key).is_some_and(|rec| cur_it < rec.until)
    }

    /// Khóa cưỡng bức khung IC hiện tại trong 800 vòng (`TabuBasin.forceBan`)
    pub fn force_ban(&mut self, key: u64, cur_it: u32) {
        if key == 0 {
            return;
        }
        self.map.insert(
            key,
            TabuRecord {
                visits: 99,
                best: 0.0,
                last_imp: 0,
                until: cur_it + 800,
            },
        );
        self.ban_count += 1;
    }
}
