# ldtk-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server, written in Rust, that lets
an AI agent read and edit [LDtk](https://ldtk.io) (`.ldtk`) projects so it can help generate game
levels.

It operates directly on the `.ldtk` JSON file (the format documented in `JSON_SCHEMA.json`,
currently v1.5.3). It does **not** drive the LDtk GUI. After the agent edits and saves, reload the
project in LDtk to see the changes.

## Build

```bash
cargo build --release
# binary at target/release/ldtk-mcp
```

Requires a recent stable Rust toolchain.

## Tools

| Tool | Purpose |
| --- | --- |
| `open_project` | Load a `.ldtk` file. Must be called first. |
| `list_levels` | List levels with world position and size (across all worlds). |
| `describe_defs` | Layers (incl. IntGrid values), entities (incl. fields), tilesets, enums. Call before generating. |
| `get_level` | Summary of a level and its layer instances (content counts). |
| `get_layer` | Full content of one layer instance: IntGrid CSV, grid tiles, or entities (with decoded fields). |
| `get_entities` | List entity instances on a level (one layer or all) with iid, grid position, tags, and decoded field values. |
| `get_intgrid` | Read an IntGrid layer: dimensions, the row-major `csv`, and value definitions (number to identifier/color). |
| `get_entity` | Fetch a single entity instance by iid, with a decoded `fields` map and the raw `fieldInstances`. |
| `render_level` | Rasterize a level to a PNG (IntGrid colors, real tileset tiles, entities) and return it inline as an image. |
| `create_level` | Append a new empty level; layer instances are built from the project's layer defs. |
| `duplicate_level` | Deep-copy a level (fresh uid/iids) to the next free world position. |
| `move_level` | Set a level's world-space pixel position (`worldX`/`worldY`). |
| `resize_level` | Resize a level; layer instances are reflowed and out-of-bounds tiles/entities are clipped. |
| `delete_level` | Delete a level; external `.ldtkl` files are removed on `save_project`. |
| `create_world` | Create a new empty world (multi-world projects); appends to the root `worlds` array. |
| `set_world_layout` | Update a world's layout and/or grid dimensions (`worldLayout`, `worldGridWidth/Height`). |
| `set_intgrid` | Set an IntGrid layer via a full `csv` and/or rectangle fills. Clears AutoLayer tiles so LDtk regenerates them. |
| `flood_fill_intgrid` | 4-connected flood fill on an IntGrid layer from a start cell, replacing the contiguous same-value region. |
| `place_entities` | Place entity instances on an Entity layer using grid coordinates, with optional typed `fields`. |
| `move_entity` | Move an existing entity instance (by iid) to a new grid cell; pixel position recomputed from the def pivot. |
| `delete_entity` | Delete a single entity instance (by iid) from a level. |
| `set_entity_fields` | Set typed field values on an existing entity instance (by iid). |
| `set_level_fields` | Set typed custom field values on a level. |
| `paint_tiles` | Paint tiles on a Tile layer by grid coordinate and tile id (pixel `src` computed from tileset geometry). |
| `clear_tiles` | Remove tiles from a Tile layer, entirely or within a grid rectangle. |
| `create_layer_def` | Define a new layer (IntGrid/Entities/Tiles/AutoLayer) and backfill an empty instance into every level. |
| `add_intgrid_values` | Add or update IntGrid value definitions on an existing IntGrid layer def (upsert by value; extends the palette). |
| `create_entity_def` | Define a new entity (size, color, tags); optional tileset tile binding for `Tile` rendering. |
| `create_enum` | Define a new enum from a list of value identifiers. |
| `create_tileset_def` | Define a new tileset from an image path + explicit dimensions (grid/padding/spacing). |
| `add_entity_field` | Append a typed field definition to an existing entity def (Enum fields resolve `enum_id`). |
| `add_level_field` | Append a typed field definition to the project-level `levelFields`. |
| `validate_project` | Authoritative structural checks plus best-effort JSON-schema warnings. |
| `preview_changes` | Semantic diff of in-memory edits vs the file on disk; review before saving. |
| `undo` / `redo` | Step backward/forward through mutating edits (in-memory; up to 20 steps). |
| `revert_unsaved` | Discard all unsaved edits by reloading from disk. |
| `save_project` | Write the in-memory project (and any `.ldtkl` files) back to disk. |

### Typed fields

`place_entities` (via `fields`), `set_entity_fields`, and `set_level_fields` accept JSON values
keyed by field identifier and encode them the way LDtk expects: both the convenience `__value` and
the authoritative `realEditorValues` wrappers (`V_Int`/`V_Float`/`V_Bool`/`V_String`). Supported
types: Int, Float, Bool, String, Multilines, FilePath, Color (`"#rrggbb"`), Enum (validated),
Point (`{cx,cy}`), Tile (`{x,y,w,h}`), and EntityRef (target entity iid). Arrays are supported when
the field def is an array.

Edits stay in memory until you call `save_project`.

### Safety: preview, undo, revert

Every mutating tool automatically snapshots the project first, so edits are reversible:

- `preview_changes` shows a semantic diff of the in-memory project against the `.ldtk` file on
  disk (levels added/removed/modified, per-layer content-count deltas, definition changes) — call
  it before `save_project` to review what will be written. For separate-level-file projects the
  on-disk `.ldtkl` bodies are reloaded so per-layer content is compared accurately.
- `undo` / `redo` walk an in-memory history (up to 20 steps). Undone edits stay unsaved until you
  `save_project` again.
- `revert_unsaved` discards all unsaved edits by reloading from disk (and clears the history).

### Visual feedback

`render_level` rasterizes a level to a PNG and returns it inline as image content, so an agent can
*see* the result of its edits (perceive→act→verify). It draws IntGrid cells in their value colors,
real tiles sampled from the decoded tileset images, and entities as their tile sprite or a colored
box. Output is capped to ~1024px on the longest edge by default (override with `scale` / `max_px`),
and only the requested `layers` are drawn when that argument is given.

Tilesets whose image can't be decoded (`.aseprite`, embedded atlases) render as a magenta
placeholder and are reported in the tool's text note — a render never fails outright.

> **AutoLayer caveat:** the editing tools clear `autoLayerTiles` so LDtk regenerates them on load.
> A freshly-edited IntGrid therefore previews as its **value colors**, not the generated tiles,
> until the project is reopened in LDtk.

### Resources

The server exposes tileset images as MCP resources so a client (or the agent) can reference the art
when choosing tile ids:

- `ldtk://tileset/{uid}` — the tileset's image file as a base64 `image/png` (or gif/jpeg) blob.
- `ldtk://level/{level}/preview.png` — a rendered preview of a level (resource template).

### Recommended workflow

`open_project` → `describe_defs` → `create_level` → `set_intgrid` / `place_entities` →
`render_level` → `preview_changes` → `validate_project` → `save_project`.

For tile visuals, prefer editing the **IntGrid** that drives an **AutoLayer** rather than painting
tiles directly — LDtk renders the tiles from the rules automatically on load.

## Register with an MCP client (e.g. Cursor)

Add to `~/.cursor/mcp.json` (or a project `.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "ldtk": {
      "command": "/absolute/path/to/ldtk/ldtk-mcp/target/release/ldtk-mcp"
    }
  }
}
```

## Multi-world and separate level files

- **Multi-world** projects are fully supported: levels in `worlds[].levels` are addressed by
  identifier/iid/uid just like root levels.
- **Separate level files** (`externalLevels` / `.ldtkl`) are loaded into memory on `open_project`
  and rewritten on `save_project` (each `.ldtkl` gets the full level body; the main file keeps the
  stub with `layerInstances: null`).

## Tests

Unit tests cover the pure logic (typed-field encoding, project tree manipulation, schema loading):

```bash
cargo test
```

The end-to-end smoke test drives the server over stdio against the samples in `samples/` and
asserts typed entity fields, tile painting, separate-level-file round-trips, and multi-world
editing all work:

```bash
python3 scripts/smoke_test.py
```

## Current limitations

- `paint_tiles` targets `Tiles` layers; AutoLayer visuals should still be driven via IntGrid.
- New levels are appended to the root `levels`, or to the first world in multi-world projects.
- `validate_project`'s JSON-schema pass is best-effort (the official schema is loose); the
  structural checks remain the authoritative gate.
