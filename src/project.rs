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
    /// Relative `.ldtkl` paths whose levels were deleted in memory; unlinked on `save`.
    pub pending_external_deletes: Vec<String>,
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
            pending_external_deletes: Vec::new(),
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
        // Remove .ldtkl bodies for levels deleted in memory (best-effort; ignore missing).
        if !self.pending_external_deletes.is_empty() {
            let dir = self.project_dir();
            for rel in self.pending_external_deletes.drain(..) {
                std::fs::remove_file(dir.join(&rel)).ok();
            }
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

    /// All layer instances of type `Entities` within a level.
    pub fn entity_layer_instances(&self, r: LevelRef) -> Vec<&Value> {
        self.level_ref(r)
            .and_then(|lvl| lvl.get("layerInstances"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|li| li.get("__type").and_then(Value::as_str) == Some("Entities"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Immutable access to an entity instance located anywhere in a level.
    pub fn entity_instance_ref(&self, r: LevelRef, entity_iid: &str) -> Option<&Value> {
        for li in self.level_ref(r)?.get("layerInstances")?.as_array()? {
            if let Some(ents) = li.get("entityInstances").and_then(Value::as_array) {
                if let Some(e) = ents
                    .iter()
                    .find(|e| e.get("iid").and_then(Value::as_str) == Some(entity_iid))
                {
                    return Some(e);
                }
            }
        }
        None
    }

    /// The `intGridValues` array from the layer definition matching `layer_id`
    /// (by `identifier`), describing what each IntGrid number means.
    pub fn intgrid_value_defs(&self, layer_id: &str) -> Vec<Value> {
        self.defs()
            .and_then(|d| d.get("layers"))
            .and_then(Value::as_array)
            .and_then(|layers| {
                layers
                    .iter()
                    .find(|l| l.get("identifier").and_then(Value::as_str) == Some(layer_id))
            })
            .and_then(|l| l.get("intGridValues"))
            .and_then(Value::as_array)
            .map(|vals| {
                vals.iter()
                    .map(|v| {
                        json!({
                            "value": v.get("value"),
                            "identifier": v.get("identifier"),
                            "color": v.get("color"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
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

    /// The world layout governing a level's container (root project or its world).
    fn layout_for(&self, r: LevelRef) -> String {
        match r {
            LevelRef::Root(_) => self.world_layout(),
            LevelRef::World(w, _) => self
                .root
                .get("worlds")
                .and_then(Value::as_array)
                .and_then(|ws| ws.get(w))
                .and_then(|wv| wv.get("worldLayout"))
                .and_then(Value::as_str)
                .unwrap_or("Free")
                .to_string(),
        }
    }

    /// Append a level into the same container (root or world) that `r` points into.
    fn push_level(&mut self, r: LevelRef, level: Value) -> Result<()> {
        match r {
            LevelRef::Root(_) => {
                self.root
                    .get_mut("levels")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| anyhow!("no `levels` array"))?
                    .push(level);
            }
            LevelRef::World(w, _) => {
                self.root
                    .get_mut("worlds")
                    .and_then(Value::as_array_mut)
                    .and_then(|ws| ws.get_mut(w))
                    .and_then(|wv| wv.get_mut("levels"))
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| anyhow!("world has no `levels` array"))?
                    .push(level);
            }
        }
        Ok(())
    }

    /// Drop any `__neighbours` entries pointing at a removed level iid, across all levels.
    fn remove_neighbour_refs(&mut self, iid: &str) {
        fn strip(levels: &mut Value, iid: &str) {
            if let Some(arr) = levels.as_array_mut() {
                for lvl in arr {
                    if let Some(ns) = lvl.get_mut("__neighbours").and_then(Value::as_array_mut) {
                        ns.retain(|n| n.get("levelIid").and_then(Value::as_str) != Some(iid));
                    }
                }
            }
        }
        if let Some(levels) = self.root.get_mut("levels") {
            strip(levels, iid);
        }
        if let Some(worlds) = self.root.get_mut("worlds").and_then(Value::as_array_mut) {
            for w in worlds {
                if let Some(levels) = w.get_mut("levels") {
                    strip(levels, iid);
                }
            }
        }
    }

    /// Delete a level by identifier/iid/uid. Returns the deleted level's iid.
    /// For external-level projects, the `.ldtkl` body is unlinked on the next `save`.
    pub fn delete_level(&mut self, key: &str) -> Result<String> {
        let r = self.find_level(key).ok_or_else(|| anyhow!("level '{key}' not found"))?;
        let (iid, ext_rel) = {
            let lvl = self.level_ref(r).unwrap();
            (
                lvl.get("iid").and_then(Value::as_str).unwrap_or("").to_string(),
                lvl.get("externalRelPath").and_then(Value::as_str).map(String::from),
            )
        };
        match r {
            LevelRef::Root(i) => {
                self.root
                    .get_mut("levels")
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| anyhow!("no `levels` array"))?
                    .remove(i);
            }
            LevelRef::World(w, i) => {
                self.root
                    .get_mut("worlds")
                    .and_then(Value::as_array_mut)
                    .and_then(|ws| ws.get_mut(w))
                    .and_then(|wv| wv.get_mut("levels"))
                    .and_then(Value::as_array_mut)
                    .ok_or_else(|| anyhow!("world has no `levels` array"))?
                    .remove(i);
            }
        }
        if self.external_levels() {
            if let Some(rel) = ext_rel {
                self.pending_external_deletes.push(rel);
            }
        }
        if !iid.is_empty() {
            self.remove_neighbour_refs(&iid);
        }
        self.dirty = true;
        Ok(iid)
    }

    /// Duplicate a level into the same container, with fresh uid/iid(s) and a free position.
    pub fn duplicate_level(&mut self, src_key: &str, identifier: Option<&str>) -> Result<Value> {
        let r = self
            .find_level(src_key)
            .ok_or_else(|| anyhow!("level '{src_key}' not found"))?;
        let src_id = self
            .level_ref(r)
            .unwrap()
            .get("identifier")
            .and_then(Value::as_str)
            .unwrap_or("Level")
            .to_string();
        let new_id = match identifier {
            Some(s) => {
                if self.find_level(s).is_some() {
                    bail!("a level named '{s}' already exists");
                }
                s.to_string()
            }
            None => {
                let mut candidate = format!("{src_id}_copy");
                let mut n = 2;
                while self.find_level(&candidate).is_some() {
                    candidate = format!("{src_id}_copy{n}");
                    n += 1;
                }
                candidate
            }
        };
        let layout = self.layout_for(r);
        let px_wid = self
            .level_ref(r)
            .unwrap()
            .get("pxWid")
            .and_then(Value::as_i64)
            .unwrap_or(256);
        let (world_x, world_y) = self.next_world_position(px_wid, &layout);
        let new_uid = self.alloc_uid()?;

        let mut lvl = self.level_ref(r).unwrap().clone();
        lvl["identifier"] = json!(new_id);
        lvl["iid"] = json!(Self::new_iid());
        lvl["uid"] = json!(new_uid);
        lvl["worldX"] = json!(world_x);
        lvl["worldY"] = json!(world_y);
        lvl["__neighbours"] = json!([]);
        lvl["externalRelPath"] = Value::Null;
        if let Some(insts) = lvl.get_mut("layerInstances").and_then(Value::as_array_mut) {
            for li in insts {
                li["iid"] = json!(Self::new_iid());
                li["levelId"] = json!(new_uid);
            }
        }
        self.push_level(r, lvl.clone())?;
        self.dirty = true;
        Ok(lvl)
    }

    /// Set a level's world-space pixel position.
    pub fn move_level(&mut self, key: &str, world_x: i64, world_y: i64) -> Result<()> {
        let r = self.find_level(key).ok_or_else(|| anyhow!("level '{key}' not found"))?;
        let lvl = self.level_value_mut(r)?;
        lvl["worldX"] = json!(world_x);
        lvl["worldY"] = json!(world_y);
        self.dirty = true;
        Ok(())
    }

    /// Resize a level, reflowing every layer instance and clipping out-of-bounds content.
    pub fn resize_level(&mut self, key: &str, px_wid: i64, px_hei: i64) -> Result<()> {
        if px_wid <= 0 || px_hei <= 0 {
            bail!("level dimensions must be positive (got {px_wid}x{px_hei})");
        }
        let r = self.find_level(key).ok_or_else(|| anyhow!("level '{key}' not found"))?;
        let lvl = self.level_value_mut(r)?;
        lvl["pxWid"] = json!(px_wid);
        lvl["pxHei"] = json!(px_hei);
        if let Some(insts) = lvl.get_mut("layerInstances").and_then(Value::as_array_mut) {
            for li in insts {
                resize_layer_instance(li, px_wid, px_hei);
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Find a world index by `identifier` or `iid`.
    pub fn find_world(&self, key: &str) -> Option<usize> {
        self.root.get("worlds").and_then(Value::as_array)?.iter().position(|w| {
            w.get("identifier").and_then(Value::as_str) == Some(key)
                || w.get("iid").and_then(Value::as_str) == Some(key)
        })
    }

    /// Append a new (empty) world to the root `worlds` array, creating the array if needed.
    #[allow(clippy::too_many_arguments)]
    pub fn create_world(
        &mut self,
        identifier: &str,
        world_layout: Option<&str>,
        world_grid_width: Option<i64>,
        world_grid_height: Option<i64>,
        default_level_width: Option<i64>,
        default_level_height: Option<i64>,
    ) -> Result<Value> {
        if self.find_world(identifier).is_some() {
            bail!("a world named '{identifier}' already exists");
        }
        let layout = world_layout.unwrap_or("Free");
        let default_w = default_level_width
            .or_else(|| self.root.get("defaultLevelWidth").and_then(Value::as_i64))
            .unwrap_or(256);
        let default_h = default_level_height
            .or_else(|| self.root.get("defaultLevelHeight").and_then(Value::as_i64))
            .unwrap_or(256);
        let world = json!({
            "iid": Self::new_iid(),
            "identifier": identifier,
            "worldLayout": layout,
            "worldGridWidth": world_grid_width.unwrap_or(256),
            "worldGridHeight": world_grid_height.unwrap_or(256),
            "defaultLevelWidth": default_w,
            "defaultLevelHeight": default_h,
            "levels": [],
        });
        let obj = self
            .root
            .as_object_mut()
            .ok_or_else(|| anyhow!("root is not an object"))?;
        if !obj.get("worlds").map(Value::is_array).unwrap_or(false) {
            obj.insert("worlds".into(), json!([]));
        }
        obj.get_mut("worlds")
            .and_then(Value::as_array_mut)
            .unwrap()
            .push(world.clone());
        self.dirty = true;
        Ok(world)
    }

    /// Update a world's layout and/or grid dimensions.
    pub fn set_world_layout(
        &mut self,
        world_key: &str,
        world_layout: Option<&str>,
        world_grid_width: Option<i64>,
        world_grid_height: Option<i64>,
    ) -> Result<()> {
        let idx = self
            .find_world(world_key)
            .ok_or_else(|| anyhow!("world '{world_key}' not found"))?;
        let world = self
            .root
            .get_mut("worlds")
            .and_then(Value::as_array_mut)
            .and_then(|ws| ws.get_mut(idx))
            .ok_or_else(|| anyhow!("world index out of range"))?;
        if let Some(layout) = world_layout {
            world["worldLayout"] = json!(layout);
        }
        if let Some(gw) = world_grid_width {
            world["worldGridWidth"] = json!(gw);
        }
        if let Some(gh) = world_grid_height {
            world["worldGridHeight"] = json!(gh);
        }
        self.dirty = true;
        Ok(())
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

    /// Move an existing entity instance to a new grid cell, recomputing its pixel
    /// position from the entity definition's pivot (matching `place_entities`).
    pub fn move_entity(&mut self, r: LevelRef, entity_iid: &str, cx: i64, cy: i64) -> Result<()> {
        // Snapshot pivots before taking the mutable borrow of the tree.
        let defs = self.entity_defs();
        let mut found = false;
        {
            let level = self.level_value_mut(r)?;
            let layers = level
                .get_mut("layerInstances")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| anyhow!("level has no layerInstances"))?;
            'outer: for li in layers {
                let grid = li.get("__gridSize").and_then(Value::as_i64).unwrap_or(16);
                if let Some(ents) = li.get_mut("entityInstances").and_then(Value::as_array_mut) {
                    if let Some(e) = ents
                        .iter_mut()
                        .find(|e| e.get("iid").and_then(Value::as_str) == Some(entity_iid))
                    {
                        let id = e.get("__identifier").and_then(Value::as_str).unwrap_or("");
                        let (pivot_x, pivot_y) = defs
                            .iter()
                            .find(|d| d.identifier == id)
                            .map(|d| (d.pivot_x, d.pivot_y))
                            .unwrap_or((0.0, 0.0));
                        let px_x = (cx as f64 * grid as f64 + pivot_x * grid as f64).round() as i64;
                        let px_y = (cy as f64 * grid as f64 + pivot_y * grid as f64).round() as i64;
                        e["__grid"] = json!([cx, cy]);
                        e["px"] = json!([px_x, px_y]);
                        found = true;
                        break 'outer;
                    }
                }
            }
        }
        if found {
            self.dirty = true;
            Ok(())
        } else {
            bail!("entity instance '{entity_iid}' not found in level")
        }
    }

    /// Remove a single entity instance (by iid) from anywhere in a level.
    pub fn delete_entity(&mut self, r: LevelRef, entity_iid: &str) -> Result<()> {
        let mut removed = false;
        {
            let level = self.level_value_mut(r)?;
            let layers = level
                .get_mut("layerInstances")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| anyhow!("level has no layerInstances"))?;
            for li in layers {
                if let Some(ents) = li.get_mut("entityInstances").and_then(Value::as_array_mut) {
                    let before = ents.len();
                    ents.retain(|e| e.get("iid").and_then(Value::as_str) != Some(entity_iid));
                    if ents.len() != before {
                        removed = true;
                        break;
                    }
                }
            }
        }
        if removed {
            self.dirty = true;
            Ok(())
        } else {
            bail!("entity instance '{entity_iid}' not found in level")
        }
    }

    /// 4-connected flood fill on an IntGrid layer starting at `(cx, cy)`, replacing the
    /// contiguous region sharing the start cell's value. Returns the number of cells filled.
    pub fn flood_fill_intgrid(&mut self, r: LevelRef, layer_id: &str, cx: i64, cy: i64, value: i64) -> Result<usize> {
        let filled;
        {
            let li = self.layer_instance_mut(r, layer_id)?;
            if li.get("__type").and_then(Value::as_str) != Some("IntGrid") {
                bail!("layer '{layer_id}' is not an IntGrid layer");
            }
            let cw = li.get("__cWid").and_then(Value::as_i64).unwrap_or(0);
            let ch = li.get("__cHei").and_then(Value::as_i64).unwrap_or(0);
            if cx < 0 || cy < 0 || cx >= cw || cy >= ch {
                bail!("start cell ({cx},{cy}) is out of bounds for {cw}x{ch}");
            }
            let mut grid: Vec<i64> = li
                .get("intGridCsv")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|v| v.as_i64().unwrap_or(0)).collect())
                .unwrap_or_default();
            let total = (cw * ch).max(0) as usize;
            if grid.len() != total {
                bail!("intGridCsv length {} != {cw}x{ch} = {total}", grid.len());
            }
            let target = grid[(cy * cw + cx) as usize];
            let mut count = 0usize;
            if target != value {
                let mut stack = vec![(cx, cy)];
                while let Some((x, y)) = stack.pop() {
                    if x < 0 || y < 0 || x >= cw || y >= ch {
                        continue;
                    }
                    let i = (y * cw + x) as usize;
                    if grid[i] != target {
                        continue;
                    }
                    grid[i] = value;
                    count += 1;
                    stack.push((x + 1, y));
                    stack.push((x - 1, y));
                    stack.push((x, y + 1));
                    stack.push((x, y - 1));
                }
                li["intGridCsv"] = json!(grid);
                li["autoLayerTiles"] = json!([]);
            }
            filled = count;
        }
        if filled > 0 {
            self.dirty = true;
        }
        Ok(filled)
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

/// Reflow a single layer instance to new level pixel dimensions, clipping content that
/// falls outside the new bounds.
fn resize_layer_instance(li: &mut Value, px_wid: i64, px_hei: i64) {
    let grid = li.get("__gridSize").and_then(Value::as_i64).unwrap_or(16).max(1);
    let old_cw = li.get("__cWid").and_then(Value::as_i64).unwrap_or(0);
    let old_ch = li.get("__cHei").and_then(Value::as_i64).unwrap_or(0);
    let new_cw = ((px_wid as f64 / grid as f64).ceil() as i64).max(0);
    let new_ch = ((px_hei as f64 / grid as f64).ceil() as i64).max(0);
    let kind = li.get("__type").and_then(Value::as_str).unwrap_or("").to_string();

    // IntGrid: rebuild the CSV, preserving the overlapping top-left region; clear the
    // generated AutoLayer tiles so LDtk regenerates them on load.
    if kind == "IntGrid" {
        let old: Vec<i64> = li
            .get("intGridCsv")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|v| v.as_i64().unwrap_or(0)).collect())
            .unwrap_or_default();
        let mut grid_csv = vec![0i64; (new_cw * new_ch).max(0) as usize];
        let copy_w = old_cw.min(new_cw);
        let copy_h = old_ch.min(new_ch);
        for y in 0..copy_h {
            for x in 0..copy_w {
                let oi = (y * old_cw + x) as usize;
                let ni = (y * new_cw + x) as usize;
                if oi < old.len() && ni < grid_csv.len() {
                    grid_csv[ni] = old[oi];
                }
            }
        }
        li["intGridCsv"] = json!(grid_csv);
        li["autoLayerTiles"] = json!([]);
    }

    // Clip tile arrays by pixel bounds and recompute the coord-id `d` for the new width.
    for tiles_key in ["gridTiles", "autoLayerTiles"] {
        if let Some(tiles) = li.get_mut(tiles_key).and_then(Value::as_array_mut) {
            tiles.retain(|t| {
                t.get("px")
                    .and_then(Value::as_array)
                    .map(|p| {
                        let x = p.first().and_then(Value::as_i64).unwrap_or(0);
                        let y = p.get(1).and_then(Value::as_i64).unwrap_or(0);
                        x >= 0 && y >= 0 && x < px_wid && y < px_hei
                    })
                    .unwrap_or(false)
            });
            for t in tiles.iter_mut() {
                let (x, y) = t
                    .get("px")
                    .and_then(Value::as_array)
                    .map(|p| {
                        (
                            p.first().and_then(Value::as_i64).unwrap_or(0),
                            p.get(1).and_then(Value::as_i64).unwrap_or(0),
                        )
                    })
                    .unwrap_or((0, 0));
                t["d"] = json!([(y / grid) * new_cw + (x / grid)]);
            }
        }
    }

    // Clip entity instances whose grid cell now falls outside the level.
    if let Some(ents) = li.get_mut("entityInstances").and_then(Value::as_array_mut) {
        ents.retain(|e| {
            e.get("__grid")
                .and_then(Value::as_array)
                .map(|g| {
                    let cx = g.first().and_then(Value::as_i64).unwrap_or(0);
                    let cy = g.get(1).and_then(Value::as_i64).unwrap_or(0);
                    cx >= 0 && cy >= 0 && cx < new_cw && cy < new_ch
                })
                .unwrap_or(true)
        });
    }

    li["__cWid"] = json!(new_cw);
    li["__cHei"] = json!(new_ch);
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
            pending_external_deletes: Vec::new(),
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
    fn entity_instance_ref_finds_by_iid() {
        let p = project(json!({
            "levels": [{
                "identifier": "L",
                "layerInstances": [
                    { "__type": "IntGrid", "__identifier": "Collisions", "entityInstances": [] },
                    { "__type": "Entities", "__identifier": "Entities", "entityInstances": [
                        { "iid": "ent-1", "__identifier": "Chest" },
                    ] },
                ],
            }],
        }));
        let r = p.find_level("L").unwrap();
        let e = p.entity_instance_ref(r, "ent-1").expect("found");
        assert_eq!(e.get("__identifier").and_then(Value::as_str), Some("Chest"));
        assert!(p.entity_instance_ref(r, "missing").is_none());
    }

    #[test]
    fn entity_layer_instances_filters_to_entities() {
        let p = project(json!({
            "levels": [{
                "identifier": "L",
                "layerInstances": [
                    { "__type": "IntGrid", "__identifier": "Collisions" },
                    { "__type": "Entities", "__identifier": "GameEntities" },
                    { "__type": "Entities", "__identifier": "Markers" },
                ],
            }],
        }));
        let r = p.find_level("L").unwrap();
        let ids: Vec<&str> = p
            .entity_layer_instances(r)
            .iter()
            .filter_map(|li| li.get("__identifier").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["GameEntities", "Markers"]);
    }

    #[test]
    fn intgrid_value_defs_reads_layer_def() {
        let p = project(json!({
            "defs": {
                "layers": [{
                    "identifier": "Collisions",
                    "__type": "IntGrid",
                    "intGridValues": [
                        { "value": 1, "identifier": "wall", "color": "#000000" },
                        { "value": 2, "identifier": "water", "color": "#0000FF" },
                    ],
                }],
            },
        }));
        let vals = p.intgrid_value_defs("Collisions");
        assert_eq!(vals.len(), 2);
        assert_eq!(vals[0].get("identifier").and_then(Value::as_str), Some("wall"));
        assert_eq!(vals[1].get("value").and_then(Value::as_i64), Some(2));
        assert!(p.intgrid_value_defs("Nope").is_empty());
    }

    #[test]
    fn delete_level_removes_and_records_external() {
        let mut p = project(json!({
            "externalLevels": true,
            "levels": [
                { "identifier": "A", "iid": "iid-a", "externalRelPath": "A.ldtkl",
                  "__neighbours": [] },
                { "identifier": "B", "iid": "iid-b", "externalRelPath": "B.ldtkl",
                  "__neighbours": [{ "dir": "w", "levelIid": "iid-a" }] },
            ],
        }));
        let iid = p.delete_level("A").unwrap();
        assert_eq!(iid, "iid-a");
        // A is gone, only B remains.
        assert_eq!(p.find_level("A"), None);
        assert!(p.find_level("B").is_some());
        // External body queued for deletion.
        assert_eq!(p.pending_external_deletes, vec!["A.ldtkl".to_string()]);
        // Dangling neighbour ref to A removed from B.
        let b = p.level_ref(p.find_level("B").unwrap()).unwrap();
        assert_eq!(b.get("__neighbours").and_then(Value::as_array).unwrap().len(), 0);
    }

    #[test]
    fn duplicate_level_gets_fresh_ids_and_position() {
        let mut p = project(json!({
            "nextUid": 50,
            "worldLayout": "Free",
            "levels": [{
                "identifier": "Room", "iid": "iid-room", "uid": 10,
                "worldX": 0, "worldY": 0, "pxWid": 256, "pxHei": 256,
                "__neighbours": [{ "dir": "e", "levelIid": "other" }],
                "layerInstances": [{ "__identifier": "L", "iid": "li-old", "levelId": 10 }],
            }],
        }));
        let dup = p.duplicate_level("Room", None).unwrap();
        assert_eq!(dup.get("identifier").and_then(Value::as_str), Some("Room_copy"));
        assert_eq!(dup.get("uid").and_then(Value::as_i64), Some(50));
        assert_ne!(dup.get("iid").and_then(Value::as_str), Some("iid-room"));
        // Repositioned to the right of the original (256 + 16 gap).
        assert_eq!(dup.get("worldX").and_then(Value::as_i64), Some(256 + 16));
        // Neighbours cleared, layer instance got a new iid + levelId.
        assert_eq!(dup.get("__neighbours").and_then(Value::as_array).unwrap().len(), 0);
        let li = &dup.get("layerInstances").and_then(Value::as_array).unwrap()[0];
        assert_ne!(li.get("iid").and_then(Value::as_str), Some("li-old"));
        assert_eq!(li.get("levelId").and_then(Value::as_i64), Some(50));
        // Both levels now exist.
        assert!(p.find_level("Room").is_some());
        assert!(p.find_level("Room_copy").is_some());
    }

    #[test]
    fn duplicate_level_rejects_existing_identifier() {
        let mut p = project(json!({
            "nextUid": 1,
            "levels": [
                { "identifier": "A", "uid": 1, "pxWid": 64, "pxHei": 64, "layerInstances": [] },
                { "identifier": "B", "uid": 2, "pxWid": 64, "pxHei": 64, "layerInstances": [] },
            ],
        }));
        let err = p.duplicate_level("A", Some("B")).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn move_level_sets_world_position() {
        let mut p = project(json!({
            "levels": [{ "identifier": "A", "worldX": 0, "worldY": 0 }],
        }));
        p.move_level("A", 320, -64).unwrap();
        let a = p.level_ref(p.find_level("A").unwrap()).unwrap();
        assert_eq!(a.get("worldX").and_then(Value::as_i64), Some(320));
        assert_eq!(a.get("worldY").and_then(Value::as_i64), Some(-64));
    }

    #[test]
    fn resize_level_expands_and_shrinks_intgrid_with_clipping() {
        let mut p = project(json!({
            "levels": [{
                "identifier": "A", "pxWid": 32, "pxHei": 32,
                "layerInstances": [{
                    "__identifier": "Collisions", "__type": "IntGrid",
                    "__cWid": 2, "__cHei": 2, "__gridSize": 16,
                    // row-major 2x2: [1,2,3,4]
                    "intGridCsv": [1, 2, 3, 4], "autoLayerTiles": [{ "px": [0, 0] }],
                    "gridTiles": [], "entityInstances": [],
                }],
            }],
        }));
        // Expand to 3x2 cells (48x32px): top-left 2x2 preserved, new cells 0.
        p.resize_level("A", 48, 32).unwrap();
        let li = &p.level_ref(p.find_level("A").unwrap()).unwrap()["layerInstances"][0];
        assert_eq!(li.get("__cWid").and_then(Value::as_i64), Some(3));
        assert_eq!(li.get("__cHei").and_then(Value::as_i64), Some(2));
        let csv: Vec<i64> = li["intGridCsv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        // new width 3: row0 = [1,2,0], row1 = [3,4,0]
        assert_eq!(csv, vec![1, 2, 0, 3, 4, 0]);
        // AutoLayer tiles cleared on IntGrid resize.
        assert_eq!(li["autoLayerTiles"].as_array().unwrap().len(), 0);

        // Shrink to 1x1 cell (16x16px): only top-left cell survives.
        p.resize_level("A", 16, 16).unwrap();
        let li = &p.level_ref(p.find_level("A").unwrap()).unwrap()["layerInstances"][0];
        assert_eq!(li.get("__cWid").and_then(Value::as_i64), Some(1));
        let csv: Vec<i64> = li["intGridCsv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        assert_eq!(csv, vec![1]);
    }

    #[test]
    fn resize_level_clips_tiles_and_entities() {
        let mut p = project(json!({
            "levels": [{
                "identifier": "A", "pxWid": 64, "pxHei": 64,
                "layerInstances": [{
                    "__identifier": "Tiles", "__type": "Tiles",
                    "__cWid": 4, "__cHei": 4, "__gridSize": 16,
                    "intGridCsv": [], "autoLayerTiles": [],
                    "gridTiles": [
                        { "px": [0, 0], "d": [0] },
                        { "px": [48, 48], "d": [15] },
                    ],
                    "entityInstances": [
                        { "__grid": [0, 0] },
                        { "__grid": [3, 3] },
                    ],
                }],
            }],
        }));
        // Shrink to 2x2 cells (32x32px): only the (0,0) tile and entity survive.
        p.resize_level("A", 32, 32).unwrap();
        let li = &p.level_ref(p.find_level("A").unwrap()).unwrap()["layerInstances"][0];
        let tiles = li["gridTiles"].as_array().unwrap();
        assert_eq!(tiles.len(), 1);
        // d recomputed for the new width (2): cell (0,0) -> 0.
        assert_eq!(tiles[0]["d"], json!([0]));
        assert_eq!(li["entityInstances"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn create_world_appends_with_defaults_and_rejects_dupes() {
        let mut p = project(json!({ "defaultLevelWidth": 320, "defaultLevelHeight": 240 }));
        let w = p.create_world("Overworld", None, None, None, None, None).unwrap();
        assert_eq!(w.get("worldLayout").and_then(Value::as_str), Some("Free"));
        assert_eq!(w.get("defaultLevelWidth").and_then(Value::as_i64), Some(320));
        assert_eq!(p.find_world("Overworld"), Some(0));
        let err = p
            .create_world("Overworld", None, None, None, None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn set_world_layout_updates_fields() {
        let mut p = project(json!({
            "worlds": [{ "identifier": "W", "iid": "w-iid", "worldLayout": "Free",
                         "worldGridWidth": 256, "worldGridHeight": 256, "levels": [] }],
        }));
        p.set_world_layout("W", Some("GridVania"), Some(128), None).unwrap();
        let w = &p.root["worlds"][0];
        assert_eq!(w.get("worldLayout").and_then(Value::as_str), Some("GridVania"));
        assert_eq!(w.get("worldGridWidth").and_then(Value::as_i64), Some(128));
        assert_eq!(w.get("worldGridHeight").and_then(Value::as_i64), Some(256));
        // Addressable by iid too.
        assert_eq!(p.find_world("w-iid"), Some(0));
        assert!(p.set_world_layout("missing", Some("Free"), None, None).is_err());
    }

    #[test]
    fn move_entity_recomputes_grid_and_px() {
        let mut p = project(json!({
            "defs": {
                "entities": [{
                    "uid": 3, "identifier": "Chest", "width": 16, "height": 16,
                    "pivotX": 0.5, "pivotY": 1.0,
                }],
            },
            "levels": [{
                "identifier": "L",
                "layerInstances": [{
                    "__type": "Entities", "__identifier": "Entities", "__gridSize": 16,
                    "entityInstances": [{
                        "iid": "ent-1", "__identifier": "Chest", "__grid": [0, 0], "px": [8, 16],
                    }],
                }],
            }],
        }));
        let r = p.find_level("L").unwrap();
        p.move_entity(r, "ent-1", 3, 4).unwrap();
        let e = p.entity_instance_ref(r, "ent-1").unwrap();
        assert_eq!(e.get("__grid"), Some(&json!([3, 4])));
        // px = cx*grid + pivot*grid: x = 3*16 + 0.5*16 = 56, y = 4*16 + 1.0*16 = 80
        assert_eq!(e.get("px"), Some(&json!([56, 80])));
        assert!(p.dirty);
        assert!(p.move_entity(r, "missing", 0, 0).is_err());
    }

    #[test]
    fn delete_entity_removes_instance() {
        let mut p = project(json!({
            "levels": [{
                "identifier": "L",
                "layerInstances": [{
                    "__type": "Entities", "__identifier": "Entities",
                    "entityInstances": [
                        { "iid": "a", "__identifier": "Chest" },
                        { "iid": "b", "__identifier": "Mob" },
                    ],
                }],
            }],
        }));
        let r = p.find_level("L").unwrap();
        p.delete_entity(r, "a").unwrap();
        assert!(p.entity_instance_ref(r, "a").is_none());
        assert!(p.entity_instance_ref(r, "b").is_some());
        assert!(p.delete_entity(r, "missing").is_err());
    }

    #[test]
    fn flood_fill_intgrid_fills_bounded_region() {
        // 4x4 grid, border of 1s, interior 0s:
        // 1 1 1 1
        // 1 0 0 1
        // 1 0 0 1
        // 1 1 1 1
        let csv = vec![1, 1, 1, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 1, 1, 1];
        let mut p = project(json!({
            "levels": [{
                "identifier": "L",
                "layerInstances": [{
                    "__type": "IntGrid", "__identifier": "Collisions",
                    "__cWid": 4, "__cHei": 4, "__gridSize": 16,
                    "intGridCsv": csv, "autoLayerTiles": [{ "px": [0, 0] }],
                }],
            }],
        }));
        let r = p.find_level("L").unwrap();
        // Fill the interior (1,1) -> value 2: only the 4 interior cells.
        let filled = p.flood_fill_intgrid(r, "Collisions", 1, 1, 2).unwrap();
        assert_eq!(filled, 4);
        let li = p.layer_instance_ref(r, "Collisions").unwrap();
        let out: Vec<i64> = li["intGridCsv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        assert_eq!(out, vec![1, 1, 1, 1, 1, 2, 2, 1, 1, 2, 2, 1, 1, 1, 1, 1]);
        // AutoLayer tiles cleared for regeneration.
        assert_eq!(li["autoLayerTiles"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn flood_fill_intgrid_noop_when_value_matches() {
        let mut p = project(json!({
            "levels": [{
                "identifier": "L",
                "layerInstances": [{
                    "__type": "IntGrid", "__identifier": "Collisions",
                    "__cWid": 2, "__cHei": 2, "__gridSize": 16,
                    "intGridCsv": [5, 5, 5, 5], "autoLayerTiles": [{ "px": [0, 0] }],
                }],
            }],
        }));
        let r = p.find_level("L").unwrap();
        let filled = p.flood_fill_intgrid(r, "Collisions", 0, 0, 5).unwrap();
        assert_eq!(filled, 0);
        // No-op leaves autoLayerTiles untouched.
        let li = p.layer_instance_ref(r, "Collisions").unwrap();
        assert_eq!(li["autoLayerTiles"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn flood_fill_intgrid_validates_layer_and_bounds() {
        let mut p = project(json!({
            "levels": [{
                "identifier": "L",
                "layerInstances": [
                    { "__type": "Tiles", "__identifier": "Tiles", "__cWid": 2, "__cHei": 2,
                      "__gridSize": 16, "intGridCsv": [] },
                    { "__type": "IntGrid", "__identifier": "Collisions", "__cWid": 2, "__cHei": 2,
                      "__gridSize": 16, "intGridCsv": [0, 0, 0, 0], "autoLayerTiles": [] },
                ],
            }],
        }));
        let r = p.find_level("L").unwrap();
        assert!(p.flood_fill_intgrid(r, "Tiles", 0, 0, 1).is_err());
        assert!(p.flood_fill_intgrid(r, "Collisions", 5, 5, 1).is_err());
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
