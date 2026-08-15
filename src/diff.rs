//! Semantic diff between two LDtk project roots, for `preview_changes`.
//!
//! Compares an `old` root (the state on disk) against a `new` root (the in-memory edits)
//! and produces a concise, human-readable list of changes rather than a raw JSON patch:
//! levels added/removed/modified, per-layer content-count deltas, and definition changes.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// Content counts for one layer instance:
/// (non-zero IntGrid cells, grid tiles, auto-layer tiles, entity instances).
///
/// Shared with `get_level` so the preview and the level summary never drift.
pub fn layer_counts(li: &Value) -> (usize, usize, usize, usize) {
    let arr_len = |k: &str| li.get(k).and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
    let intgrid_nonzero = li
        .get("intGridCsv")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter(|v| v.as_i64() != Some(0)).count())
        .unwrap_or(0);
    (
        intgrid_nonzero,
        arr_len("gridTiles"),
        arr_len("autoLayerTiles"),
        arr_len("entityInstances"),
    )
}

/// Produce one summary line per change. Empty result means the two roots are equivalent
/// for the purposes we track (levels + defs).
pub fn summarize(old: &Value, new: &Value) -> Vec<String> {
    let mut out = Vec::new();
    diff_levels(old, new, &mut out);
    diff_defs(old, new, &mut out);
    out
}

/// All levels in a root, mirroring `Project::levels`: root `levels` if non-empty, else every
/// world's `levels`.
fn levels_in(root: &Value) -> Vec<&Value> {
    if let Some(arr) = root.get("levels").and_then(Value::as_array) {
        if !arr.is_empty() {
            return arr.iter().collect();
        }
    }
    let mut out = Vec::new();
    if let Some(worlds) = root.get("worlds").and_then(Value::as_array) {
        for w in worlds {
            if let Some(arr) = w.get("levels").and_then(Value::as_array) {
                out.extend(arr.iter());
            }
        }
    }
    out
}

/// Stable match key for a level: `iid` (survives a rename), falling back to `identifier`.
fn level_key(lvl: &Value) -> &str {
    lvl.get("iid")
        .and_then(Value::as_str)
        .or_else(|| lvl.get("identifier").and_then(Value::as_str))
        .unwrap_or("?")
}

fn level_name(lvl: &Value) -> &str {
    lvl.get("identifier").and_then(Value::as_str).unwrap_or("?")
}

fn diff_levels(old: &Value, new: &Value, out: &mut Vec<String>) {
    let old_levels = levels_in(old);
    let new_levels = levels_in(new);
    let old_by: HashMap<&str, &Value> = old_levels.iter().map(|l| (level_key(l), *l)).collect();
    let new_keys: HashSet<&str> = new_levels.iter().map(|l| level_key(l)).collect();

    // Iterate the source vectors (not the maps) so output order is deterministic.
    for l in &new_levels {
        if !old_by.contains_key(level_key(l)) {
            out.push(format!("+ level '{}' added", level_name(l)));
        }
    }
    for l in &old_levels {
        if !new_keys.contains(level_key(l)) {
            out.push(format!("- level '{}' removed", level_name(l)));
        }
    }
    for l in &new_levels {
        if let Some(o) = old_by.get(level_key(l)) {
            diff_level_pair(o, l, out);
        }
    }
}

fn diff_level_pair(o: &Value, n: &Value, out: &mut Vec<String>) {
    let name = level_name(n);
    let oid = o.get("identifier").and_then(Value::as_str).unwrap_or("?");
    let nid = n.get("identifier").and_then(Value::as_str).unwrap_or("?");
    if oid != nid {
        out.push(format!("~ level '{oid}' renamed to '{nid}'"));
    }
    for (key, label) in [("pxWid", "width"), ("pxHei", "height")] {
        let ov = o.get(key).and_then(Value::as_i64);
        let nv = n.get(key).and_then(Value::as_i64);
        if ov != nv {
            out.push(format!(
                "~ level '{name}' {label} {} -> {}",
                ov.unwrap_or(0),
                nv.unwrap_or(0)
            ));
        }
    }
    let pos = |l: &Value| {
        (
            l.get("worldX").and_then(Value::as_i64),
            l.get("worldY").and_then(Value::as_i64),
        )
    };
    if pos(o) != pos(n) {
        let (x, y) = pos(n);
        out.push(format!(
            "~ level '{name}' moved to ({}, {})",
            x.unwrap_or(0),
            y.unwrap_or(0)
        ));
    }
    let fi_count = |l: &Value| {
        l.get("fieldInstances")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0)
    };
    if fi_count(o) != fi_count(n) {
        out.push(format!("~ level '{name}' fields {} -> {}", fi_count(o), fi_count(n)));
    }
    diff_layers(name, o, n, out);
}

fn diff_layers(level: &str, o: &Value, n: &Value, out: &mut Vec<String>) {
    let layer_id = |li: &Value| {
        li.get("__identifier")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    };
    let old_layers = o.get("layerInstances").and_then(Value::as_array);
    let o_by: HashMap<String, &Value> = old_layers
        .map(|a| a.iter().map(|li| (layer_id(li), li)).collect())
        .unwrap_or_default();
    let Some(new_layers) = n.get("layerInstances").and_then(Value::as_array) else {
        return;
    };
    for li in new_layers {
        let id = layer_id(li);
        // New layer instances (e.g. backfilled by a new layer def) are covered by the def diff.
        let Some(oli) = o_by.get(&id) else { continue };
        let (oi, og, oa, oe) = layer_counts(oli);
        let (ni, ng, na, ne) = layer_counts(li);
        let mut parts = Vec::new();
        if oi != ni {
            parts.push(format!("intgrid {oi}->{ni}"));
        }
        if og != ng {
            parts.push(format!("tiles {og}->{ng}"));
        }
        if oa != na {
            parts.push(format!("autotiles {oa}->{na}"));
        }
        if oe != ne {
            parts.push(format!("entities {oe}->{ne}"));
        }
        if !parts.is_empty() {
            out.push(format!("~ {level}/{id}: {}", parts.join(", ")));
        }
    }
}

fn def_list<'a>(root: &'a Value, key: &str) -> Vec<&'a Value> {
    root.get("defs")
        .and_then(|d| d.get(key))
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

fn ident(v: &Value) -> &str {
    v.get("identifier").and_then(Value::as_str).unwrap_or("?")
}

fn diff_defs(old: &Value, new: &Value, out: &mut Vec<String>) {
    for (key, label) in [
        ("layers", "layer"),
        ("entities", "entity"),
        ("enums", "enum"),
        ("tilesets", "tileset"),
        ("levelFields", "level field"),
    ] {
        let o = def_list(old, key);
        let n = def_list(new, key);
        let o_ids: HashSet<&str> = o.iter().map(|d| ident(d)).collect();
        let n_ids: HashSet<&str> = n.iter().map(|d| ident(d)).collect();
        for d in &n {
            if !o_ids.contains(ident(d)) {
                out.push(format!("+ {label} def '{}' added", ident(d)));
            }
        }
        for d in &o {
            if !n_ids.contains(ident(d)) {
                out.push(format!("- {label} def '{}' removed", ident(d)));
            }
        }
    }
    diff_entity_fields(old, new, out);
}

/// Compare per-entity `fieldDefs` for entity defs present in both roots.
fn diff_entity_fields(old: &Value, new: &Value, out: &mut Vec<String>) {
    let field_idents = |d: &Value| -> Vec<String> {
        d.get("fieldDefs")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|f| ident(f).to_string()).collect())
            .unwrap_or_default()
    };
    let old_ents = def_list(old, "entities");
    let o_by: HashMap<&str, &Value> = old_ents.iter().map(|d| (ident(d), *d)).collect();
    for d in &def_list(new, "entities") {
        let Some(od) = o_by.get(ident(d)) else { continue };
        let old_fields: HashSet<String> = field_idents(od).into_iter().collect();
        let new_fields: HashSet<String> = field_idents(d).into_iter().collect();
        for f in field_idents(d) {
            if !old_fields.contains(&f) {
                out.push(format!("+ entity '{}' field '{f}' added", ident(d)));
            }
        }
        for f in field_idents(od) {
            if !new_fields.contains(&f) {
                out.push(format!("- entity '{}' field '{f}' removed", ident(d)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn identical_roots_have_no_diff() {
        let root = json!({
            "levels": [{ "iid": "a", "identifier": "L", "pxWid": 64, "pxHei": 64,
                         "layerInstances": [] }],
            "defs": { "layers": [], "entities": [], "enums": [], "tilesets": [] },
        });
        assert!(summarize(&root, &root).is_empty());
    }

    #[test]
    fn detects_level_add_and_remove() {
        let old = json!({ "levels": [{ "iid": "a", "identifier": "Keep" }] });
        let new = json!({ "levels": [
            { "iid": "a", "identifier": "Keep" },
            { "iid": "b", "identifier": "New" },
        ] });
        let added = summarize(&old, &new);
        assert!(added.iter().any(|s| s.contains("level 'New' added")), "{added:?}");

        let removed = summarize(&new, &old);
        assert!(removed.iter().any(|s| s.contains("level 'New' removed")), "{removed:?}");
    }

    #[test]
    fn detects_layer_content_deltas() {
        let mk = |nonzero: i64, tiles: usize, ents: usize| {
            json!({ "levels": [{
                "iid": "a", "identifier": "L",
                "layerInstances": [{
                    "__identifier": "Collisions",
                    "intGridCsv": [0, nonzero, 0, 0],
                    "gridTiles": vec![json!({}); tiles],
                    "entityInstances": vec![json!({}); ents],
                }],
            }] })
        };
        let out = summarize(&mk(0, 1, 0), &mk(1, 3, 2));
        let line = out.iter().find(|s| s.contains("L/Collisions")).expect("layer line");
        assert!(line.contains("intgrid 0->1"), "{line}");
        assert!(line.contains("tiles 1->3"), "{line}");
        assert!(line.contains("entities 0->2"), "{line}");
    }

    #[test]
    fn detects_rename_resize_and_def_changes() {
        let old = json!({
            "levels": [{ "iid": "a", "identifier": "Old", "pxWid": 64, "pxHei": 64 }],
            "defs": { "entities": [{ "identifier": "Chest", "fieldDefs": [] }] },
        });
        let new = json!({
            "levels": [{ "iid": "a", "identifier": "Renamed", "pxWid": 128, "pxHei": 64 }],
            "defs": { "entities": [
                { "identifier": "Chest", "fieldDefs": [{ "identifier": "hp" }] },
                { "identifier": "Door", "fieldDefs": [] },
            ] },
        });
        let out = summarize(&old, &new);
        assert!(out.iter().any(|s| s.contains("renamed to 'Renamed'")), "{out:?}");
        assert!(out.iter().any(|s| s.contains("width 64 -> 128")), "{out:?}");
        assert!(out.iter().any(|s| s.contains("entity def 'Door' added")), "{out:?}");
        assert!(
            out.iter().any(|s| s.contains("entity 'Chest' field 'hp' added")),
            "{out:?}"
        );
    }
}
