//! IDE (item definition) parser — maps a model id to its model name.
//!
//! Binary IPL instances reference objects by numeric **id** only, while collision models are keyed by
//! **name** (their embedded id is a junk tool signature). The IDE files bridge the two: each object
//! definition line begins `id, modelName, txdName, …`. We read that mapping from the object sections
//! (`objs`, `tobj`, `anim`, `lodobj`); other sections (`2dfx`, `path`, `txdp`, …) are ignored.

use std::collections::HashMap;

/// Sections whose lines start with `id, modelName, …`.
const OBJECT_SECTIONS: [&str; 4] = ["objs", "tobj", "anim", "lodobj"];

/// Parse `id → model name` from one IDE file's object sections.
pub fn parse(text: &str) -> Vec<(i32, String)> {
    let mut out = Vec::new();
    let mut in_object_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.eq_ignore_ascii_case("end") {
            in_object_section = false;
            continue;
        }
        // A bare word is a section header. Enter object sections, leave others.
        if !line.contains(',') {
            in_object_section = OBJECT_SECTIONS.iter().any(|s| line.eq_ignore_ascii_case(s));
            continue;
        }
        if !in_object_section {
            continue;
        }
        let mut f = line.split(',').map(str::trim);
        let (Some(id), Some(name)) = (f.next(), f.next()) else {
            continue;
        };
        if let Ok(id) = id.parse::<i32>() {
            out.push((id, name.to_string()));
        }
    }
    out
}

/// Merge many IDE files into one `id → name` map (later definitions win, as the game loads them).
pub fn build_map<I: IntoIterator<Item = (i32, String)>>(defs: I) -> HashMap<i32, String> {
    defs.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_ids_from_object_sections_only() {
        let text = "\
# defs
objs
1700, gen_bench, genbench, 100, 0
1701, gen_lamp, genlamp, 200, 0
end
tobj
2000, night_sign, signtxd, 150, 0, 20, 6
end
2dfx
9999, should_be_ignored
path
1, 2, 3
";
        let defs = parse(text);
        assert_eq!(defs.len(), 3);
        assert_eq!(defs[0], (1700, "gen_bench".to_string()));
        assert_eq!(defs[2], (2000, "night_sign".to_string()));

        let map = build_map(defs);
        assert_eq!(map.get(&1701).map(String::as_str), Some("gen_lamp"));
        assert!(!map.contains_key(&9999));
    }
}
