//! Typed field-instance encoding.
//!
//! LDtk loads field values from `realEditorValues` (see
//! `src/electron.renderer/data/inst/FieldInstance.hx` `fromJson`), not from `__value`.
//! Every value is stored as one of four `ValueWrapper` variants
//! (`V_Int`, `V_Float`, `V_Bool`, `V_String`). This module reproduces that encoding
//! and also emits the convenience `__value` for engine consumers.

use serde_json::{json, Value};

use crate::project::Project;

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Int,
    Float,
    Bool,
    String,
    Text,
    Color,
    Enum(String),
    Point,
    Path,
    EntityRef,
    Tile,
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub uid: i64,
    pub identifier: String,
    pub kind: FieldKind,
    pub is_array: bool,
    pub can_be_null: bool,
    pub type_str: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub tileset_uid: Option<i64>,
}

/// Resolved location of an entity instance, used for `EntityRef` `__value`.
pub struct RefInfo {
    pub layer_iid: String,
    pub level_iid: String,
    pub world_iid: String,
}

pub fn parse_field_def(v: &Value) -> Result<FieldDef, String> {
    let identifier = v
        .get("identifier")
        .and_then(Value::as_str)
        .ok_or("field def missing identifier")?
        .to_string();
    let internal = v.get("type").and_then(Value::as_str).unwrap_or("");
    let kind = parse_kind(internal).ok_or_else(|| format!("unsupported field type '{internal}' on '{identifier}'"))?;
    Ok(FieldDef {
        uid: v.get("uid").and_then(Value::as_i64).unwrap_or(0),
        identifier,
        kind,
        is_array: v.get("isArray").and_then(Value::as_bool).unwrap_or(false),
        can_be_null: v.get("canBeNull").and_then(Value::as_bool).unwrap_or(true),
        type_str: v.get("__type").and_then(Value::as_str).unwrap_or("").to_string(),
        min: v.get("min").and_then(Value::as_f64),
        max: v.get("max").and_then(Value::as_f64),
        tileset_uid: v.get("tilesetUid").and_then(Value::as_i64),
    })
}

fn parse_kind(internal: &str) -> Option<FieldKind> {
    Some(match internal {
        "F_Int" => FieldKind::Int,
        "F_Float" => FieldKind::Float,
        "F_Bool" => FieldKind::Bool,
        "F_String" => FieldKind::String,
        "F_Text" => FieldKind::Text,
        "F_Color" => FieldKind::Color,
        "F_Point" => FieldKind::Point,
        "F_Path" => FieldKind::Path,
        "F_EntityRef" => FieldKind::EntityRef,
        "F_Tile" => FieldKind::Tile,
        s if s.starts_with("F_Enum(") => {
            FieldKind::Enum(s.trim_start_matches("F_Enum(").trim_end_matches(')').to_string())
        }
        _ => return None,
    })
}

/// Mirror of `JsonTools.escapeString`: escape backslashes and newlines only.
fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

fn wrap(id: &str, param: Value) -> Value {
    json!({ "id": id, "params": [param] })
}

pub fn hex_to_int(hex: &str) -> Option<i64> {
    let h = hex.trim_start_matches('#');
    i64::from_str_radix(h, 16).ok()
}

pub fn int_to_hex(v: i64) -> String {
    format!("#{:06X}", v & 0xFF_FFFF)
}

impl Project {
    /// Look up a field definition for an entity (by entity identifier) or for levels.
    pub fn entity_field_def(&self, entity_id: &str, field_id: &str) -> Result<FieldDef, String> {
        let ents = self
            .root
            .get("defs")
            .and_then(|d| d.get("entities"))
            .and_then(Value::as_array)
            .ok_or("no entity defs")?;
        let ent = ents
            .iter()
            .find(|e| e.get("identifier").and_then(Value::as_str) == Some(entity_id))
            .ok_or_else(|| format!("entity def '{entity_id}' not found"))?;
        find_field_def(ent.get("fieldDefs"), field_id)
    }

    pub fn level_field_def(&self, field_id: &str) -> Result<FieldDef, String> {
        find_field_def(self.root.get("defs").and_then(|d| d.get("levelFields")), field_id)
    }

    /// Resolve an enum reference, which may be an identifier (e.g. `ItemType`) or, as stored
    /// in the internal `type` field, a uid (e.g. `F_Enum(5)` -> "5").
    fn enum_values(&self, enum_ref: &str) -> Option<Vec<String>> {
        let defs = self.root.get("defs")?;
        let by_uid = enum_ref.parse::<i64>().ok();
        for key in ["enums", "externalEnums"] {
            if let Some(arr) = defs.get(key).and_then(Value::as_array) {
                if let Some(en) = arr.iter().find(|e| {
                    e.get("identifier").and_then(Value::as_str) == Some(enum_ref)
                        || (by_uid.is_some() && e.get("uid").and_then(Value::as_i64) == by_uid)
                }) {
                    return en.get("values").and_then(Value::as_array).map(|vs| {
                        vs.iter()
                            .filter_map(|v| v.get("id").and_then(Value::as_str).map(String::from))
                            .collect()
                    });
                }
            }
        }
        None
    }

    /// Find an entity instance by iid anywhere in the project and return its location.
    pub fn resolve_entity_ref(&self, entity_iid: &str) -> Option<RefInfo> {
        let default_world = self
            .root
            .get("dummyWorldIid")
            .or_else(|| self.root.get("iid"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        // Helper to scan a levels array with a known world iid.
        let scan = |levels: &Value, world_iid: &str| -> Option<RefInfo> {
            for lvl in levels.as_array()? {
                let level_iid = lvl.get("iid").and_then(Value::as_str).unwrap_or("");
                let Some(layers) = lvl.get("layerInstances").and_then(Value::as_array) else {
                    continue;
                };
                for li in layers {
                    let layer_iid = li.get("iid").and_then(Value::as_str).unwrap_or("");
                    if let Some(ents) = li.get("entityInstances").and_then(Value::as_array) {
                        if ents
                            .iter()
                            .any(|e| e.get("iid").and_then(Value::as_str) == Some(entity_iid))
                        {
                            return Some(RefInfo {
                                layer_iid: layer_iid.to_string(),
                                level_iid: level_iid.to_string(),
                                world_iid: world_iid.to_string(),
                            });
                        }
                    }
                }
            }
            None
        };

        if let Some(levels) = self.root.get("levels") {
            if let Some(info) = scan(levels, &default_world) {
                return Some(info);
            }
        }
        if let Some(worlds) = self.root.get("worlds").and_then(Value::as_array) {
            for w in worlds {
                let wiid = w.get("iid").and_then(Value::as_str).unwrap_or(&default_world);
                if let Some(levels) = w.get("levels") {
                    if let Some(info) = scan(levels, wiid) {
                        return Some(info);
                    }
                }
            }
        }
        None
    }

    /// Build a full field-instance JSON object from a typed input value.
    /// `value` is the scalar value, or a JSON array when `def.is_array`.
    pub fn encode_field(&self, def: &FieldDef, value: &Value) -> Result<Value, String> {
        let items: Vec<&Value> = if def.is_array {
            value
                .as_array()
                .ok_or_else(|| format!("field '{}' is an array; expected a JSON array", def.identifier))?
                .iter()
                .collect()
        } else {
            vec![value]
        };

        let mut json_values = Vec::with_capacity(items.len());
        let mut real_editor_values = Vec::with_capacity(items.len());
        for item in items {
            let (jv, rev) = self.encode_one(def, item)?;
            json_values.push(jv);
            real_editor_values.push(rev);
        }

        let value_out = if def.is_array {
            Value::Array(json_values)
        } else {
            json_values.into_iter().next().unwrap_or(Value::Null)
        };

        Ok(json!({
            "__identifier": def.identifier,
            "__type": def.type_str,
            "__value": value_out,
            "__tile": Value::Null,
            "defUid": def.uid,
            "realEditorValues": real_editor_values,
        }))
    }

    /// Encode a single scalar -> (`__value` entry, `realEditorValues` entry).
    fn encode_one(&self, def: &FieldDef, v: &Value) -> Result<(Value, Value), String> {
        if v.is_null() {
            if !def.can_be_null {
                return Err(format!("field '{}' cannot be null", def.identifier));
            }
            return Ok((Value::Null, Value::Null));
        }
        let id = &def.identifier;
        Ok(match &def.kind {
            FieldKind::Int => {
                let mut n = v.as_i64().ok_or_else(|| format!("field '{id}' expects an integer"))?;
                if let Some(mn) = def.min {
                    n = n.max(mn as i64);
                }
                if let Some(mx) = def.max {
                    n = n.min(mx as i64);
                }
                (json!(n), wrap("V_Int", json!(n)))
            }
            FieldKind::Float => {
                let mut f = v.as_f64().ok_or_else(|| format!("field '{id}' expects a number"))?;
                if let Some(mn) = def.min {
                    if f < mn {
                        f = mn;
                    }
                }
                if let Some(mx) = def.max {
                    if f > mx {
                        f = mx;
                    }
                }
                (json!(f), wrap("V_Float", json!(f)))
            }
            FieldKind::Bool => {
                let b = v.as_bool().ok_or_else(|| format!("field '{id}' expects a boolean"))?;
                (json!(b), wrap("V_Bool", json!(b)))
            }
            FieldKind::String | FieldKind::Text | FieldKind::Path => {
                let s = v.as_str().ok_or_else(|| format!("field '{id}' expects a string"))?;
                let esc = escape_string(s);
                (json!(esc), wrap("V_String", json!(esc)))
            }
            FieldKind::Color => {
                let n = match v {
                    Value::String(s) => {
                        hex_to_int(s).ok_or_else(|| format!("field '{id}': invalid hex color '{s}'"))?
                    }
                    Value::Number(_) => v.as_i64().unwrap(),
                    _ => return Err(format!("field '{id}' expects a hex color string")),
                };
                (json!(int_to_hex(n)), wrap("V_Int", json!(n)))
            }
            FieldKind::Enum(name) => {
                let s = v
                    .as_str()
                    .ok_or_else(|| format!("field '{id}' expects an enum value string"))?;
                if let Some(values) = self.enum_values(name) {
                    if !values.contains(&s.to_string()) {
                        return Err(format!(
                            "field '{id}': '{s}' is not a value of enum '{name}' (valid: {values:?})"
                        ));
                    }
                }
                (json!(s), wrap("V_String", json!(escape_string(s))))
            }
            FieldKind::Point => {
                let (cx, cy) =
                    parse_point(v).ok_or_else(|| format!("field '{id}' expects a point {{cx,cy}} or [x,y]"))?;
                let s = format!("{cx},{cy}");
                (json!({ "cx": cx, "cy": cy }), wrap("V_String", json!(s)))
            }
            FieldKind::EntityRef => {
                let iid = v
                    .as_str()
                    .map(String::from)
                    .or_else(|| v.get("entityIid").and_then(Value::as_str).map(String::from))
                    .ok_or_else(|| format!("field '{id}' expects an entity iid string"))?;
                let info = self
                    .resolve_entity_ref(&iid)
                    .ok_or_else(|| format!("field '{id}': entity ref '{iid}' not found in project"))?;
                (
                    json!({
                        "entityIid": iid,
                        "layerIid": info.layer_iid,
                        "levelIid": info.level_iid,
                        "worldIid": info.world_iid,
                    }),
                    wrap("V_String", json!(iid)),
                )
            }
            FieldKind::Tile => {
                let (x, y, w, h) =
                    parse_tile_rect(v).ok_or_else(|| format!("field '{id}' expects a tile rect {{x,y,w,h}}"))?;
                let tileset_uid = v.get("tilesetUid").and_then(Value::as_i64).or(def.tileset_uid);
                let s = format!("{x},{y},{w},{h}");
                (
                    json!({ "tilesetUid": tileset_uid, "x": x, "y": y, "w": w, "h": h }),
                    wrap("V_String", json!(s)),
                )
            }
        })
    }
}

fn find_field_def(field_defs: Option<&Value>, field_id: &str) -> Result<FieldDef, String> {
    let arr = field_defs.and_then(Value::as_array).ok_or("no field definitions")?;
    let fd = arr
        .iter()
        .find(|f| f.get("identifier").and_then(Value::as_str) == Some(field_id))
        .ok_or_else(|| format!("field '{field_id}' not found"))?;
    parse_field_def(fd)
}

fn parse_point(v: &Value) -> Option<(i64, i64)> {
    if let (Some(cx), Some(cy)) = (v.get("cx").and_then(Value::as_i64), v.get("cy").and_then(Value::as_i64)) {
        return Some((cx, cy));
    }
    if let Some(arr) = v.as_array() {
        if arr.len() == 2 {
            return Some((arr[0].as_i64()?, arr[1].as_i64()?));
        }
    }
    if let Some(s) = v.as_str() {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() == 2 {
            return Some((parts[0].trim().parse().ok()?, parts[1].trim().parse().ok()?));
        }
    }
    None
}

fn parse_tile_rect(v: &Value) -> Option<(i64, i64, i64, i64)> {
    let g = |k: &str| v.get(k).and_then(Value::as_i64);
    Some((g("x")?, g("y")?, g("w")?, g("h")?))
}
