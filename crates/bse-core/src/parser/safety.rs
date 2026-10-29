use crate::model::circuit::{CompSpec, ParsedNet};
use crate::model::ids::{CompId, CompType, NetId, PinRef};
use rustc_hash::{FxHashMap, FxHashSet};

#[inline]
fn add_sw_edge(sw_adj: &mut FxHashMap<NetId, Vec<NetId>>, na: Option<NetId>, nb: Option<NetId>) {
    if let (Some(a), Some(b)) = (na, nb) {
        if a != b {
            sw_adj.entry(a).or_default().push(b);
            sw_adj.entry(b).or_default().push(a);
        }
    }
}

/// Kiểm tra toàn diện an toàn mạch điện & lý thuyết mạch (Khớp 100% nửa sau `parseNetlist` trong main.html)
pub fn check_circuit_safety(
    comps: &[CompSpec],
    nets: &[ParsedNet],
    net_of: &FxHashMap<PinRef, NetId>,
    errors: &mut Vec<String>,
    warns: &mut Vec<String>,
) {
    let by_cid: FxHashMap<CompId, &CompSpec> = comps.iter().map(|c| (c.cid, c)).collect();
    let net_by_id: FxHashMap<NetId, &ParsedNet> = nets.iter().map(|n| (n.id, n)).collect();
    let net_name = |nid: NetId| -> &str { net_by_id.get(&nid).map_or("", |n| n.name.as_str()) };

    let mut sw_adj: FxHashMap<NetId, Vec<NetId>> = FxHashMap::default();

    for c in comps {
        let p = |pin: u8| net_of.get(&PinRef { comp: c.cid, pin }).copied();
        if c.pins == 2 {
            if let (Some(n1), Some(n2)) = (p(1), p(2)) {
                if n1 == n2 {
                    errors.push(format!("{}: hai chân 1 và 2 bị nối tắt trong cùng mạng {}", c.id, net_name(n1)));
                }
            }
        }
        if c.comp_type == CompType::BTN2 {
            let (n1, n2) = (p(1), p(2));
            if matches!((n1, n2), (Some(NetId::VCC), Some(NetId::GND)) | (Some(NetId::GND), Some(NetId::VCC))) {
                errors.push(format!("Ngắn mạch VCC và GND khi nhấn nút {}", c.id));
            }
            add_sw_edge(&mut sw_adj, n1, n2);
        } else if c.comp_type == CompType::SW {
            for (pa, pb, pair_lbl) in [(1, 2, "1-2"), (2, 3, "2-3")] {
                let (na, nb) = (p(pa), p(pb));
                if let (Some(a), Some(b)) = (na, nb) {
                    if a == b {
                        errors.push(format!("{}: chân {pair_lbl} của công tắc bị nối tắt sẵn ở mạng {}", c.id, net_name(a)));
                    } else if matches!((a, b), (NetId::VCC, NetId::GND) | (NetId::GND, NetId::VCC)) {
                        errors.push(format!("Ngắn mạch VCC và GND khi gạt công tắc {} (cặp chân {pair_lbl})", c.id));
                    }
                    add_sw_edge(&mut sw_adj, Some(a), Some(b));
                }
            }
        }
        if matches!(c.comp_type, CompType::LED | CompType::C) {
            let (n1, n2) = (p(1), p(2));
            let label = if c.comp_type == CompType::LED { format!("LED {}", c.id) } else { format!("Tụ phân cực {}", c.id) };
            if n1 == Some(NetId::GND) || n2 == Some(NetId::VCC) {
                warns.push(format!("{label} mắc ngược cực tính (Chân 1 [+] đang nối GND hoặc Chân 2 [-] đang nối VCC)"));
            }
            let sw1 = n1.and_then(|n| sw_adj.get(&n)).map_or(&[][..], |v| v.as_slice());
            let sw2 = n2.and_then(|n| sw_adj.get(&n)).map_or(&[][..], |v| v.as_slice());
            if (sw1.contains(&NetId::GND) && (n2 == Some(NetId::VCC) || sw2.contains(&NetId::VCC)))
                || (sw2.contains(&NetId::VCC) && (n1 == Some(NetId::GND) || sw1.contains(&NetId::GND)))
            {
                warns.push(format!("{label} bị cấp ngược cực tính khi đóng công tắc/nút bấm"));
            }
        }
    }

    // Cảnh báo Floating Bounce nếu BTN2/SW nối vào IC mà thiếu điện trở kéo
    for c in comps.iter().filter(|c| matches!(c.comp_type, CompType::BTN2 | CompType::SW)) {
        for pin in 1..=c.pins {
            let Some(&nid) = net_of.get(&PinRef { comp: c.cid, pin }) else { continue; };
            if nid.is_power() { continue; }
            let Some(net_obj) = net_by_id.get(&nid) else { continue; };
            let has_ic = net_obj.pins.iter().any(|pr| by_cid.get(&pr.comp).is_some_and(|x| x.comp_type == CompType::IC));
            let has_r = net_obj.pins.iter().any(|pr| by_cid.get(&pr.comp).is_some_and(|x| x.comp_type == CompType::R));
            if has_ic && !has_r {
                warns.push(format!("{}:{pin} (net {}) nối trực tiếp vào IC mà không thấy Điện trở kéo/hạn dòng (nguy cơ thả nổi Floating Bounce)", c.id, net_obj.name));
            }
        }
    }

    // Đệ quy kiểm tra mọi đèn LED phải đi nối tiếp qua ít nhất 1 điện trở hạn dòng
    fn is_safe_series_resistor_net(
        nid: NetId,
        visited_leds: &mut FxHashSet<CompId>,
        visited_nets: &mut FxHashSet<NetId>,
        net_by_id: &FxHashMap<NetId, &ParsedNet>,
        by_cid: &FxHashMap<CompId, &CompSpec>,
        net_of: &FxHashMap<PinRef, NetId>,
        sw_adj: &FxHashMap<NetId, Vec<NetId>>,
    ) -> bool {
        if nid.is_none() || nid.is_power() || !visited_nets.insert(nid) { return false; }
        let Some(net_obj) = net_by_id.get(&nid) else { return false; };
        let sw_edges = sw_adj.get(&nid).map_or(&[][..], |v| v.as_slice());
        if sw_edges.iter().any(|e| e.is_power()) { return false; }

        let (mut has_r, mut has_direct_src) = (false, false);
        for pr in &net_obj.pins {
            let Some(comp) = by_cid.get(&pr.comp) else { continue; };
            match comp.comp_type {
                CompType::IC => has_direct_src = true,
                CompType::R => {
                    let other_p = PinRef { comp: comp.cid, pin: if pr.pin == 1 { 2 } else { 1 } };
                    if net_of.get(&other_p).is_some_and(|&on| on != nid) { has_r = true; }
                }
                CompType::LED if visited_leds.insert(comp.cid) => {
                    let n1 = net_of.get(&PinRef { comp: comp.cid, pin: 1 }).copied();
                    let n2 = net_of.get(&PinRef { comp: comp.cid, pin: 2 }).copied();
                    let other_net = if n1 == Some(nid) { n2 } else { n1 };
                    if let Some(on) = other_net {
                        if is_safe_series_resistor_net(on, visited_leds, &mut visited_nets.clone(), net_by_id, by_cid, net_of, sw_adj) {
                            has_r = true;
                        }
                    }
                }
                _ => {}
            }
        }
        if !has_r && !has_direct_src && !sw_edges.is_empty() {
            if sw_edges.iter().all(|&to_n| {
                is_safe_series_resistor_net(to_n, &mut visited_leds.clone(), &mut visited_nets.clone(), net_by_id, by_cid, net_of, sw_adj)
            }) {
                has_r = true;
            }
        }
        has_r && !has_direct_src
    }

    for c in comps.iter().filter(|c| c.comp_type == CompType::LED) {
        let n1 = net_of.get(&PinRef { comp: c.cid, pin: 1 }).copied();
        let n2 = net_of.get(&PinRef { comp: c.cid, pin: 2 }).copied();
        if n1.is_none() && n2.is_none() { continue; }
        let ok1 = n1.is_some_and(|id| is_safe_series_resistor_net(id, &mut FxHashSet::from_iter([c.cid]), &mut FxHashSet::default(), &net_by_id, &by_cid, net_of, &sw_adj));
        let ok2 = n2.is_some_and(|id| is_safe_series_resistor_net(id, &mut FxHashSet::from_iter([c.cid]), &mut FxHashSet::default(), &net_by_id, &by_cid, net_of, &sw_adj));
        if !ok1 && !ok2 {
            errors.push(format!("LED {} nối trực tiếp vào nguồn/IC/công tắc mà không đi nối tiếp qua điện trở hạn dòng", c.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::model::circuit::CompSpec;
    use crate::model::ids::{CompId, CompType};
    use crate::parser::parse_netlist;

    #[test]
    fn test_parse_and_safety_checks_match_js() {
        let comps = vec![
            CompSpec::new_ic(CompId(0), "U1", 14, 0),
            CompSpec::new_other(CompId(1), "R1", CompType::R),
            CompSpec::new_other(CompId(2), "L1", CompType::LED),
            CompSpec::new_other(CompId(3), "CC1", CompType::CC),
            CompSpec::new_other(CompId(4), "B1", CompType::BTN2),
        ];
        let txt = "
            VCC U1:14; GND U1:7
            U1:1 -> R1 -> L1 -> GND
            VCC -> U99:1
            *CLK: U1:2 -> U1:3
            U1:14 -> CC1 -> GND
            B1:1 -> U1:4; B1:2 -> GND
        ";
        let parsed = parse_netlist(txt, &comps);
        assert_eq!(parsed.errors.len(), 1); // Bắt đúng lỗi không có linh kiện U99
        assert_eq!(parsed.warns.len(), 1);  // Bắt đúng cảnh báo B1:1 nối thẳng vào U1:4 thiếu trở kéo
        assert_eq!(parsed.cap_ic_hint.get(&CompId(3)), Some(&CompId(0)));
        assert!(parsed.nets.iter().any(|n| n.name == "CLK" && n.is_star));
    }
}
