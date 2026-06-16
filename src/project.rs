//! In-memory LDtk project state.
//!
//! The whole `.ldtk` file is kept as a `serde_json::Value` (with `preserve_order`)
//! so that editor-only fields and key ordering survive a load/save round-trip.
//! We only reach into the parts of the tree we actually need to read or mutate.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

/// A loaded LDtk project plus the path it came from.
pub struct Project {
    pub path: PathBuf,
    pub root: Value,
    pub dirty: bool,
}

/// Resolved info about a layer definition, used when building layer instances.
pub struct LayerDef {
    pub uid: i64,
    pub identifier: String,
    pub kind: String, // IntGrid | Entities | Tiles | AutoLayer
    pub grid_size: i64,
    pub display_opacity: f64,
    pub px_offset_x: i64,
    pub px_offset_y: i64,
    pub tileset_def_uid: Option<i64>,
}

/// Location of a level: either the root `levels` array, or `worlds[w].levels`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelRef {
    Root(usize),
    World(usize, usize),
}

/// Tileset geometry used to map tile ids to pixel coordinates.
pub struct TilesetGeom {
    pub tile_grid_size: i64,
    pub padding: i64,
    pub spacing: i64,
    pub c_wid: i64,
}

/// Resolved info about an entity definition, used when placing entities.
pub struct EntityDef {
    pub uid: i64,
    pub identifier: String,
    pub width: i64,
    pub height: i64,
    pub pivot_x: f64,
    pub pivot_y: f64,
    pub color: String,
    pub tile: Option<Value>,
    pub tags: Vec<String>,
}

impl Project {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let root: Value = serde_json::from_str(&text).with_context(|| format!("parsing JSON in {}", path.display()))?;
        if !root.is_object() {
            bail!("{} is not a valid LDtk project (root is not an object)", path.display());
        }
        let mut proj = Self {
            path,
            root,
            dirty: false,
        };
        proj.merge_external_levels()?;
        Ok(proj)
    }

    fn project_dir(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// For external-level projects, pull each `.ldtkl` body into its in-tree level entry so
    /// all editing happens uniformly in memory. `save` reverses this.
    fn merge_external_levels(&mut self) -> Result<()> {
        if !self.external_levels() {
            return Ok(());
        }
        let dir = self.project_dir();
        for r in self.all_level_refs() {
            let (rel, needs) = {
                let lvl = self.level_ref(r).unwrap();
                let rel = lvl.get("externalRelPath").and_then(Value::as_str).map(String::from);
                let needs = lvl.get("layerInstances").map(Value::is_null).unwrap_or(true);
                (rel, needs)
            };
            if let (Some(rel), true) = (rel, needs) {
                let body_path = dir.join(&rel);
                let text = std::fs::read_to_string(&body_path)
                    .with_context(|| format!("reading external level {}", body_path.display()))?;
                let body: Value = serde_json::from_str(&text)
                    .with_context(|| format!("parsing external level {}", body_path.display()))?;
                let lvl = self.level_value_mut(r)?;
                if let Some(li) = body.get("layerInstances") {
                    lvl["layerInstances"] = li.clone();
                }
                if let Some(fi) = body.get("fieldInstances") {
                    lvl["fieldInstances"] = fi.clone();
                }
            }
        }
        Ok(())
    }

    pub fn save(&mut self) -> Result<()> {
        let minify = self.root.get("minifyJson").and_then(Value::as_bool).unwrap_or(false);
        let to_text = |v: &Value| -> Result<String> {
            Ok(if minify {
                serde_json::to_string(v)?
            } else {
                serde_json::to_string_pretty(v)?
            })
        };

        if self.external_levels() {
            let dir = self.project_dir();
            // Assign external paths to any new levels, then write each .ldtkl body.
            for r in self.all_level_refs() {
                let rel = {
                    let lvl = self.level_value_mut(r)?;
                    let rel = match lvl.get("externalRelPath").and_then(Value::as_str) {
                        Some(p) => p.to_string(),
                        None => {
                            let id = lvl.get("identifier").and_then(Value::as_str).unwrap_or("Level");
                            let rel = format!("{id}.ldtkl");
                            lvl["externalRelPath"] = json!(rel);
                            rel
                        }
                    };
                    rel
                };
                let mut body = self.level_ref(r).unwrap().clone();
                body["externalRelPath"] = Value::Null;
                let body_path = dir.join(&rel);
                if let Some(parent) = body_path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::write(&body_path, to_text(&body)?)
                    .with_context(|| format!("writing external level {}", body_path.display()))?;
            }
            // Main file: clone tree and null out layerInstances for external levels.
            let mut main = self.root.clone();
            null_external_layer_instances(&mut main);
            std::fs::write(&self.path, to_text(&main)?).with_context(|| format!("writing {}", self.path.display()))?;
        } else {
            std::fs::write(&self.path, to_text(&self.root)?)
                .with_context(|| format!("writing {}", self.path.display()))?;
        }
        self.dirty = false;
        Ok(())
    }

    /// The project tree as it would be serialized to the main `.ldtk` file
    /// (external level bodies nulled out). Used for schema validation.
    pub fn main_file_json(&self) -> Value {
        let mut v = self.root.clone();
        if self.external_levels() {
            null_external_layer_instances(&mut v);
        }
        v
    }

    pub fn json_version(&self) -> String {
        self.root
            .get("jsonVersion")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string()
    }

    pub fn world_layout(&self) -> String {
        self.root
            .get("worldLayout")
            .and_then(Value::as_str)
            .unwrap_or("Free")
            .to_string()
    }

    pub fn external_levels(&self) -> bool {
        self.root
            .get("externalLevels")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// Allocate the next unique integer UID and bump `nextUid`.
    pub fn alloc_uid(&mut self) -> Result<i64> {
        let obj = self
            .root
            .as_object_mut()
            .ok_or_else(|| anyhow!("root is not an object"))?;
        let next = obj.get("nextUid").and_then(Value::as_i64).unwrap_or(1);
        obj.insert("nextUid".into(), json!(next + 1));
        Ok(next)
    }

    pub fn new_iid() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn defs(&self) -> Option<&Value> {
        self.root.get("defs")
    }

    pub fn layer_defs(&self) -> Vec<LayerDef> {
        let mut out = Vec::new();
        let Some(layers) = self.defs().and_then(|d| d.get("layers")).and_then(Value::as_array) else {
            return out;
        };
        for l in layers {
            let kind = l
                .get("__type")
                .or_else(|| l.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            // Prefer auto tileset if present, else the plain tileset.
            let tileset_def_uid = l
                .get("autoTilesetDefUid")
                .and_then(Value::as_i64)
                .or_else(|| l.get("tilesetDefUid").and_then(Value::as_i64));
            out.push(LayerDef {
                uid: l.get("uid").and_then(Value::as_i64).unwrap_or(0),
                identifier: l.get("identifier").and_then(Value::as_str).unwrap_or("").to_string(),
                kind: kind.to_string(),
                grid_size: l.get("gridSize").and_then(Value::as_i64).unwrap_or(16),
                display_opacity: l.get("displayOpacity").and_then(Value::as_f64).unwrap_or(1.0),
                px_offset_x: l.get("pxOffsetX").and_then(Value::as_i64).unwrap_or(0),
                px_offset_y: l.get("pxOffsetY").and_then(Value::as_i64).unwrap_or(0),
                tileset_def_uid,
            });
        }
        out
    }

    pub fn entity_defs(&self) -> Vec<EntityDef> {
        let mut out = Vec::new();
        let Some(ents) = self.defs().and_then(|d| d.get("entities")).and_then(Value::as_array) else {
            return out;
        };
        for e in ents {
            out.push(EntityDef {
                uid: e.get("uid").and_then(Value::as_i64).unwrap_or(0),
                identifier: e.get("identifier").and_then(Value::as_str).unwrap_or("").to_string(),
                width: e.get("width").and_then(Value::as_i64).unwrap_or(16),
                height: e.get("height").and_then(Value::as_i64).unwrap_or(16),
                pivot_x: e.get("pivotX").and_then(Value::as_f64).unwrap_or(0.0),
                pivot_y: e.get("pivotY").and_then(Value::as_f64).unwrap_or(0.0),
                color: e.get("color").and_then(Value::as_str).unwrap_or("#FFFFFF").to_string(),
                tile: e.get("tileRect").cloned().filter(|v| !v.is_null()),
                tags: e
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
            });
        }
        out
    }

    /// Geometry of a tileset definition, for converting tile ids to pixel `src` coords.
    pub fn tileset_def(&self, uid: i64) -> Option<TilesetGeom> {
        let tilesets = self.defs()?.get("tilesets")?.as_array()?;
        let t = tilesets
            .iter()
            .find(|t| t.get("uid").and_then(Value::as_i64) == Some(uid))?;
        Some(TilesetGeom {
            tile_grid_size: t.get("tileGridSize").and_then(Value::as_i64).unwrap_or(16),
            padding: t.get("padding").and_then(Value::as_i64).unwrap_or(0),
            spacing: t.get("spacing").and_then(Value::as_i64).unwrap_or(0),
            c_wid: t.get("__cWid").and_then(Value::as_i64).unwrap_or(1).max(1),
        })
    }

    /// Top-left pixel coordinate of a tile id within its tileset image.
    pub fn tile_src(&self, uid: i64, tile_id: i64) -> Option<[i64; 2]> {
        let g = self.tileset_def(uid)?;
        let col = tile_id % g.c_wid;
        let row = tile_id / g.c_wid;
        Some([
            g.padding + col * (g.tile_grid_size + g.spacing),
            g.padding + row * (g.tile_grid_size + g.spacing),
        ])
    }

    pub fn tileset_rel_path(&self, uid: i64) -> Option<String> {
        let tilesets = self.defs()?.get("tilesets")?.as_array()?;
        tilesets
            .iter()
            .find(|t| t.get("uid").and_then(Value::as_i64) == Some(uid))
            .and_then(|t| t.get("relPath").and_then(Value::as_str))
            .map(String::from)
    }

    /// All levels, regardless of multi-world mode (root `levels` or `worlds[].levels`).
    pub fn levels(&self) -> Vec<&Value> {
        if let Some(arr) = self.root.get("levels").and_then(Value::as_array) {
            if !arr.is_empty() {
                return arr.iter().collect();
            }
        }
        let mut out = Vec::new();
        if let Some(worlds) = self.root.get("worlds").and_then(Value::as_array) {
            for w in worlds {
                if let Some(arr) = w.get("levels").and_then(Value::as_array) {
                    out.extend(arr.iter());
                }
            }
        }
        out
    }

    /// All level locations across the root `levels` array and every world.
    pub fn all_level_refs(&self) -> Vec<LevelRef> {
        let mut out = Vec::new();
        if let Some(arr) = self.root.get("levels").and_then(Value::as_array) {
            for i in 0..arr.len() {
                out.push(LevelRef::Root(i));
            }
        }
        if let Some(worlds) = self.root.get("worlds").and_then(Value::as_array) {
            for (wi, w) in worlds.iter().enumerate() {
                if let Some(arr) = w.get("levels").and_then(Value::as_array) {
                    for i in 0..arr.len() {
                        out.push(LevelRef::World(wi, i));
                    }
                }
            }
        }
        out
    }

    /// Find a level by `identifier`, `iid`, or stringified `uid`, across root and worlds.
    pub fn find_level(&self, key: &str) -> Option<LevelRef> {
        self.all_level_refs()
            .into_iter()
            .find(|r| self.level_ref(*r).map(|lvl| level_matches(lvl, key)).unwrap_or(false))
    }

    /// Immutable access to a level by location.
    pub fn level_ref(&self, r: LevelRef) -> Option<&Value> {
        match r {
            LevelRef::Root(i) => self.root.get("levels")?.as_array()?.get(i),
            LevelRef::World(w, i) => self
                .root
                .get("worlds")?
                .as_array()?
                .get(w)?
                .get("levels")?
                .as_array()?
                .get(i),
        }
    }

    /// Immutable access to a layer instance within a level by location.
    pub fn layer_instance_ref(&self, r: LevelRef, layer_id: &str) -> Option<&Value> {
        self.level_ref(r)?.get("layerInstances")?.as_array()?.iter().find(|li| {
            li.get("__identifier").and_then(Value::as_str) == Some(layer_id)
                || li.get("iid").and_then(Value::as_str) == Some(layer_id)
        })
    }

    /// Build an empty layer instance for a given layer def and level dimensions.
    fn empty_layer_instance(&self, def: &LayerDef, level_uid: i64, px_wid: i64, px_hei: i64) -> Value {
        let c_wid = (px_wid as f64 / def.grid_size as f64).ceil() as i64;
        let c_hei = (px_hei as f64 / def.grid_size as f64).ceil() as i64;
        let tileset_rel = def
            .tileset_def_uid
            .and_then(|u| self.tileset_rel_path(u))
            .map(Value::from)
            .unwrap_or(Value::Null);
        let int_grid_csv = if def.kind == "IntGrid" {
            json!(vec![0i64; (c_wid * c_hei).max(0) as usize])
        } else {
            json!([])
        };
        json!({
            "__identifier": def.identifier,
            "__type": def.kind,
            "__cWid": c_wid,
            "__cHei": c_hei,
            "__gridSize": def.grid_size,
            "__opacity": def.display_opacity,
            "__pxTotalOffsetX": def.px_offset_x,
            "__pxTotalOffsetY": def.px_offset_y,
            "__tilesetDefUid": def.tileset_def_uid.map(Value::from).unwrap_or(Value::Null),
            "__tilesetRelPath": tileset_rel,
            "iid": Self::new_iid(),
            "levelId": level_uid,
            "layerDefUid": def.uid,
            "pxOffsetX": 0,
            "pxOffsetY": 0,
            "visible": true,
            "optionalRules": [],
            "intGridCsv": int_grid_csv,
            "autoLayerTiles": [],
            "seed": rng_seed(),
            "overrideTilesetUid": Value::Null,
            "gridTiles": [],
            "entityInstances": []
        })
    }

    /// Decide where a newly created level should live: root, or the first world in
    /// multi-world projects (root `levels` empty but `worlds` present).
    fn create_target_world(&self) -> Option<usize> {
        let root_empty = self
            .root
            .get("levels")
            .and_then(Value::as_array)
            .map(|a| a.is_empty())
            .unwrap_or(true);
        let has_worlds = self
            .root
            .get("worlds")
            .and_then(Value::as_array)
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if root_empty && has_worlds {
            Some(0)
        } else {
            None
        }
    }

    /// Append a new level with empty layer instances built from the current defs.
    pub fn create_level(&mut self, identifier: &str, px_wid: i64, px_hei: i64) -> Result<Value> {
        if self.find_level(identifier).is_some() {
            bail!("a level named '{identifier}' already exists");
        }
        let target_world = self.create_target_world();
        let layout = match target_world {
            Some(w) => self
                .root
                .get("worlds")
                .and_then(Value::as_array)
                .and_then(|a| a.get(w))
                .and_then(|w| w.get("worldLayout"))
                .and_then(Value::as_str)
                .unwrap_or("Free")
                .to_string(),
            None => self.world_layout(),
        };
        let default_bg = self
            .root
            .get("defaultLevelBgColor")
            .and_then(Value::as_str)
            .unwrap_or("#696A79")
            .to_string();

        // Position the new level in world space.
        let (world_x, world_y) = self.next_world_position(px_wid, &layout);

        let level_uid = self.alloc_uid()?;
        let defs = self.layer_defs();
        let layer_instances: Vec<Value> = defs
            .iter()
            .map(|d| self.empty_layer_instance(d, level_uid, px_wid, px_hei))
            .collect();

        let level = json!({
            "identifier": identifier,
            "iid": Self::new_iid(),
            "uid": level_uid,
            "worldX": world_x,
            "worldY": world_y,
            "worldDepth": 0,
            "pxWid": px_wid,
            "pxHei": px_hei,
            "__bgColor": default_bg,
            "bgColor": Value::Null,
            "useAutoIdentifier": false,
            "bgRelPath": Value::Null,
            "bgPos": Value::Null,
            "bgPivotX": 0.5,
            "bgPivotY": 0.5,
            "__smartColor": "#8C8E9B",
            "__bgPos": Value::Null,
            "__neighbours": [],
            "externalRelPath": Value::Null,
            "fieldInstances": [],
            "layerInstances": layer_instances
        });

        match target_world {
            Some(w) => {
                let worlds = self
                    .root
                    .get_mut("worlds")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| anyhow!("no `worlds` array"))?;
                let world = worlds.get_mut(w).ok_or_else(|| anyhow!("world index out of range"))?;
                if !world.get("levels").map(Value::is_array).unwrap_or(false) {
                    world["levels"] = json!([]);
                }
                world["levels"].as_array_mut().unwrap().push(level.clone());
            }
            None => {
                let obj = self
                    .root
                    .as_object_mut()
                    .ok_or_else(|| anyhow!("root is not an object"))?;
                if !obj.contains_key("levels") || !obj["levels"].is_array() {
                    obj.insert("levels".into(), json!([]));
                }
                obj.get_mut("levels")
                    .and_then(Value::as_array_mut)
                    .unwrap()
                    .push(level.clone());
            }
        }
        self.dirty = true;
        Ok(level)
    }

    fn next_world_position(&self, px_wid: i64, layout: &str) -> (i64, i64) {
        match layout {
            "LinearHorizontal" | "LinearVertical" => (-1, -1),
            _ => {
                // Place to the right of the right-most existing level.
                let max_right = self
                    .levels()
                    .iter()
                    .map(|l| {
                        let x = l.get("worldX").and_then(Value::as_i64).unwrap_or(0);
                        let w = l.get("pxWid").and_then(Value::as_i64).unwrap_or(0);
                        x + w
                    })
                    .max()
                    .unwrap_or(0);
                let gap = 16;
                let mut x = if self.levels().is_empty() { 0 } else { max_right + gap };
                if layout == "GridVania" {
                    let g = self
                        .root
                        .get("worldGridWidth")
                        .and_then(Value::as_i64)
                        .unwrap_or(px_wid.max(1));
                    if g > 0 {
                        x = ((x + g - 1) / g) * g;
                    }
                }
                (x, 0)
            }
        }
    }

    /// Mutable access to a level object by location.
    pub fn level_value_mut(&mut self, r: LevelRef) -> Result<&mut Value> {
        match r {
            LevelRef::Root(i) => self
                .root
                .get_mut("levels")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| anyhow!("no `levels` array"))?
                .get_mut(i)
                .ok_or_else(|| anyhow!("level index out of range")),
            LevelRef::World(w, i) => self
                .root
                .get_mut("worlds")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| anyhow!("no `worlds` array"))?
                .get_mut(w)
                .ok_or_else(|| anyhow!("world index out of range"))?
                .get_mut("levels")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| anyhow!("world has no `levels` array"))?
                .get_mut(i)
                .ok_or_else(|| anyhow!("level index out of range")),
        }
    }

    /// The `__identifier` of an entity instance located anywhere in a level.
    pub fn entity_identifier(&self, r: LevelRef, entity_iid: &str) -> Option<String> {
        for li in self.level_ref(r)?.get("layerInstances")?.as_array()? {
            if let Some(ents) = li.get("entityInstances").and_then(Value::as_array) {
                for e in ents {
                    if e.get("iid").and_then(Value::as_str) == Some(entity_iid) {
                        return e.get("__identifier").and_then(Value::as_str).map(String::from);
                    }
                }
            }
        }
        None
    }

    /// Mutable access to an entity instance located anywhere in a level.
    pub fn entity_instance_mut(&mut self, r: LevelRef, entity_iid: &str) -> Result<&mut Value> {
        let level = self.level_value_mut(r)?;
        let layers = level
            .get_mut("layerInstances")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("level has no layerInstances"))?;
        for li in layers {
            if let Some(ents) = li.get_mut("entityInstances").and_then(Value::as_array_mut) {
                if let Some(e) = ents
                    .iter_mut()
                    .find(|e| e.get("iid").and_then(Value::as_str) == Some(entity_iid))
                {
                    return Ok(e);
                }
            }
        }
        bail!("entity instance '{entity_iid}' not found in level")
    }

    /// Mutable access to a specific layer instance inside a level.
    pub fn layer_instance_mut(&mut self, r: LevelRef, layer_id: &str) -> Result<&mut Value> {
        let level = self.level_value_mut(r)?;
        let instances = level
            .get_mut("layerInstances")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("level has no layerInstances"))?;
        instances
            .iter_mut()
            .find(|li| {
                li.get("__identifier").and_then(Value::as_str) == Some(layer_id)
                    || li.get("iid").and_then(Value::as_str) == Some(layer_id)
            })
            .ok_or_else(|| anyhow!("layer '{layer_id}' not found in level"))
    }
}

fn level_matches(lvl: &Value, key: &str) -> bool {
    lvl.get("identifier").and_then(Value::as_str) == Some(key)
        || lvl.get("iid").and_then(Value::as_str) == Some(key)
        || lvl.get("uid").and_then(Value::as_i64).map(|u| u.to_string()).as_deref() == Some(key)
}

/// In a cloned tree destined for the main project file, null out `layerInstances` of any
/// level that is stored externally (has an `externalRelPath`).
fn null_external_layer_instances(root: &mut Value) {
    let null_in = |levels: &mut Value| {
        if let Some(arr) = levels.as_array_mut() {
            for lvl in arr {
                let external = lvl.get("externalRelPath").map(|v| !v.is_null()).unwrap_or(false);
                if external {
                    lvl["layerInstances"] = Value::Null;
                }
            }
        }
    };
    if let Some(levels) = root.get_mut("levels") {
        null_in(levels);
    }
    if let Some(worlds) = root.get_mut("worlds").and_then(Value::as_array_mut) {
        for w in worlds {
            if let Some(levels) = w.get_mut("levels") {
                null_in(levels);
            }
        }
    }
}

/// Cheap non-crypto seed for auto-layer rendering (LDtk just needs *a* number).
fn rng_seed() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 9_999_999) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: Value) -> Project {
        Project {
            path: PathBuf::from("/tmp/test.ldtk"),
            root,
            dirty: false,
        }
    }

    fn sample_defs() -> Value {
        json!({
            "nextUid": 100,
            "worldLayout": "Free",
            "defs": {
                "layers": [
                    { "uid": 1, "identifier": "Collisions", "__type": "IntGrid", "gridSize": 16,
                      "autoTilesetDefUid": 9 },
                    { "uid": 2, "identifier": "Entities", "__type": "Entities", "gridSize": 16 },
                ],
                "entities": [
                    { "uid": 3, "identifier": "Chest", "width": 24, "height": 24,
                      "pivotX": 0.5, "pivotY": 1.0, "color": "#FF0000", "tags": ["loot"] },
                ],
                "tilesets": [
                    { "uid": 9, "identifier": "Tiles", "relPath": "tiles.png",
                      "tileGridSize": 16, "padding": 1, "spacing": 2, "__cWid": 4 },
                ],
            },
            "levels": [],
        })
    }

    #[test]
    fn layer_defs_parsed() {
        let p = project(sample_defs());
        let defs = p.layer_defs();
        assert_eq!(defs.len(), 2);
        let intgrid = &defs[0];
        assert_eq!(intgrid.identifier, "Collisions");
        assert_eq!(intgrid.kind, "IntGrid");
        assert_eq!(intgrid.grid_size, 16);
        // Auto tileset is preferred when present.
        assert_eq!(intgrid.tileset_def_uid, Some(9));
    }

    #[test]
    fn entity_defs_parsed() {
        let p = project(sample_defs());
        let defs = p.entity_defs();
        assert_eq!(defs.len(), 1);
        let chest = &defs[0];
        assert_eq!(chest.identifier, "Chest");
        assert_eq!(chest.width, 24);
        assert_eq!(chest.pivot_y, 1.0);
        assert_eq!(chest.tags, vec!["loot".to_string()]);
    }

    #[test]
    fn tile_src_uses_tileset_geometry() {
        let p = project(sample_defs());
        // c_wid = 4, tile_grid_size = 16, padding = 1, spacing = 2.
        // tile 0 -> col 0,row 0 -> (1, 1)
        assert_eq!(p.tile_src(9, 0), Some([1, 1]));
        // tile 5 -> col 1,row 1 -> padding + col*(16+2) = 1+18 = 19
        assert_eq!(p.tile_src(9, 5), Some([19, 19]));
        assert_eq!(p.tile_src(404, 0), None);
    }

    #[test]
    fn tileset_rel_path_lookup() {
        let p = project(sample_defs());
        assert_eq!(p.tileset_rel_path(9), Some("tiles.png".to_string()));
        assert_eq!(p.tileset_rel_path(0), None);
    }

    #[test]
    fn level_matches_by_identifier_iid_uid() {
        let lvl = json!({ "identifier": "Cave_01", "iid": "abc-123", "uid": 7 });
        assert!(level_matches(&lvl, "Cave_01"));
        assert!(level_matches(&lvl, "abc-123"));
        assert!(level_matches(&lvl, "7"));
        assert!(!level_matches(&lvl, "nope"));
    }

    #[test]
    fn alloc_uid_bumps_next_uid() {
        let mut p = project(sample_defs());
        assert_eq!(p.alloc_uid().unwrap(), 100);
        assert_eq!(p.alloc_uid().unwrap(), 101);
        assert_eq!(p.root.get("nextUid").and_then(Value::as_i64), Some(102));
    }

    #[test]
    fn find_level_and_refs_across_root_and_worlds() {
        let p = project(json!({
            "levels": [{ "identifier": "Root_A", "uid": 1 }],
            "worlds": [{
                "iid": "w1",
                "levels": [{ "identifier": "World_A", "uid": 2 }],
            }],
        }));
        assert_eq!(p.all_level_refs(), vec![LevelRef::Root(0), LevelRef::World(0, 0)]);
        assert_eq!(p.find_level("Root_A"), Some(LevelRef::Root(0)));
        assert_eq!(p.find_level("World_A"), Some(LevelRef::World(0, 0)));
        assert_eq!(p.find_level("missing"), None);
    }

    #[test]
    fn levels_falls_back_to_worlds_when_root_empty() {
        let p = project(json!({
            "levels": [],
            "worlds": [{ "levels": [{ "identifier": "W1" }, { "identifier": "W2" }] }],
        }));
        let ids: Vec<&str> = p
            .levels()
            .iter()
            .filter_map(|l| l.get("identifier").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["W1", "W2"]);
    }

    #[test]
    fn create_level_appends_and_builds_layer_instances() {
        let mut p = project(sample_defs());
        let level = p.create_level("Cave_01", 256, 128).unwrap();
        assert_eq!(level.get("identifier").and_then(Value::as_str), Some("Cave_01"));
        assert_eq!(level.get("pxWid").and_then(Value::as_i64), Some(256));
        assert!(p.dirty);

        let instances = level.get("layerInstances").and_then(Value::as_array).unwrap();
        assert_eq!(instances.len(), 2);
        let intgrid = instances
            .iter()
            .find(|li| li.get("__identifier").and_then(Value::as_str) == Some("Collisions"))
            .unwrap();
        // 256/16 = 16 wide, 128/16 = 8 high -> 128 cells, all zero.
        let csv = intgrid.get("intGridCsv").and_then(Value::as_array).unwrap();
        assert_eq!(csv.len(), 16 * 8);
        assert!(csv.iter().all(|v| v.as_i64() == Some(0)));

        // It is now findable.
        assert!(p.find_level("Cave_01").is_some());
    }

    #[test]
    fn create_level_rejects_duplicate_identifier() {
        let mut p = project(sample_defs());
        p.create_level("Cave_01", 256, 256).unwrap();
        let err = p.create_level("Cave_01", 256, 256).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn create_level_targets_first_world_when_root_empty() {
        let mut p = project(json!({
            "nextUid": 1,
            "levels": [],
            "worlds": [{ "iid": "w1", "worldLayout": "Free", "levels": [] }],
            "defs": { "layers": [], "entities": [], "tilesets": [] },
        }));
        p.create_level("W_Level", 128, 128).unwrap();
        assert_eq!(p.find_level("W_Level"), Some(LevelRef::World(0, 0)));
    }

    #[test]
    fn next_world_position_places_to_right_in_free_layout() {
        let p = project(json!({
            "worldLayout": "Free",
            "levels": [{ "worldX": 0, "pxWid": 256, "pxHei": 256 }],
        }));
        // Right-most edge is 256, plus a 16px gap.
        assert_eq!(p.next_world_position(256, "Free"), (256 + 16, 0));
    }

    #[test]
    fn next_world_position_linear_uses_sentinel() {
        let p = project(json!({ "levels": [] }));
        assert_eq!(p.next_world_position(256, "LinearHorizontal"), (-1, -1));
        assert_eq!(p.next_world_position(256, "LinearVertical"), (-1, -1));
    }

    #[test]
    fn null_external_layer_instances_nulls_only_external() {
        let mut root = json!({
            "levels": [
                { "identifier": "Embedded", "layerInstances": [{ "iid": "a" }] },
                { "identifier": "External", "externalRelPath": "External.ldtkl",
                  "layerInstances": [{ "iid": "b" }] },
            ],
        });
        null_external_layer_instances(&mut root);
        let levels = root.get("levels").and_then(Value::as_array).unwrap();
        assert!(!levels[0].get("layerInstances").unwrap().is_null());
        assert!(levels[1].get("layerInstances").unwrap().is_null());
    }

    #[test]
    fn layer_instance_ref_matches_identifier_or_iid() {
        let p = project(json!({
            "levels": [{
                "identifier": "L",
                "layerInstances": [{ "__identifier": "Collisions", "iid": "layer-iid" }],
            }],
        }));
        let r = p.find_level("L").unwrap();
        assert!(p.layer_instance_ref(r, "Collisions").is_some());
        assert!(p.layer_instance_ref(r, "layer-iid").is_some());
        assert!(p.layer_instance_ref(r, "nope").is_none());
    }
}
