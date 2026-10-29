use crate::model::circuit::{CompSpec, ParsedNet, ParsedNetlist};
use crate::model::ids::{CompId, CompType, NetId, PinRef};
use crate::parser::safety::check_circuit_safety;
use rustc_hash::FxHashMap;

/// Kiểm tra chuỗi có định dạng mã linh kiện (Chữ cái in hoa ở đầu + Chữ số ở cuối, VD: "U1", "CC12")
#[inline]
pub fn is_comp_id_format(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_uppercase() {
        i += 1;
    }
    if i == 0 || i == bytes.len() {
        return false;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i == bytes.len()
}

/// Tách token dạng `COMP:PIN` (VD: "U1:14" -> Some(("U1", 14)))
#[inline]
pub fn parse_comp_pin(tok: &str) -> Option<(&str, u8)> {
    let (comp, pin_str) = tok.split_once(':')?;
    if !is_comp_id_format(comp) {
        return None;
    }
    Some((comp, pin_str.parse::<u8>().ok()?))
}

/// Chuẩn hóa ký hiệu chân phân cực `A/+` -> `1`, `K/-` -> `2` (`normPinToken` trong main.html)
fn norm_pin_token(tok: &str) -> String {
    if let Some((comp, pin_part)) = tok.split_once(':') {
        if is_comp_id_format(comp) {
            let mapped = match pin_part {
                "A" | "+" => Some("1"),
                "K" | "-" => Some("2"),
                _ if !pin_part.is_empty() && pin_part.bytes().all(|b| b.is_ascii_digit()) => Some(pin_part),
                _ => None,
            };
            if let Some(p) = mapped {
                return format!("{comp}:{p}");
            }
        }
    }
    tok.to_string()
}

/// Nhận diện nhãn tên mạng ở đầu câu lệnh (VD: `*CLK: U1:2 -> U2:3` hoặc `SIG_A = U1:1 U2:2`)
fn extract_net_label<'a>(stmt: &'a str, by_id: &FxHashMap<&str, &CompSpec>) -> (Option<String>, &'a str) {
    let Some(sep_idx) = stmt.find([':', '=']) else { return (None, stmt); };
    let cand_raw = stmt[..sep_idx].trim();
    let rest = stmt[sep_idx + 1..].trim();
    if cand_raw.is_empty() || rest.is_empty() { return (None, stmt); }
    let without_star = cand_raw.strip_prefix('*').unwrap_or(cand_raw);
    let mut chars = without_star.chars();
    let Some(first_ch) = chars.next() else { return (None, stmt); };
    if !(first_ch.is_ascii_alphabetic() || first_ch == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return (None, stmt);
    }
    let cand_upper = cand_raw.to_ascii_uppercase();
    if by_id.contains_key(cand_upper.as_str()) {
        let first_tok = rest.split_whitespace().next().unwrap_or("");
        let first_tok = first_tok.trim_end_matches("->").trim_end_matches('→');
        let is_pin_suffix = matches!(first_tok, "A" | "a" | "K" | "k" | "+" | "-")
            || (!first_tok.is_empty() && first_tok.bytes().all(|b| b.is_ascii_digit()));
        if is_pin_suffix { return (None, stmt); }
    }
    (Some(cand_upper), rest)
}

/// Trích xuất chỉ dẫn `#SYM_LED:` (VD: `#SYM_LED: L1=L2, L3=L4` hoặc `#SYM_LED: L1:L2:L3`) (#10)
fn parse_sym_led_directive(
    raw_line: &str,
    by_id: &FxHashMap<&str, &CompSpec>,
    sym_led_groups: &mut Vec<Vec<CompId>>,
) {
    let upper = raw_line.to_ascii_uppercase();
    let Some(pos) = upper.find("SYM_LED:") else { return };
    let prefix = upper[..pos].trim_end();
    if !prefix.ends_with('#') && !prefix.ends_with("//") && !prefix.is_empty() {
        return;
    }
    let spec = upper[pos + "SYM_LED:".len()..].trim();
    if spec.is_empty() {
        return;
    }
    let chunks: Vec<&str> = if spec.contains([',', ';']) {
        spec.split([',', ';']).collect()
    } else if spec.contains(['=', ':', '-', '/']) {
        spec.split_whitespace().collect()
    } else {
        vec![spec]
    };
    for chunk in chunks {
        let mut grp = Vec::new();
        for tok in chunk.split(['=', ':', '-', '/', ' ', '\t']) {
            let t = tok.trim();
            if let Some(c) = by_id.get(t).filter(|c| c.comp_type == CompType::LED) {
                if !grp.contains(&c.cid) {
                    grp.push(c.cid);
                }
            }
        }
        if grp.len() >= 2 && !sym_led_groups.contains(&grp) {
            sym_led_groups.push(grp);
        }
    }
}

/// Phân tích cú pháp Netlist chuẩn BSE-Netlist V2 & kiểm tra an toàn mạch (`parseNetlist` trong main.html)
pub fn parse_netlist(text: &str, comps: &[CompSpec]) -> ParsedNetlist {
    let mut errors = Vec::new();
    let mut warns = Vec::new();
    let mut cap_ic_hint: FxHashMap<CompId, CompId> = FxHashMap::default();
    let mut sym_led_groups: Vec<Vec<CompId>> = Vec::new();
    let by_id: FxHashMap<&str, &CompSpec> = comps.iter().map(|c| (c.id.as_str(), c)).collect();

    let mut parent: Vec<usize> = Vec::new();
    let mut node_names: Vec<String> = Vec::new();
    let mut node_idx: FxHashMap<String, usize> = FxHashMap::default();

    let mut ensure_node = |name: &str, parent: &mut Vec<usize>, node_names: &mut Vec<String>| -> usize {
        if let Some(&idx) = node_idx.get(name) { return idx; }
        let idx = parent.len();
        parent.push(idx);
        node_names.push(name.to_string());
        node_idx.insert(name.to_string(), idx);
        idx
    };

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x { parent[x] = parent[parent[x]]; x = parent[x]; }
        x
    }

    let mut statements: Vec<(String, String)> = Vec::new();
    for (line_idx, raw_line) in text.lines().enumerate() {
        parse_sym_led_directive(raw_line, &by_id, &mut sym_led_groups);
        let clean = raw_line.split('#').next().unwrap_or("").split("//").next().unwrap_or("").trim();
        if clean.is_empty() { continue; }
        for part in clean.split([';', ',']) {
            let s = part.trim();
            if !s.is_empty() { statements.push((s.to_string(), format!("Dòng {}", line_idx + 1))); }
        }
    }

    for (raw_stmt, ln) in statements {
        let (net_label, stmt) = extract_net_label(&raw_stmt, &by_id);
        let has_arrow = stmt.contains("->") || stmt.contains('→');
        let norm_str = stmt.to_uppercase().replace('→', "->").replace("->", " -> ");
        let stripped: Vec<String> = norm_str
            .split_whitespace()
            .filter(|&t| t != "->" && t != "-")
            .map(norm_pin_token)
            .collect();

        if let Some(stmt_ic) = stripped.iter().find_map(|t| {
            let c = by_id.get(t.split(':').next()?)?;
            (c.comp_type == CompType::IC).then_some(c.cid)
        }) {
            for t in &stripped {
                if let Some(c) = by_id.get(t.split(':').next().unwrap_or("")) {
                    if matches!(c.comp_type, CompType::CC | CompType::C) {
                        cap_ic_hint.entry(c.cid).or_insert(stmt_ic);
                    }
                }
            }
        }

        let is_rev_flow = has_arrow && stripped.len() > 1
            && ((stripped[0] == "GND" && stripped.last().unwrap() != "GND")
                || (stripped.last().unwrap() == "VCC" && stripped[0] != "VCC"));

        let mut expanded: Vec<String> = Vec::new();
        let len = stripped.len();
        for (i, t) in stripped.iter().enumerate() {
            if let Some(c) = by_id.get(t.as_str()) {
                if c.pins == 2 && has_arrow {
                    let rev_pol = is_rev_flow && matches!(c.comp_type, CompType::LED | CompType::C);
                    let (p_in, p_out) = if rev_pol { ("2", "1") } else { ("1", "2") };
                    if i > 0 && i + 1 < len {
                        expanded.push(format!("{t}:{p_in}"));
                        expanded.push(format!("{t}:{p_out}"));
                    } else if i == 0 && len > 1 {
                        expanded.push(format!("{t}:{p_out}"));
                    } else if i + 1 == len && len > 1 {
                        expanded.push(format!("{t}:{p_in}"));
                    } else {
                        expanded.push(t.clone());
                    }
                    continue;
                }
            }
            expanded.push(t.clone());
        }

        let mut segments: Vec<Vec<String>> = Vec::new();
        let mut cur_seg: Vec<String> = Vec::new();
        for cur in expanded {
            if let Some(prev) = cur_seg.last() {
                if has_arrow {
                    if let (Some((c_p, p_p)), Some((c_c, p_c))) = (parse_comp_pin(prev), parse_comp_pin(&cur)) {
                        if c_p == c_c && p_p != p_c && by_id.get(c_p).is_some_and(|c| c.comp_type != CompType::IC) {
                            segments.push(cur_seg);
                            cur_seg = vec![cur];
                            continue;
                        }
                    }
                }
            }
            cur_seg.push(cur);
        }
        if !cur_seg.is_empty() { segments.push(cur_seg); }
        if let Some(lbl) = net_label {
            if let Some(first_seg) = segments.first_mut() { first_seg.insert(0, lbl); }
        }

        for toks in segments {
            if toks.len() < 2 {
                errors.push(format!("{ln}: cụm \"{}\" cần ít nhất 2 nút để nối dây", toks.join(" ")));
                continue;
            }
            let mut ok = true;
            for t in &toks {
                if let Some((c_str, p_num)) = parse_comp_pin(t) {
                    match by_id.get(c_str) {
                        None => { errors.push(format!("{ln}: không có linh kiện {c_str}")); ok = false; }
                        Some(c) if p_num < 1 || p_num > c.pins => {
                            errors.push(format!("{ln}: {t} ngoài dải 1-{}", c.pins));
                            ok = false;
                        }
                        _ => {}
                    }
                } else if by_id.contains_key(t.as_str()) {
                    errors.push(format!("{ln}: {t} thiếu số chân (dạng {t}:1 hoặc dùng mũi tên ->)"));
                    ok = false;
                }
            }
            if !ok { continue; }
            let first_idx = ensure_node(&toks[0], &mut parent, &mut node_names);
            for t in &toks[1..] {
                let tidx = ensure_node(t, &mut parent, &mut node_names);
                let (ra, rb) = (find(&mut parent, first_idx), find(&mut parent, tidx));
                parent[ra] = rb;
            }
        }
    }

    let mut groups: Vec<(usize, Vec<String>)> = Vec::new();
    for i in 0..node_names.len() {
        let r = find(&mut parent, i);
        if let Some(grp) = groups.iter_mut().find(|(root, _)| *root == r) {
            grp.1.push(node_names[i].clone());
        } else {
            groups.push((r, vec![node_names[i].clone()]));
        }
    }

    let (mut nets, mut net_of, mut k, mut next_sig_id) = (Vec::new(), FxHashMap::default(), 0usize, 3u16);
    for (_, g) in groups {
        let (has_v, has_g) = (g.iter().any(|n| n == "VCC"), g.iter().any(|n| n == "GND"));
        if has_v && has_g {
            errors.push("Ngắn mạch trực tiếp: VCC và GND bị nối chung trong cùng một mạng".to_string());
        }
        let bare: Vec<&String> = g.iter().filter(|n| !n.contains(':') && *n != "VCC" && *n != "GND").collect();
        let star_name = bare.iter().find(|n| n.starts_with('*')).copied();
        let clean_named = star_name.or_else(|| bare.first().copied()).map(|s| s.trim_start_matches('*').to_string());
        let name = if has_v { "VCC".into() } else if has_g { "GND".into() } else if let Some(cn) = clean_named.filter(|s| !s.is_empty()) { cn } else { k += 1; format!("NET{k}") };
        let net_id = if name == "VCC" { NetId::VCC } else if name == "GND" { NetId::GND } else { let id = NetId(next_sig_id); next_sig_id += 1; id };
        let is_star = star_name.is_some() || g.iter().any(|n| n.starts_with('*'));
        let (mut pin_strs, mut pins) = (Vec::new(), Vec::new());
        for tok in g {
            if tok.contains(':') {
                if let Some((c_str, p_num)) = parse_comp_pin(&tok) {
                    if let Some(c) = by_id.get(c_str) {
                        let pref = PinRef { comp: c.cid, pin: p_num };
                        pins.push(pref);
                        net_of.insert(pref, net_id);
                    }
                }
                pin_strs.push(tok);
            }
        }
        nets.push(ParsedNet { id: net_id, name, pins, pin_strs, is_star });
    }

    check_circuit_safety(comps, &nets, &net_of, &mut errors, &mut warns);
    ParsedNetlist { errors, warns, nets, net_of, cap_ic_hint, sym_led_groups }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sym_led_directive() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_other(CompId(1), "R1", CompType::R),
            CompSpec::new_other(CompId(2), "R2", CompType::R),
            CompSpec::new_other(CompId(3), "L1", CompType::LED),
            CompSpec::new_other(CompId(4), "L2", CompType::LED),
            CompSpec::new_other(CompId(5), "L3", CompType::LED),
            CompSpec::new_other(CompId(6), "L4", CompType::LED),
        ];
        let txt = "
            #SYM_LED: L1=L2, L3:L4
            VCC U1:14; GND U1:7
            U1:1 -> R1 -> L1 -> GND; U1:2 -> R2 -> L2 -> GND
        ";
        let parsed = parse_netlist(txt, &comps);
        assert_eq!(parsed.sym_led_groups, vec![vec![CompId(3), CompId(4)], vec![CompId(5), CompId(6)]]);
    }
}
