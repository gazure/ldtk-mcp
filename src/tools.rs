//! MCP tool surface for editing LDtk projects.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use base64::Engine as _;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{
        Annotated, CallToolResult, Content, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams,
        RawResource, RawResourceTemplate, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
    },
    schemars::{self, JsonSchema},
    service::RequestContext,
    tool, tool_router, ErrorData, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    project::Project,
    render::{self, RenderOpts},
};

#[derive(Clone)]
pub struct LdtkServer {
    state: Arc<Mutex<Option<Project>>>,
    tool_router: rmcp::handler::server::tool::ToolRouter<Self>,
}

fn err(msg: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(msg.into(), None)
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

/// Compact, agent-friendly view of an entity instance: grid coords, size, tags, and a
/// `fields` map of `__identifier` -> `__value` folded from `fieldInstances`.
fn entity_summary(e: &Value) -> Value {
    let grid = e.get("__grid").and_then(Value::as_array);
    let cx = grid.and_then(|g| g.first()).and_then(Value::as_i64);
    let cy = grid.and_then(|g| g.get(1)).and_then(Value::as_i64);
    let mut fields = serde_json::Map::new();
    if let Some(fis) = e.get("fieldInstances").and_then(Value::as_array) {
        for fi in fis {
            if let Some(id) = fi.get("__identifier").and_then(Value::as_str) {
                fields.insert(id.to_string(), fi.get("__value").cloned().unwrap_or(Value::Null));
            }
        }
    }
    json!({
        "iid": e.get("iid"),
        "identifier": e.get("__identifier"),
        "cx": cx,
        "cy": cy,
        "px": e.get("px"),
        "width": e.get("width"),
        "height": e.get("height"),
        "tags": e.get("__tags"),
        "fields": Value::Object(fields),
    })
}

/// Merge encoded field instances into a `fieldInstances` array: replace entries with the
/// same `__identifier`, append new ones.
fn merge_field_instances(target: &mut Value, encoded: Vec<Value>) {
    if !target.is_array() {
        *target = json!([]);
    }
    let arr = target.as_array_mut().unwrap();
    for fi in encoded {
        let id = fi.get("__identifier").and_then(Value::as_str).map(String::from);
        if let Some(pos) = arr
            .iter()
            .position(|e| e.get("__identifier").and_then(Value::as_str).map(String::from) == id)
        {
            arr[pos] = fi;
        } else {
            arr.push(fi);
        }
    }
}

// ---- Parameter types -------------------------------------------------------

#[derive(Deserialize, JsonSchema)]
pub struct OpenArgs {
    /// Absolute or relative path to a `.ldtk` project file.
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LevelKey {
    /// Level identifier, iid, or uid.
    pub level: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateLevelArgs {
    /// Unique identifier for the new level (e.g. "Cave_01").
    pub identifier: String,
    /// Level width in pixels. Defaults to the project's `defaultLevelWidth`.
    pub px_wid: Option<i64>,
    /// Level height in pixels. Defaults to the project's `defaultLevelHeight`.
    pub px_hei: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Rect {
    /// Left grid cell (column).
    pub cx: i64,
    /// Top grid cell (row).
    pub cy: i64,
    /// Width in cells.
    pub w: i64,
    /// Height in cells.
    pub h: i64,
    /// IntGrid value to write (0 = empty, 1+ = a defined value).
    pub value: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetIntGridArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// IntGrid layer identifier.
    pub layer: String,
    /// Optional full row-major grid (length must equal cWid*cHei). Replaces the whole layer.
    pub csv: Option<Vec<i64>>,
    /// Optional rectangle fills applied on top of the current (or provided) grid.
    pub rects: Option<Vec<Rect>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct EntityPlacement {
    /// Entity definition identifier (must exist in defs).
    pub identifier: String,
    /// Grid X (column).
    pub cx: i64,
    /// Grid Y (row).
    pub cy: i64,
    /// Optional width override in pixels.
    pub width: Option<i64>,
    /// Optional height override in pixels.
    pub height: Option<i64>,
    /// Optional typed field values, keyed by field identifier (encoded against the entity def).
    pub fields: Option<HashMap<String, Value>>,
    /// Optional pre-built `fieldInstances` array (advanced; passed through as-is). Overrides `fields`.
    pub field_instances: Option<Vec<Value>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PlaceEntitiesArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Entity layer identifier.
    pub layer: String,
    /// Entities to place.
    pub entities: Vec<EntityPlacement>,
    /// If true, remove existing entity instances on the layer first. Default false.
    pub replace: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetEntityFieldsArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Instance iid of the entity to modify.
    pub entity_iid: String,
    /// Typed field values keyed by field identifier. `null` clears a field.
    pub fields: HashMap<String, Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetLevelFieldsArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Typed level field values keyed by field identifier. `null` clears a field.
    pub fields: HashMap<String, Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PaintTile {
    /// Grid X (column).
    pub cx: i64,
    /// Grid Y (row).
    pub cy: i64,
    /// Tile id in the layer's tileset.
    pub t: i64,
    /// Flip bits: 0=none, 1=X, 2=Y, 3=both. Defaults to 0.
    pub flip: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PaintTilesArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Tile layer identifier.
    pub layer: String,
    /// Tiles to place.
    pub tiles: Vec<PaintTile>,
    /// If true, remove existing grid tiles first. Default false (new tiles render on top).
    pub replace: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GridRect {
    pub cx: i64,
    pub cy: i64,
    pub w: i64,
    pub h: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct ClearTilesArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Tile layer identifier.
    pub layer: String,
    /// Optional grid rectangle to clear. If omitted, clears the whole layer.
    pub rect: Option<GridRect>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetLayerArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Layer identifier or iid.
    pub layer: String,
    /// Include the full `autoLayerTiles` array (can be large). Default false (count only).
    pub include_auto_tiles: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetEntitiesArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Optional Entity layer identifier or iid. If omitted, scans all Entity layers.
    pub layer: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetIntGridArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// IntGrid layer identifier or iid.
    pub layer: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetEntityArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Instance iid of the entity to fetch.
    pub entity_iid: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteLevelArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct DuplicateLevelArgs {
    /// Source level identifier, iid, or uid.
    pub level: String,
    /// Identifier for the copy. Defaults to `<source>_copy` (deduplicated).
    pub identifier: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct MoveLevelArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// New world-space X position in pixels.
    pub world_x: i64,
    /// New world-space Y position in pixels.
    pub world_y: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct ResizeLevelArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// New level width in pixels.
    pub px_wid: i64,
    /// New level height in pixels.
    pub px_hei: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateWorldArgs {
    /// Unique identifier for the new world.
    pub identifier: String,
    /// World layout: `Free`, `GridVania`, `LinearHorizontal`, or `LinearVertical`. Default `Free`.
    pub world_layout: Option<String>,
    /// World grid width in pixels (GridVania). Default 256.
    pub world_grid_width: Option<i64>,
    /// World grid height in pixels (GridVania). Default 256.
    pub world_grid_height: Option<i64>,
    /// Default new-level width in pixels. Defaults to the project's `defaultLevelWidth`.
    pub default_level_width: Option<i64>,
    /// Default new-level height in pixels. Defaults to the project's `defaultLevelHeight`.
    pub default_level_height: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct SetWorldLayoutArgs {
    /// World identifier or iid.
    pub world: String,
    /// New world layout: `Free`, `GridVania`, `LinearHorizontal`, or `LinearVertical`.
    pub world_layout: Option<String>,
    /// New world grid width in pixels (GridVania).
    pub world_grid_width: Option<i64>,
    /// New world grid height in pixels (GridVania).
    pub world_grid_height: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct MoveEntityArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Instance iid of the entity to move.
    pub entity_iid: String,
    /// New grid X (column).
    pub cx: i64,
    /// New grid Y (row).
    pub cy: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct DeleteEntityArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Instance iid of the entity to delete.
    pub entity_iid: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct FloodFillIntGridArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// IntGrid layer identifier or iid.
    pub layer: String,
    /// Start grid X (column).
    pub cx: i64,
    /// Start grid Y (row).
    pub cy: i64,
    /// IntGrid value to fill the contiguous region with.
    pub value: i64,
}

#[derive(Deserialize, JsonSchema)]
pub struct IntGridValueSpec {
    /// The IntGrid value (1-based, matching LDtk).
    pub value: i64,
    /// Optional human-readable identifier for the value.
    pub identifier: Option<String>,
    /// Optional hex color (e.g. `#FF0000`). Defaults to `#FFFFFF`.
    pub color: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddIntGridValuesArgs {
    /// IntGrid layer definition to extend, by identifier or uid.
    pub layer: String,
    /// Value definitions to add or update (upserted by `value`).
    pub values: Vec<IntGridValueSpec>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateLayerDefArgs {
    /// Unique identifier for the new layer definition.
    pub identifier: String,
    /// Layer type: `IntGrid`, `Entities`, `Tiles`, or `AutoLayer`.
    #[serde(rename = "type")]
    pub layer_type: String,
    /// Grid size in pixels. Defaults to the project's `defaultGridSize` or 16.
    pub grid_size: Option<i64>,
    /// Tileset def uid to bind (`Tiles`/`AutoLayer` layers).
    pub tileset_def_uid: Option<i64>,
    /// For `IntGrid` layers: the value definitions to populate.
    pub int_grid_values: Option<Vec<IntGridValueSpec>>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateEntityDefArgs {
    /// Unique identifier for the new entity definition.
    pub identifier: String,
    /// Pixel width. Default 16.
    pub width: Option<i64>,
    /// Pixel height. Default 16.
    pub height: Option<i64>,
    /// Base color as hex (e.g. `#94D9B3`). Default `#94D9B3`.
    pub color: Option<String>,
    /// Tags classifying this entity.
    pub tags: Option<Vec<String>>,
    /// Tileset def uid for tile rendering (requires `tile_id`).
    pub tileset_uid: Option<i64>,
    /// Tile id within the tileset (requires `tileset_uid`); enables `Tile` render mode.
    pub tile_id: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateEnumArgs {
    /// Unique identifier for the new enum.
    pub identifier: String,
    /// Enum value identifiers.
    pub values: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct CreateTilesetDefArgs {
    /// Unique identifier for the new tileset definition.
    pub identifier: String,
    /// Path to the image file, relative to the project JSON.
    pub rel_path: String,
    /// Image width in pixels.
    pub px_wid: i64,
    /// Image height in pixels.
    pub px_hei: i64,
    /// Tile grid size in pixels. Default 16.
    pub tile_grid_size: Option<i64>,
    /// Padding in pixels from image borders. Default 0.
    pub padding: Option<i64>,
    /// Spacing in pixels between tiles. Default 0.
    pub spacing: Option<i64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddEntityFieldArgs {
    /// Entity definition identifier to add the field to.
    pub entity: String,
    /// Unique identifier for the new field.
    pub identifier: String,
    /// Field type: `Int`, `Float`, `Bool`, `String`, `Multilines`, `FilePath`, `Color`, `Point`, `EntityRef`, `Tile`, or `Enum`.
    pub field_type: String,
    /// If true, the field holds an array of values. Default false.
    pub is_array: Option<bool>,
    /// If true, the value can be null. Default true.
    pub can_be_null: Option<bool>,
    /// Optional min limit (Int/Float).
    pub min: Option<f64>,
    /// Optional max limit (Int/Float).
    pub max: Option<f64>,
    /// For `Enum` fields: the enum identifier to reference.
    pub enum_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddLevelFieldArgs {
    /// Unique identifier for the new field.
    pub identifier: String,
    /// Field type: `Int`, `Float`, `Bool`, `String`, `Multilines`, `FilePath`, `Color`, `Point`, `EntityRef`, `Tile`, or `Enum`.
    pub field_type: String,
    /// If true, the field holds an array of values. Default false.
    pub is_array: Option<bool>,
    /// If true, the value can be null. Default true.
    pub can_be_null: Option<bool>,
    /// Optional min limit (Int/Float).
    pub min: Option<f64>,
    /// Optional max limit (Int/Float).
    pub max: Option<f64>,
    /// For `Enum` fields: the enum identifier to reference.
    pub enum_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct RenderLevelArgs {
    /// Level identifier, iid, or uid.
    pub level: String,
    /// Output pixels per source pixel. Overrides `max_px` when set.
    pub scale: Option<f64>,
    /// Cap for the longest output edge in pixels (when `scale` is omitted). Default 1024.
    pub max_px: Option<i64>,
    /// If set, render only the layers whose identifier is listed (bottom-to-top order preserved).
    pub layers: Option<Vec<String>>,
}

// ---- Tool implementations --------------------------------------------------

#[tool_router]
impl LdtkServer {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    fn with_project<T>(&self, f: impl FnOnce(&mut Project) -> Result<T, ErrorData>) -> Result<T, ErrorData> {
        let mut guard = self.state.lock().map_err(|_| err("state lock poisoned"))?;
        let proj = guard
            .as_mut()
            .ok_or_else(|| err("no project open; call open_project first"))?;
        f(proj)
    }

    /// Like `with_project`, but snapshots `root` for undo before running `f`. The snapshot is
    /// kept only if `f` succeeds, so a tool that errors (e.g. failed validation) leaves no
    /// spurious history. Used by every mutating tool.
    fn with_project_mut<T>(&self, f: impl FnOnce(&mut Project) -> Result<T, ErrorData>) -> Result<T, ErrorData> {
        let mut guard = self.state.lock().map_err(|_| err("state lock poisoned"))?;
        let proj = guard
            .as_mut()
            .ok_or_else(|| err("no project open; call open_project first"))?;
        let snapshot = proj.root.clone();
        let result = f(proj);
        if result.is_ok() {
            proj.commit_undo(snapshot);
        }
        result
    }

    #[tool(description = "Open a .ldtk project file for reading and editing. Must be called before other tools.")]
    fn open_project(&self, Parameters(args): Parameters<OpenArgs>) -> Result<String, ErrorData> {
        let proj = Project::load(&args.path).map_err(|e| err(format!("{e:#}")))?;
        let summary = json!({
            "path": proj.path.display().to_string(),
            "jsonVersion": proj.json_version(),
            "worldLayout": proj.world_layout(),
            "externalLevels": proj.external_levels(),
            "levelCount": proj.levels().len(),
            "layerDefs": proj.layer_defs().iter().map(|d| &d.identifier).collect::<Vec<_>>(),
            "entityDefs": proj.entity_defs().iter().map(|d| &d.identifier).collect::<Vec<_>>(),
            "externalLevelNote": if proj.external_levels() {
                "Separate level files (.ldtkl) are loaded in memory and rewritten on save_project."
            } else { "" },
        });
        *self.state.lock().map_err(|_| err("state lock poisoned"))? = Some(proj);
        Ok(pretty(&summary))
    }

    #[tool(description = "List all levels with their world position and pixel size.")]
    fn list_levels(&self) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let levels: Vec<Value> = p
                .levels()
                .iter()
                .map(|l| {
                    json!({
                        "identifier": l.get("identifier"),
                        "iid": l.get("iid"),
                        "uid": l.get("uid"),
                        "worldX": l.get("worldX"),
                        "worldY": l.get("worldY"),
                        "pxWid": l.get("pxWid"),
                        "pxHei": l.get("pxHei"),
                    })
                })
                .collect();
            Ok(pretty(&json!(levels)))
        })
    }

    #[tool(
        description = "Describe project definitions: layers (with IntGrid values), entities (with fields), tilesets and enums. Call this before generating content."
    )]
    fn describe_defs(&self) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let defs = p.root.get("defs").cloned().unwrap_or(json!({}));
            let layers: Vec<Value> = defs
                .get("layers")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|l| {
                            json!({
                                "identifier": l.get("identifier"),
                                "type": l.get("__type").or_else(|| l.get("type")),
                                "uid": l.get("uid"),
                                "gridSize": l.get("gridSize"),
                                "intGridValues": l.get("intGridValues"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let entities: Vec<Value> = defs
                .get("entities")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|e| {
                            json!({
                                "identifier": e.get("identifier"),
                                "uid": e.get("uid"),
                                "width": e.get("width"),
                                "height": e.get("height"),
                                "tags": e.get("tags"),
                                "fields": e.get("fieldDefs").and_then(Value::as_array).map(|fa| {
                                    fa.iter().map(|f| json!({
                                        "identifier": f.get("identifier"),
                                        "type": f.get("__type").or_else(|| f.get("type")),
                                    })).collect::<Vec<_>>()
                                }),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let tilesets: Vec<Value> = defs
                .get("tilesets")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|t| {
                            json!({
                                "identifier": t.get("identifier"),
                                "uid": t.get("uid"),
                                "relPath": t.get("relPath"),
                                "tileGridSize": t.get("tileGridSize"),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let enums: Vec<Value> = defs
                .get("enums")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|en| {
                            json!({
                                "identifier": en.get("identifier"),
                                "values": en.get("values").and_then(Value::as_array).map(|va| {
                                    va.iter().filter_map(|v| v.get("id").cloned()).collect::<Vec<_>>()
                                }),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(pretty(&json!({
                "layers": layers,
                "entities": entities,
                "tilesets": tilesets,
                "enums": enums,
            })))
        })
    }

    #[tool(
        description = "Get a level with a summary of each of its layer instances (type, grid size, content counts)."
    )]
    fn get_level(&self, Parameters(args): Parameters<LevelKey>) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let level = p.level_ref(idx).ok_or_else(|| err("level vanished"))?;
            let layers: Vec<Value> = level
                .get("layerInstances")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|li| {
                            let (intgrid_nonzero, grid_tiles, auto_tiles, entities) = crate::diff::layer_counts(li);
                            json!({
                                "identifier": li.get("__identifier"),
                                "type": li.get("__type"),
                                "cWid": li.get("__cWid"),
                                "cHei": li.get("__cHei"),
                                "gridSize": li.get("__gridSize"),
                                "entityCount": entities,
                                "gridTileCount": grid_tiles,
                                "autoTileCount": auto_tiles,
                                "intGridNonZero": intgrid_nonzero,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(pretty(&json!({
                "identifier": level.get("identifier"),
                "uid": level.get("uid"),
                "pxWid": level.get("pxWid"),
                "pxHei": level.get("pxHei"),
                "fieldInstances": level.get("fieldInstances"),
                "layers": layers,
            })))
        })
    }

    #[tool(
        description = "Read the full content of a single layer instance: IntGrid CSV, grid tiles, or entities (with decoded fields), plus dimensions. AutoLayer tiles are counted unless include_auto_tiles is true."
    )]
    fn get_layer(&self, Parameters(args): Parameters<GetLayerArgs>) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let li = p
                .layer_instance_ref(idx, &args.layer)
                .ok_or_else(|| err(format!("layer '{}' not found in level", args.layer)))?;
            let kind = li.get("__type").and_then(Value::as_str).unwrap_or("");
            let mut out = json!({
                "identifier": li.get("__identifier"),
                "type": li.get("__type"),
                "cWid": li.get("__cWid"),
                "cHei": li.get("__cHei"),
                "gridSize": li.get("__gridSize"),
                "tilesetDefUid": li.get("__tilesetDefUid"),
                "tilesetRelPath": li.get("__tilesetRelPath"),
                "opacity": li.get("__opacity"),
                "visible": li.get("visible"),
            });
            let obj = out.as_object_mut().unwrap();
            match kind {
                "IntGrid" => {
                    obj.insert("intGridCsv".into(), li.get("intGridCsv").cloned().unwrap_or(json!([])));
                }
                "Tiles" => {
                    obj.insert("gridTiles".into(), li.get("gridTiles").cloned().unwrap_or(json!([])));
                }
                "Entities" => {
                    let ents: Vec<Value> = li
                        .get("entityInstances")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().map(entity_summary).collect())
                        .unwrap_or_default();
                    obj.insert("entities".into(), json!(ents));
                }
                _ => {}
            }
            // AutoLayer tiles can appear on IntGrid and AutoLayer layers; large and generated.
            let auto = li.get("autoLayerTiles").and_then(Value::as_array);
            if args.include_auto_tiles.unwrap_or(false) {
                obj.insert("autoLayerTiles".into(), json!(auto.cloned().unwrap_or_default()));
            } else {
                obj.insert("autoLayerTileCount".into(), json!(auto.map(|a| a.len()).unwrap_or(0)));
            }
            Ok(pretty(&out))
        })
    }

    #[tool(
        description = "List entity instances on a level with their iid, grid position, size, tags, and decoded field values. If `layer` is omitted, scans all Entity layers."
    )]
    fn get_entities(&self, Parameters(args): Parameters<GetEntitiesArgs>) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let layers: Vec<&Value> = match &args.layer {
                Some(layer_id) => {
                    let li = p
                        .layer_instance_ref(idx, layer_id)
                        .ok_or_else(|| err(format!("layer '{layer_id}' not found in level")))?;
                    if li.get("__type").and_then(Value::as_str) != Some("Entities") {
                        return Err(err(format!("layer '{layer_id}' is not an Entities layer")));
                    }
                    vec![li]
                }
                None => p.entity_layer_instances(idx),
            };
            let mut result = Vec::new();
            for li in layers {
                let layer_id = li.get("__identifier").and_then(Value::as_str).unwrap_or("");
                let entities: Vec<Value> = li
                    .get("entityInstances")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(entity_summary).collect())
                    .unwrap_or_default();
                result.push(json!({ "layer": layer_id, "entities": entities }));
            }
            Ok(pretty(&json!(result)))
        })
    }

    #[tool(
        description = "Read an IntGrid layer: dimensions, the row-major `csv` (same shape set_intgrid accepts), and the value definitions (number -> identifier/color)."
    )]
    fn get_intgrid(&self, Parameters(args): Parameters<GetIntGridArgs>) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let li = p
                .layer_instance_ref(idx, &args.layer)
                .ok_or_else(|| err(format!("layer '{}' not found in level", args.layer)))?;
            if li.get("__type").and_then(Value::as_str) != Some("IntGrid") {
                return Err(err(format!("layer '{}' is not an IntGrid layer", args.layer)));
            }
            let layer_id = li
                .get("__identifier")
                .and_then(Value::as_str)
                .unwrap_or(&args.layer)
                .to_string();
            let payload = json!({
                "identifier": li.get("__identifier"),
                "cWid": li.get("__cWid"),
                "cHei": li.get("__cHei"),
                "gridSize": li.get("__gridSize"),
                "csv": li.get("intGridCsv").cloned().unwrap_or(json!([])),
                "values": p.intgrid_value_defs(&layer_id),
            });
            Ok(pretty(&payload))
        })
    }

    #[tool(
        description = "Fetch a single entity instance by its iid, including a decoded `fields` map and the raw `fieldInstances` (useful before set_entity_fields)."
    )]
    fn get_entity(&self, Parameters(args): Parameters<GetEntityArgs>) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let e = p.entity_instance_ref(idx, &args.entity_iid).ok_or_else(|| {
                err(format!(
                    "entity '{}' not found in level '{}'",
                    args.entity_iid, args.level
                ))
            })?;
            let mut summary = entity_summary(e);
            summary.as_object_mut().unwrap().insert(
                "fieldInstances".into(),
                e.get("fieldInstances").cloned().unwrap_or(json!([])),
            );
            Ok(pretty(&summary))
        })
    }

    #[tool(description = "Create a new empty level. Layer instances are generated from the project layer definitions.")]
    fn create_level(&self, Parameters(args): Parameters<CreateLevelArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let px_wid = args
                .px_wid
                .or_else(|| p.root.get("defaultLevelWidth").and_then(Value::as_i64))
                .unwrap_or(256);
            let px_hei = args
                .px_hei
                .or_else(|| p.root.get("defaultLevelHeight").and_then(Value::as_i64))
                .unwrap_or(256);
            let level = p
                .create_level(&args.identifier, px_wid, px_hei)
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Created level '{}' ({}x{}px). uid={}, iid={}.\nRemember to call save_project.",
                args.identifier,
                px_wid,
                px_hei,
                level.get("uid").and_then(Value::as_i64).unwrap_or(0),
                level.get("iid").and_then(Value::as_str).unwrap_or(""),
            ))
        })
    }

    #[tool(
        description = "Delete a level by identifier/iid/uid. For external-level projects the .ldtkl file is removed on save_project."
    )]
    fn delete_level(&self, Parameters(args): Parameters<DeleteLevelArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let iid = p.delete_level(&args.level).map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Deleted level '{}' (iid={}). Call save_project to persist.",
                args.level, iid
            ))
        })
    }

    #[tool(
        description = "Duplicate a level (deep copy with fresh uid/iids), placed at the next free world position. Optionally name the copy."
    )]
    fn duplicate_level(&self, Parameters(args): Parameters<DuplicateLevelArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let level = p
                .duplicate_level(&args.level, args.identifier.as_deref())
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Duplicated '{}' as '{}' (uid={}, iid={}). Call save_project to persist.",
                args.level,
                level.get("identifier").and_then(Value::as_str).unwrap_or(""),
                level.get("uid").and_then(Value::as_i64).unwrap_or(0),
                level.get("iid").and_then(Value::as_str).unwrap_or(""),
            ))
        })
    }

    #[tool(description = "Move a level to a new world-space pixel position (worldX/worldY).")]
    fn move_level(&self, Parameters(args): Parameters<MoveLevelArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            p.move_level(&args.level, args.world_x, args.world_y)
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Moved level '{}' to ({}, {}). Call save_project to persist.",
                args.level, args.world_x, args.world_y
            ))
        })
    }

    #[tool(
        description = "Resize a level. Layer instances are reflowed: IntGrid is resized, and tiles/entities outside the new bounds are clipped."
    )]
    fn resize_level(&self, Parameters(args): Parameters<ResizeLevelArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            p.resize_level(&args.level, args.px_wid, args.px_hei)
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Resized level '{}' to {}x{}px (content clipped to bounds). Call save_project to persist.",
                args.level, args.px_wid, args.px_hei
            ))
        })
    }

    #[tool(description = "Create a new (empty) world in a multi-world project. Appends to the root `worlds` array.")]
    fn create_world(&self, Parameters(args): Parameters<CreateWorldArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let root_has_levels = p
                .root
                .get("levels")
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            let world = p
                .create_world(
                    &args.identifier,
                    args.world_layout.as_deref(),
                    args.world_grid_width,
                    args.world_grid_height,
                    args.default_level_width,
                    args.default_level_height,
                )
                .map_err(|e| err(format!("{e:#}")))?;
            let note = if root_has_levels {
                " Note: this project also has root-level `levels`; multi-world tools address worlds separately."
            } else {
                ""
            };
            Ok(format!(
                "Created world '{}' (iid={}, layout={}).{} Call save_project to persist.",
                args.identifier,
                world.get("iid").and_then(Value::as_str).unwrap_or(""),
                world.get("worldLayout").and_then(Value::as_str).unwrap_or("Free"),
                note,
            ))
        })
    }

    #[tool(
        description = "Update a world's layout and/or grid dimensions (worldLayout, worldGridWidth, worldGridHeight)."
    )]
    fn set_world_layout(&self, Parameters(args): Parameters<SetWorldLayoutArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            p.set_world_layout(
                &args.world,
                args.world_layout.as_deref(),
                args.world_grid_width,
                args.world_grid_height,
            )
            .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!("Updated world '{}'. Call save_project to persist.", args.world))
        })
    }

    #[tool(
        description = "Move an existing entity instance (by iid) to a new grid cell. Pixel position is recomputed from the entity definition pivot."
    )]
    fn move_entity(&self, Parameters(args): Parameters<MoveEntityArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            p.move_entity(idx, &args.entity_iid, args.cx, args.cy)
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Moved entity '{}' to ({}, {}). Call save_project to persist.",
                args.entity_iid, args.cx, args.cy
            ))
        })
    }

    #[tool(description = "Delete a single entity instance (by iid) from a level.")]
    fn delete_entity(&self, Parameters(args): Parameters<DeleteEntityArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            p.delete_entity(idx, &args.entity_iid)
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Deleted entity '{}' from level '{}'. Call save_project to persist.",
                args.entity_iid, args.level
            ))
        })
    }

    #[tool(
        description = "Flood fill an IntGrid layer (4-connected) from a start cell, replacing the contiguous region that shares the start cell's value. AutoLayer tiles are cleared so LDtk regenerates them."
    )]
    fn flood_fill_intgrid(&self, Parameters(args): Parameters<FloodFillIntGridArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let filled = p
                .flood_fill_intgrid(idx, &args.layer, args.cx, args.cy, args.value)
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Flood filled {} cell(s) on '{}' from ({}, {}) with value {}. Call save_project to persist.",
                filled, args.layer, args.cx, args.cy, args.value
            ))
        })
    }

    #[tool(
        description = "Create a new layer definition (IntGrid/Entities/Tiles/AutoLayer) and backfill an empty instance into every existing level. For IntGrid, provide `int_grid_values`."
    )]
    fn create_layer_def(&self, Parameters(args): Parameters<CreateLayerDefArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let ig_values = args.int_grid_values.map(|vs| {
                vs.into_iter()
                    .map(|s| {
                        json!({
                            "value": s.value,
                            "identifier": s.identifier,
                            "color": s.color,
                        })
                    })
                    .collect::<Vec<Value>>()
            });
            let def = p
                .create_layer_def(
                    &args.identifier,
                    &args.layer_type,
                    args.grid_size,
                    args.tileset_def_uid,
                    ig_values,
                )
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Created {} layer def '{}' (uid={}) and backfilled all levels. Call save_project to persist.",
                args.layer_type,
                args.identifier,
                def.get("uid").and_then(Value::as_i64).unwrap_or(0),
            ))
        })
    }

    #[tool(
        description = "Add or update IntGrid value definitions on an existing IntGrid layer def (addressed by identifier or uid). Upserts by `value`; extends a layer's palette without recreating it. Level instances are unaffected."
    )]
    fn add_intgrid_values(&self, Parameters(args): Parameters<AddIntGridValuesArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let specs: Vec<Value> = args
                .values
                .iter()
                .map(|s| json!({ "value": s.value, "identifier": s.identifier, "color": s.color }))
                .collect();
            let (added, updated) = p
                .add_intgrid_values(&args.layer, specs)
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "IntGrid layer '{}': {added} value(s) added, {updated} updated. Call save_project to persist.",
                args.layer
            ))
        })
    }

    #[tool(
        description = "Create a new entity definition. Provide `tileset_uid`+`tile_id` for tile rendering, otherwise it renders as a rectangle."
    )]
    fn create_entity_def(&self, Parameters(args): Parameters<CreateEntityDefArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let def = p
                .create_entity_def(
                    &args.identifier,
                    args.width,
                    args.height,
                    args.color,
                    args.tags,
                    args.tileset_uid,
                    args.tile_id,
                )
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Created entity def '{}' (uid={}, renderMode={}). Call save_project to persist.",
                args.identifier,
                def.get("uid").and_then(Value::as_i64).unwrap_or(0),
                def.get("renderMode").and_then(Value::as_str).unwrap_or(""),
            ))
        })
    }

    #[tool(description = "Create a new enum definition from a list of value identifiers.")]
    fn create_enum(&self, Parameters(args): Parameters<CreateEnumArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let def = p
                .create_enum(&args.identifier, args.values.clone())
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Created enum '{}' (uid={}) with {} value(s). Call save_project to persist.",
                args.identifier,
                def.get("uid").and_then(Value::as_i64).unwrap_or(0),
                args.values.len(),
            ))
        })
    }

    #[tool(
        description = "Create a new tileset definition. Image dimensions (px_wid/px_hei) are explicit; no image decoding is performed."
    )]
    fn create_tileset_def(&self, Parameters(args): Parameters<CreateTilesetDefArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let def = p
                .create_tileset_def(
                    &args.identifier,
                    &args.rel_path,
                    args.px_wid,
                    args.px_hei,
                    args.tile_grid_size,
                    args.padding,
                    args.spacing,
                )
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Created tileset def '{}' (uid={}, {}x{} grid). Call save_project to persist.",
                args.identifier,
                def.get("uid").and_then(Value::as_i64).unwrap_or(0),
                def.get("__cWid").and_then(Value::as_i64).unwrap_or(0),
                def.get("__cHei").and_then(Value::as_i64).unwrap_or(0),
            ))
        })
    }

    #[tool(
        description = "Add a field definition to an existing entity definition. `field_type` is one of Int/Float/Bool/String/Multilines/FilePath/Color/Point/EntityRef/Tile/Enum (Enum requires `enum_id`)."
    )]
    fn add_entity_field(&self, Parameters(args): Parameters<AddEntityFieldArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let def = p
                .add_entity_field(
                    &args.entity,
                    &args.identifier,
                    &args.field_type,
                    args.is_array.unwrap_or(false),
                    args.can_be_null.unwrap_or(true),
                    args.min,
                    args.max,
                    args.enum_id.as_deref(),
                )
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Added field '{}' (type={}) to entity '{}'. Call save_project to persist.",
                args.identifier,
                def.get("type").and_then(Value::as_str).unwrap_or(""),
                args.entity,
            ))
        })
    }

    #[tool(
        description = "Add a field definition to the project-level `levelFields`. `field_type` is one of Int/Float/Bool/String/Multilines/FilePath/Color/Point/EntityRef/Tile/Enum (Enum requires `enum_id`)."
    )]
    fn add_level_field(&self, Parameters(args): Parameters<AddLevelFieldArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let def = p
                .add_level_field(
                    &args.identifier,
                    &args.field_type,
                    args.is_array.unwrap_or(false),
                    args.can_be_null.unwrap_or(true),
                    args.min,
                    args.max,
                    args.enum_id.as_deref(),
                )
                .map_err(|e| err(format!("{e:#}")))?;
            Ok(format!(
                "Added level field '{}' (type={}). Call save_project to persist.",
                args.identifier,
                def.get("type").and_then(Value::as_str).unwrap_or(""),
            ))
        })
    }

    #[tool(
        description = "Set the IntGrid of a layer. Provide a full `csv` (row-major, length cWid*cHei) and/or `rects` to fill regions. AutoLayer tiles are cleared so LDtk regenerates them on load."
    )]
    fn set_intgrid(&self, Parameters(args): Parameters<SetIntGridArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let li = p
                .layer_instance_mut(idx, &args.layer)
                .map_err(|e| err(format!("{e:#}")))?;
            if li.get("__type").and_then(Value::as_str) != Some("IntGrid") {
                return Err(err(format!("layer '{}' is not an IntGrid layer", args.layer)));
            }
            let c_wid = li.get("__cWid").and_then(Value::as_i64).unwrap_or(0);
            let c_hei = li.get("__cHei").and_then(Value::as_i64).unwrap_or(0);
            let total = (c_wid * c_hei).max(0) as usize;

            let mut grid: Vec<i64> = if let Some(csv) = args.csv {
                if csv.len() != total {
                    return Err(err(format!(
                        "csv length {} != cWid*cHei = {}x{} = {}",
                        csv.len(),
                        c_wid,
                        c_hei,
                        total
                    )));
                }
                csv
            } else {
                li.get("intGridCsv")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(|v| v.as_i64().unwrap_or(0)).collect())
                    .filter(|v: &Vec<i64>| v.len() == total)
                    .unwrap_or_else(|| vec![0; total])
            };

            if let Some(rects) = &args.rects {
                for r in rects {
                    for y in r.cy..(r.cy + r.h) {
                        for x in r.cx..(r.cx + r.w) {
                            if x >= 0 && y >= 0 && x < c_wid && y < c_hei {
                                grid[(y * c_wid + x) as usize] = r.value;
                            }
                        }
                    }
                }
            }

            let non_zero = grid.iter().filter(|&&v| v != 0).count();
            li["intGridCsv"] = json!(grid);
            li["autoLayerTiles"] = json!([]);
            p.dirty = true;
            Ok(format!(
                "IntGrid '{}' set on level '{}': {}x{} cells, {} non-empty. Call save_project to persist.",
                args.layer, args.level, c_wid, c_hei, non_zero
            ))
        })
    }

    #[tool(
        description = "Place entity instances on an Entity layer. Coordinates are grid cells; pixel position is derived from the entity definition pivot."
    )]
    fn place_entities(&self, Parameters(args): Parameters<PlaceEntitiesArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            // Snapshot entity defs before taking a mutable borrow of the tree.
            let edefs = p.entity_defs();

            // Read the target layer immutably to validate type and read grid size.
            let (is_entities, grid_size) = {
                let li = p
                    .layer_instance_ref(idx, &args.layer)
                    .ok_or_else(|| err(format!("layer '{}' not found in level", args.layer)))?;
                (
                    li.get("__type").and_then(Value::as_str) == Some("Entities"),
                    li.get("__gridSize").and_then(Value::as_i64).unwrap_or(16),
                )
            };
            if !is_entities {
                return Err(err(format!("layer '{}' is not an Entities layer", args.layer)));
            }

            // Build all instances (incl. encoded field instances) before mutating the tree.
            let mut built = Vec::new();
            for e in &args.entities {
                let def = edefs
                    .iter()
                    .find(|d| d.identifier == e.identifier)
                    .ok_or_else(|| err(format!("entity definition '{}' not found", e.identifier)))?;
                let w = e.width.unwrap_or(def.width);
                let h = e.height.unwrap_or(def.height);
                let px_x = (e.cx as f64 * grid_size as f64 + def.pivot_x * grid_size as f64).round() as i64;
                let px_y = (e.cy as f64 * grid_size as f64 + def.pivot_y * grid_size as f64).round() as i64;
                let field_instances = if let Some(raw) = &e.field_instances {
                    raw.clone()
                } else if let Some(fields) = &e.fields {
                    let mut out = Vec::new();
                    for (name, value) in fields {
                        let def = p.entity_field_def(&e.identifier, name).map_err(err)?;
                        out.push(p.encode_field(&def, value).map_err(err)?);
                    }
                    out
                } else {
                    Vec::new()
                };
                built.push(json!({
                    "__identifier": def.identifier,
                    "__grid": [e.cx, e.cy],
                    "__pivot": [def.pivot_x, def.pivot_y],
                    "__tags": def.tags,
                    "__tile": def.tile.clone().unwrap_or(Value::Null),
                    "__smartColor": def.color,
                    "iid": Project::new_iid(),
                    "width": w,
                    "height": h,
                    "defUid": def.uid,
                    "px": [px_x, px_y],
                    "fieldInstances": field_instances,
                }));
            }

            let li = p
                .layer_instance_mut(idx, &args.layer)
                .map_err(|e| err(format!("{e:#}")))?;
            let arr = li
                .get_mut("entityInstances")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| err("layer has no entityInstances array"))?;
            if args.replace.unwrap_or(false) {
                arr.clear();
            }
            let added = built.len();
            arr.extend(built);
            p.dirty = true;
            Ok(format!(
                "Placed {} entit{} on layer '{}' (level '{}'). Call save_project to persist.",
                added,
                if added == 1 { "y" } else { "ies" },
                args.layer,
                args.level
            ))
        })
    }

    #[tool(
        description = "Set typed field values on an existing entity instance (identified by its iid). Fields are encoded against the entity definition."
    )]
    fn set_entity_fields(&self, Parameters(args): Parameters<SetEntityFieldsArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let entity_id = p.entity_identifier(idx, &args.entity_iid).ok_or_else(|| {
                err(format!(
                    "entity '{}' not found in level '{}'",
                    args.entity_iid, args.level
                ))
            })?;

            let mut encoded = Vec::new();
            for (name, value) in &args.fields {
                let def = p.entity_field_def(&entity_id, name).map_err(err)?;
                encoded.push(p.encode_field(&def, value).map_err(err)?);
            }
            let n = encoded.len();

            let ei = p
                .entity_instance_mut(idx, &args.entity_iid)
                .map_err(|e| err(format!("{e:#}")))?;
            merge_field_instances(&mut ei["fieldInstances"], encoded);
            p.dirty = true;
            Ok(format!(
                "Set {n} field(s) on entity '{}' ({}). Call save_project to persist.",
                entity_id, args.entity_iid
            ))
        })
    }

    #[tool(description = "Set typed custom field values on a level (encoded against the project's levelFields).")]
    fn set_level_fields(&self, Parameters(args): Parameters<SetLevelFieldsArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;

            let mut encoded = Vec::new();
            for (name, value) in &args.fields {
                let def = p.level_field_def(name).map_err(err)?;
                encoded.push(p.encode_field(&def, value).map_err(err)?);
            }
            let n = encoded.len();

            let level = p.level_value_mut(idx).map_err(|e| err(format!("{e:#}")))?;
            merge_field_instances(&mut level["fieldInstances"], encoded);
            p.dirty = true;
            Ok(format!(
                "Set {n} level field(s) on '{}'. Call save_project to persist.",
                args.level
            ))
        })
    }

    #[tool(
        description = "Paint tiles on a Tile layer by grid coordinate and tile id. Pixel src is computed from the tileset geometry."
    )]
    fn paint_tiles(&self, Parameters(args): Parameters<PaintTilesArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;

            let (grid_size, c_wid, tileset_uid) = {
                let li = p
                    .layer_instance_ref(idx, &args.layer)
                    .ok_or_else(|| err(format!("layer '{}' not found in level", args.layer)))?;
                if li.get("__type").and_then(Value::as_str) != Some("Tiles") {
                    return Err(err(format!("layer '{}' is not a Tiles layer", args.layer)));
                }
                let uid = li
                    .get("overrideTilesetUid")
                    .and_then(Value::as_i64)
                    .or_else(|| li.get("__tilesetDefUid").and_then(Value::as_i64))
                    .ok_or_else(|| err(format!("layer '{}' has no tileset assigned", args.layer)))?;
                (
                    li.get("__gridSize").and_then(Value::as_i64).unwrap_or(16),
                    li.get("__cWid").and_then(Value::as_i64).unwrap_or(1).max(1),
                    uid,
                )
            };

            let mut built = Vec::with_capacity(args.tiles.len());
            for t in &args.tiles {
                let src = p
                    .tile_src(tileset_uid, t.t)
                    .ok_or_else(|| err(format!("tileset {tileset_uid} not found for tile id {}", t.t)))?;
                let coord_id = t.cy * c_wid + t.cx;
                built.push(json!({
                    "px": [t.cx * grid_size, t.cy * grid_size],
                    "src": src,
                    "f": t.flip.unwrap_or(0),
                    "t": t.t,
                    "d": [coord_id],
                    "a": 1,
                }));
            }

            let li = p
                .layer_instance_mut(idx, &args.layer)
                .map_err(|e| err(format!("{e:#}")))?;
            let arr = li
                .get_mut("gridTiles")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| err("layer has no gridTiles array"))?;
            if args.replace.unwrap_or(false) {
                arr.clear();
            }
            let added = built.len();
            arr.extend(built);
            p.dirty = true;
            Ok(format!(
                "Painted {added} tile(s) on layer '{}' (level '{}'). Call save_project to persist.",
                args.layer, args.level
            ))
        })
    }

    #[tool(description = "Remove tiles from a Tile layer, either entirely or within a grid rectangle.")]
    fn clear_tiles(&self, Parameters(args): Parameters<ClearTilesArgs>) -> Result<String, ErrorData> {
        self.with_project_mut(|p| {
            let idx = p
                .find_level(&args.level)
                .ok_or_else(|| err(format!("level '{}' not found", args.level)))?;
            let grid_size = {
                let li = p
                    .layer_instance_ref(idx, &args.layer)
                    .ok_or_else(|| err(format!("layer '{}' not found in level", args.layer)))?;
                li.get("__gridSize").and_then(Value::as_i64).unwrap_or(16)
            };

            let li = p
                .layer_instance_mut(idx, &args.layer)
                .map_err(|e| err(format!("{e:#}")))?;
            let arr = li
                .get_mut("gridTiles")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| err("layer has no gridTiles array"))?;
            let before = arr.len();
            match &args.rect {
                None => arr.clear(),
                Some(r) => arr.retain(|t| {
                    let px = t.get("px").and_then(Value::as_array);
                    let (cx, cy) = match px {
                        Some(a) if a.len() == 2 => (
                            a[0].as_i64().unwrap_or(0) / grid_size,
                            a[1].as_i64().unwrap_or(0) / grid_size,
                        ),
                        _ => return true,
                    };
                    !(cx >= r.cx && cx < r.cx + r.w && cy >= r.cy && cy < r.cy + r.h)
                }),
            }
            let removed = before - arr.len();
            p.dirty = true;
            Ok(format!(
                "Removed {removed} tile(s) from layer '{}' (level '{}').",
                args.layer, args.level
            ))
        })
    }

    #[tool(
        description = "Validate the project: authoritative structural checks (layerDefUid/defUid references, IntGrid sizes) plus best-effort JSON-schema warnings."
    )]
    fn validate_project(&self) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let mut issues: Vec<String> = Vec::new();
            let layer_uids: Vec<i64> = p.layer_defs().iter().map(|d| d.uid).collect();
            let entity_uids: Vec<i64> = p.entity_defs().iter().map(|d| d.uid).collect();

            for lvl in p.levels() {
                let lid = lvl.get("identifier").and_then(Value::as_str).unwrap_or("?");
                let Some(instances) = lvl.get("layerInstances").and_then(Value::as_array) else {
                    continue;
                };
                for li in instances {
                    let li_id = li.get("__identifier").and_then(Value::as_str).unwrap_or("?");
                    if let Some(uid) = li.get("layerDefUid").and_then(Value::as_i64) {
                        if !layer_uids.contains(&uid) {
                            issues.push(format!("{lid}/{li_id}: layerDefUid {uid} has no matching layer def"));
                        }
                    }
                    if li.get("__type").and_then(Value::as_str) == Some("IntGrid") {
                        let cw = li.get("__cWid").and_then(Value::as_i64).unwrap_or(0);
                        let ch = li.get("__cHei").and_then(Value::as_i64).unwrap_or(0);
                        let len = li
                            .get("intGridCsv")
                            .and_then(Value::as_array)
                            .map(|a| a.len())
                            .unwrap_or(0);
                        if len as i64 != cw * ch {
                            issues.push(format!("{lid}/{li_id}: intGridCsv len {len} != {cw}x{ch}"));
                        }
                    }
                    if let Some(ents) = li.get("entityInstances").and_then(Value::as_array) {
                        for e in ents {
                            if let Some(uid) = e.get("defUid").and_then(Value::as_i64) {
                                if !entity_uids.contains(&uid) {
                                    issues.push(format!("{lid}/{li_id}: entity defUid {uid} has no matching def"));
                                }
                            }
                        }
                    }
                }
            }
            // Best-effort JSON-schema validation (non-fatal warnings).
            let schema_section = match crate::schema::validate(&p.main_file_json()) {
                Ok(warns) if warns.is_empty() => {
                    format!("\nSchema (v{}): no warnings.", crate::schema::schema_version())
                }
                Ok(warns) => {
                    let shown: Vec<String> = warns.iter().take(15).cloned().collect();
                    let more = warns.len().saturating_sub(shown.len());
                    format!(
                        "\nSchema (v{}): {} warning(s){}:\n- {}",
                        crate::schema::schema_version(),
                        warns.len(),
                        if more > 0 {
                            format!(" (showing first {})", shown.len())
                        } else {
                            String::new()
                        },
                        shown.join("\n- "),
                    )
                }
                Err(e) => format!("\nSchema validation unavailable: {e}"),
            };

            let structural = if issues.is_empty() {
                "OK: no structural issues found.".to_string()
            } else {
                format!("Found {} structural issue(s):\n- {}", issues.len(), issues.join("\n- "))
            };
            Ok(format!("{structural}{schema_section}"))
        })
    }

    #[tool(
        description = "Render a level to a PNG and return it as an image (plus a text note). Draws IntGrid value colors, real tileset tiles, and entities; reflects the current in-memory edits. Use this to visually verify a level after editing."
    )]
    fn render_level(&self, Parameters(args): Parameters<RenderLevelArgs>) -> Result<CallToolResult, ErrorData> {
        self.with_project(|p| {
            let opts = RenderOpts {
                scale: args.scale,
                max_px: args.max_px.unwrap_or(1024),
                layers: args.layers,
            };
            let out = render::render_level(p, &args.level, &opts).map_err(|e| err(format!("{e:#}")))?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&out.png);
            let mut note = format!(
                "Rendered '{}' at {}x{}px (scale {:.3}).",
                args.level, out.width, out.height, out.scale
            );
            if !out.warnings.is_empty() {
                note.push_str("\nWarnings:\n- ");
                note.push_str(&out.warnings.join("\n- "));
            }
            Ok(CallToolResult::success(vec![
                Content::text(note),
                Content::image(b64, "image/png"),
            ]))
        })
    }

    #[tool(
        description = "Preview unsaved edits: a semantic diff of the in-memory project vs the .ldtk file on disk (levels added/removed/modified, per-layer content deltas, definition changes). Read-only; call before save_project."
    )]
    fn preview_changes(&self) -> Result<String, ErrorData> {
        self.with_project(|p| {
            let (disk, note) = match p.disk_root() {
                Ok(root) => (root, String::new()),
                Err(e) => (
                    json!({}),
                    format!("(no readable on-disk version: {e:#}; everything below reads as new)\n"),
                ),
            };
            let changes = crate::diff::summarize(&disk, &p.root);
            if changes.is_empty() {
                return Ok(format!("{note}In-memory state matches the file on disk."));
            }
            let shown: Vec<String> = changes.iter().take(40).cloned().collect();
            let more = changes.len().saturating_sub(shown.len());
            let tail = if more > 0 {
                format!("\n… (+{more} more)")
            } else {
                String::new()
            };
            Ok(format!(
                "{note}{} pending change(s) vs {}:\n{}{tail}",
                changes.len(),
                p.path.display(),
                shown.join("\n"),
            ))
        })
    }

    #[tool(
        description = "Undo the most recent mutating tool call (in-memory). Edits remain unsaved until save_project. Up to 20 steps of history are kept."
    )]
    fn undo(&self) -> Result<String, ErrorData> {
        self.with_project(|p| {
            p.undo().map_err(|e| err(format!("{e:#}")))?;
            Ok("Reverted the last change. Call save_project to persist, or redo to re-apply.".to_string())
        })
    }

    #[tool(description = "Re-apply the most recently undone change (in-memory).")]
    fn redo(&self) -> Result<String, ErrorData> {
        self.with_project(|p| {
            p.redo().map_err(|e| err(format!("{e:#}")))?;
            Ok("Re-applied the change. Call save_project to persist.".to_string())
        })
    }

    #[tool(
        description = "Discard ALL unsaved edits by reloading the project from disk. Clears undo/redo history; this cannot itself be undone."
    )]
    fn revert_unsaved(&self) -> Result<String, ErrorData> {
        self.with_project(|p| {
            p.revert().map_err(|e| err(format!("{e:#}")))?;
            Ok(format!("Discarded unsaved edits; reloaded {}.", p.path.display()))
        })
    }

    #[tool(description = "Write the in-memory project back to its .ldtk file on disk.")]
    fn save_project(&self) -> Result<String, ErrorData> {
        self.with_project(|p| {
            p.save().map_err(|e| err(format!("{e:#}")))?;
            Ok(format!("Saved {}", p.path.display()))
        })
    }
}

/// MIME type for a tileset image by extension. `None` for formats we don't serve as images
/// (e.g. `.aseprite`), so they're excluded from the resource list.
fn image_mime(rel: &str) -> Option<&'static str> {
    let lower = rel.to_ascii_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else {
        None
    }
}

// ---- Resource backends (used by the ServerHandler impl) --------------------

impl LdtkServer {
    /// One `ldtk://tileset/{uid}` resource per tileset def with a servable image file.
    fn resource_list(&self) -> Result<ListResourcesResult, ErrorData> {
        let guard = self.state.lock().map_err(|_| err("state lock poisoned"))?;
        let mut resources = Vec::new();
        if let Some(p) = guard.as_ref() {
            if let Some(tilesets) = p
                .root
                .get("defs")
                .and_then(|d| d.get("tilesets"))
                .and_then(Value::as_array)
            {
                for t in tilesets {
                    let (Some(uid), Some(rel)) = (
                        t.get("uid").and_then(Value::as_i64),
                        t.get("relPath").and_then(Value::as_str),
                    ) else {
                        continue;
                    };
                    let Some(mime) = image_mime(rel) else { continue };
                    let ident = t.get("identifier").and_then(Value::as_str).unwrap_or("tileset");
                    let mut raw = RawResource::new(format!("ldtk://tileset/{uid}"), format!("{ident} ({rel})"));
                    raw.mime_type = Some(mime.to_string());
                    resources.push(Annotated::new(raw, None));
                }
            }
        }
        Ok(ListResourcesResult::with_all_items(resources))
    }

    fn resource_templates(&self) -> ListResourceTemplatesResult {
        let templates = vec![
            Annotated::new(RawResourceTemplate::new("ldtk://tileset/{uid}", "Tileset image"), None),
            Annotated::new(
                RawResourceTemplate::new("ldtk://level/{level}/preview.png", "Level preview PNG"),
                None,
            ),
        ];
        ListResourceTemplatesResult::with_all_items(templates)
    }

    fn resource_read(&self, uri: &str) -> Result<ReadResourceResult, ErrorData> {
        let guard = self.state.lock().map_err(|_| err("state lock poisoned"))?;
        let p = guard
            .as_ref()
            .ok_or_else(|| err("no project open; call open_project first"))?;

        if let Some(rest) = uri.strip_prefix("ldtk://tileset/") {
            let uid: i64 = rest.parse().map_err(|_| err(format!("bad tileset uri '{uri}'")))?;
            let rel = p
                .tileset_rel_path(uid)
                .ok_or_else(|| err(format!("tileset {uid} has no image path")))?;
            let mime = image_mime(&rel).unwrap_or("application/octet-stream");
            let path = p.resolve_rel_path(&rel);
            let bytes = std::fs::read(&path).map_err(|e| err(format!("reading {}: {e}", path.display())))?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
            let contents = ResourceContents::blob(b64, uri.to_string()).with_mime_type(mime);
            return Ok(ReadResourceResult::new(vec![contents]));
        }

        if let Some(rest) = uri.strip_prefix("ldtk://level/") {
            let key = rest
                .strip_suffix("/preview.png")
                .ok_or_else(|| err(format!("bad level uri '{uri}'")))?;
            let out = render::render_level(p, key, &RenderOpts::default()).map_err(|e| err(format!("{e:#}")))?;
            let b64 = base64::engine::general_purpose::STANDARD.encode(out.png);
            let contents = ResourceContents::blob(b64, uri.to_string()).with_mime_type("image/png");
            return Ok(ReadResourceResult::new(vec![contents]));
        }

        Err(err(format!("unknown resource uri '{uri}'")))
    }
}

#[rmcp::tool_handler(router = self.tool_router)]
impl ServerHandler for LdtkServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        // Advertise the `tools` and `resources` capabilities during the handshake. Without this,
        // clients won't issue `tools/list` or `resources/list` and the server appears empty.
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions(
            "Edit LDtk (.ldtk) projects to generate game levels. \
             Workflow: open_project -> describe_defs -> create_level / set_intgrid / place_entities -> render_level -> preview_changes -> validate_project -> save_project. \
             Edits stay in memory until save_project; use render_level to see a level, preview_changes to review pending edits, and undo / revert_unsaved to recover from mistakes. \
             Tileset images are exposed as ldtk://tileset/{uid} resources. \
             For tile visuals, edit the IntGrid that drives an AutoLayer rather than painting tiles directly.",
        )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.resource_list()
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        Ok(self.resource_templates())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        self.resource_read(&request.uri)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_summary_maps_grid_size_and_fields() {
        let e = json!({
            "iid": "ent-1",
            "__identifier": "Chest",
            "__grid": [5, 7],
            "px": [80, 112],
            "width": 24,
            "height": 24,
            "__tags": ["loot"],
            "fieldInstances": [
                { "__identifier": "content", "__value": ["Gold", "Trout"] },
                { "__identifier": "requireKey", "__value": true },
            ],
        });
        let s = entity_summary(&e);
        assert_eq!(s.get("iid").and_then(Value::as_str), Some("ent-1"));
        assert_eq!(s.get("identifier").and_then(Value::as_str), Some("Chest"));
        assert_eq!(s.get("cx").and_then(Value::as_i64), Some(5));
        assert_eq!(s.get("cy").and_then(Value::as_i64), Some(7));
        assert_eq!(s.get("width").and_then(Value::as_i64), Some(24));
        assert_eq!(s["fields"]["content"], json!(["Gold", "Trout"]));
        assert_eq!(s["fields"]["requireKey"], json!(true));
        assert_eq!(s["tags"], json!(["loot"]));
    }

    #[test]
    fn entity_summary_tolerates_missing_optional_fields() {
        let e = json!({ "iid": "x", "__identifier": "Mob" });
        let s = entity_summary(&e);
        assert_eq!(s.get("cx").cloned(), Some(Value::Null));
        assert_eq!(s["fields"], json!({}));
    }
}
