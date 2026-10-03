# ldtk-mcp

A [Model Context Protocol](https://modelcontextprotocol.io) server, written in Rust, that lets an
AI agent read and edit [LDtk](https://ldtk.io) (`.ldtk`) projects so it can help generate game
levels.

The server operates directly on the `.ldtk` JSON file, in the format documented in
`JSON_SCHEMA.json` (currently v1.5.3). It does **not** drive the LDtk GUI. After the agent edits
and saves, reload the project in LDtk to see the changes.

## Install

```bash
cargo install --git https://github.com/gazure/ldtk-mcp
```

Installing requires a recent stable Rust toolchain. The crate isn't on crates.io yet, so install
it from Git rather than by name. To install from a clone you're editing, run `cargo install
--path .` from the repository root.

Either form puts a release binary at `~/.cargo/bin/ldtk-mcp`, which a standard rustup setup
already has on your `PATH`.

## Register with an MCP client

The server speaks JSON-RPC over stdio, so any MCP client can launch it as a subprocess.

For Claude Code:

```bash
claude mcp add ldtk ldtk-mcp
```

For Cursor, add the server to `~/.cursor/mcp.json` or to a project `.cursor/mcp.json`:

```json
{
  "mcpServers": {
    "ldtk": {
      "command": "ldtk-mcp"
    }
  }
}
```

If your client reports that the command isn't found, give it the absolute path
(`~/.cargo/bin/ldtk-mcp`, expanded). Clients launched from the desktop rather than a shell don't
always inherit the `PATH` your terminal has.

## Recommended workflow

`open_project` → `describe_defs` → `create_level` → `set_intgrid` / `place_entities` →
`render_level` → `preview_changes` → `validate_project` → `save_project`.

Edits stay in memory until you call `save_project`.

For tile visuals, edit the **IntGrid** that drives an **AutoLayer** rather than painting tiles
directly. LDtk renders the tiles from the rules automatically on load.

## Tools

| Tool | Purpose |
| --- | --- |
| `open_project` | Load a `.ldtk` file. Call this first. |
| `list_levels` | List levels with world position and size, across all worlds. |
| `describe_defs` | Describe layers (including IntGrid values), entities (including fields), tilesets, and enums. Call before generating. |
| `get_level` | Summarize a level and its layer instances, with content counts. |
| `get_layer` | Read a small layer's content: IntGrid CSV, grid tiles, or entities with decoded fields. Large reads return an error with guidance. |
| `get_entities` | Read a page of entities, filtered by layer, identifier, tags, or grid region, with optional field selection. |
| `get_intgrid` | Read an IntGrid region as row-major cells, run-length encoding, or sparse cells, with dimensions and value definitions. |
| `get_entity` | Fetch a single entity instance by iid, with a decoded `fields` map and the raw `fieldInstances`. |
| `render_level` | Rasterize a level to a PNG — IntGrid colors, real tileset tiles, entities — and return it inline as an image. |
| `create_level` | Append a new empty level, building layer instances from the project's layer defs. |
| `duplicate_level` | Deep-copy a level, with fresh uid and iids, to the next free world position. |
| `move_level` | Set a level's world-space pixel position (`worldX` and `worldY`). |
| `resize_level` | Resize a level, reflowing layer instances and clipping out-of-bounds tiles and entities. |
| `delete_level` | Delete a level. `save_project` removes any external `.ldtkl` file. |
| `create_world` | Create a new empty world in a multi-world project, appending to the root `worlds` array. |
| `set_world_layout` | Update a world's layout or grid dimensions (`worldLayout`, `worldGridWidth`, `worldGridHeight`). |
| `set_intgrid` | Set an IntGrid layer from a full `csv`, from rectangle fills, or both. Clears AutoLayer tiles so LDtk regenerates them. |
| `flood_fill_intgrid` | Run a 4-connected flood fill on an IntGrid layer from a start cell, replacing the contiguous same-value region. |
| `place_entities` | Place entity instances on an Entity layer using grid coordinates, with optional typed `fields`. |
| `move_entity` | Move an existing entity instance, by iid, to a new grid cell. Recomputes the pixel position from the def pivot. |
| `delete_entity` | Delete a single entity instance, by iid, from a level. |
| `set_entity_fields` | Set typed field values on an existing entity instance, by iid. |
| `set_level_fields` | Set typed custom field values on a level. |
| `paint_tiles` | Paint tiles on a Tile layer by grid coordinate and tile id. Computes the pixel `src` from the tileset geometry. |
| `clear_tiles` | Remove tiles from a Tile layer, either entirely or within a grid rectangle. |
| `create_layer_def` | Define a new layer — IntGrid, Entities, Tiles, or AutoLayer — and backfill an empty instance into every level. |
| `add_intgrid_values` | Add or update IntGrid value definitions on an existing IntGrid layer def. Upserts by value, extending the palette. |
| `create_entity_def` | Define a new entity with size, color, and tags, plus an optional tileset tile binding for `Tile` rendering. |
| `create_enum` | Define a new enum from a list of value identifiers. |
| `create_tileset_def` | Define a new tileset from an image path and explicit dimensions: grid, padding, and spacing. |
| `add_entity_field` | Append a typed field definition to an existing entity def. Enum fields resolve `enum_id`. |
| `add_level_field` | Append a typed field definition to the project-level `levelFields`. |
| `validate_project` | Run authoritative structural checks plus best-effort JSON-schema warnings. |
| `preview_changes` | Show a semantic diff of in-memory edits against the file on disk. Review this before saving. |
| `undo` / `redo` | Step backward or forward through mutating edits, in memory, up to 20 steps. |
| `revert_unsaved` | Discard all unsaved edits by reloading from disk. |
| `save_project` | Write the in-memory project, and any `.ldtkl` files, back to disk. |

### Selective reads

Use `get_level` for content counts before reading a layer. `get_layer`, `get_entities`, and
`get_intgrid` return compact JSON with a 64 KiB response limit. Oversized reads return an error
that explains how to narrow the query; they never silently truncate content.

`get_entities` returns up to 100 entities by default. Set `limit` to a value from 1 to 500.
The limit applies across all selected layers. Combine these optional filters:

- `layer`: Layer identifier or iid. Omit to search all entity layers.
- `identifier`: Exact entity definition identifier, such as `Chest`.
- `tags`: Tags that each returned entity must have. An empty array matches all tags.
- `rect`: `{cx, cy, w, h}` in each layer's grid coordinates. The filter tests the entity's
  anchor cell, with exclusive right and bottom edges. Width and height must be positive.
- `fields`: Custom field identifiers to return. Omit for all fields, or use `[]` for none.

For example, pass these arguments to `get_entities`:

```json
{"level":"Cave_01","identifier":"Chest","tags":["loot"],"limit":20,"fields":["content"]}
```

The response is an object with `layers`, `total`, `offset`, `limit`, `returned`, and `next_offset`.
`layers` contains `{layer, entities}` groups for the current page. `total` counts all matching
entities before pagination. To read the next page, pass `next_offset` as `offset` with the same
filters. A null `next_offset` marks the end. Results follow project layer and entity order;
edits between calls can shift pages. Layers with no entities in the page are omitted.

**Response format change:** `get_entities` previously returned the groups as a top-level array.
Clients must now read those groups from `response.layers` and follow pagination.

`get_intgrid` accepts an optional `rect` fully inside the layer and an `encoding`:

| Encoding | Response data | Meaning |
| --- | --- | --- |
| `csv` (default) | `csv: [value, ...]` | Row-major cells in the selected region. |
| `rle` | `runs: [[value, count], ...]` | Row-major runs that can span rows within the selected region. |
| `sparse` | `cells: [[cx, cy, value], ...]` | Nonzero cells in absolute layer coordinates. Omitted cells in the region are zero. |

For example, pass these arguments to `get_intgrid`:

```json
{"level":"Cave_01","layer":"Collision","rect":{"cx":8,"cy":4,"w":16,"h":12},"encoding":"rle"}
```

The response includes the region origin (`cx`, `cy`), region dimensions (`cWid`, `cHei`), full
layer dimensions (`layerCWid`, `layerCHei`), `gridSize`, and value definitions (`values`).
Without `rect`, the region covers the full layer. Each read supports up to 4096 cells, runs,
or sparse entries, depending on the encoding. For large uniform or mostly empty grids, use
`rle` or `sparse`. If the encoded result is still too large, request smaller regions.

`set_intgrid.csv` still replaces a **whole layer**. A cropped CSV or compact encoding is not
a replacement payload; use `set_intgrid.rects` for rectangle edits.

`get_layer` supports at most 4096 items per included content array, in addition to the byte
limit. For large layers, use the selective reads, `get_level` for counts, or `render_level`
for visual inspection. AutoLayer tiles count toward the limit only when requested.

### Typed fields

`place_entities` (through `fields`), `set_entity_fields`, and `set_level_fields` accept JSON values
keyed by field identifier and encode them the way LDtk expects: both the convenience `__value` and
the authoritative `realEditorValues` wrappers (`V_Int`, `V_Float`, `V_Bool`, and `V_String`).

The supported types are Int, Float, Bool, String, Multilines, FilePath, Color (`"#rrggbb"`), Enum
(validated), Point (`{cx,cy}`), Tile (`{x,y,w,h}`), and EntityRef (the target entity iid). Arrays
are supported when the field def is an array.

### Safety: preview, undo, and revert

Every mutating tool snapshots the project first, so edits are reversible:

- `preview_changes` shows a semantic diff of the in-memory project against the `.ldtk` file on
  disk: levels added, removed, or modified, per-layer content-count deltas, and definition
  changes. Call it before `save_project` to review what the server writes. For separate-level-file
  projects, the server reloads the on-disk `.ldtkl` bodies so per-layer content compares
  accurately.
- `undo` and `redo` walk an in-memory history of up to 20 steps. Undone edits stay unsaved until
  you call `save_project` again.
- `revert_unsaved` discards all unsaved edits by reloading from disk, and clears the history.

### Visual feedback

`render_level` rasterizes a level to a PNG and returns it inline as image content, so an agent can
*see* the result of its edits — a perceive, act, verify loop. It draws IntGrid cells in their value
colors, real tiles sampled from the decoded tileset images, and entities as their tile sprite or a
colored box. Output caps at roughly 1024px on the longest edge by default; override that with
`scale` or `max_px`. When you pass the `layers` argument, the renderer draws only those layers.

Tilesets whose image can't be decoded, such as `.aseprite` files and embedded atlases, render as a
magenta placeholder and appear in the tool's text note.

Each source or output canvas is limited to 64 MiB of RGBA pixels (for example, 4096 × 4096).
Larger renders return an error before allocating the canvases. Lowering `scale` or `max_px`
reduces the output size, but the source level must also fit the limit. This limit covers canvas
buffers, not total rendering memory, which also includes tileset images and PNG encoding.

Note: the editing tools clear `autoLayerTiles` so that LDtk regenerates them on load. A
freshly-edited IntGrid therefore previews as its **value colors**, not as the generated tiles,
until you reopen the project in LDtk.

### Resources

The server exposes tileset images as MCP resources, so a client or the agent can reference the art
when choosing tile ids:

- `ldtk://tileset/{uid}` — the tileset's image file as a base64 `image/png`, `image/gif`, or
  `image/jpeg` blob.
- `ldtk://level/{level}/preview.png` — a rendered preview of a level, as a resource template.

## Multi-world and separate level files

- **Multi-world** projects are fully supported. You address levels in `worlds[].levels` by
  identifier, iid, or uid, exactly as you address root levels.
- **Separate level files** (`externalLevels` and `.ldtkl`) load into memory on `open_project` and
  are rewritten on `save_project`. Each `.ldtkl` file gets the full level body, and the main file
  keeps the stub with `layerInstances: null`.

## AutoLayer rules

The server reads and renders AutoLayers, but it doesn't author their rules yet. If you need to
inspect or hand-edit them, this is the shape LDtk writes, confirmed against
`samples/AutoLayers_1_basic.ldtk`.

Rules live on the layer *definition*, not on the layer instance:

```
layerDef.autoRuleGroups: [{ uid, name, color, icon, active, isOptional, rules: [...],
                            usesWizard, requiredBiomeValues, biomeRequirementMode }]
rule: { uid, active, size, tileRectsIds: [[tileId,...]], alpha, chance, breakOnMatch,
        pattern: [i64; size*size], flipX, flipY, xModulo, yModulo, xOffset, yOffset,
        tileXOffset, tileYOffset, tileRandom{X,Y}{Min,Max}, checker, tileMode ("Single"|"Stamp"),
        pivotX, pivotY, outOfBoundsValue, invalidated, perlinActive, perlinSeed, perlinScale,
        perlinOctaves }
```

`pattern` is a row-major `size` × `size` window centered on the cell being tested, where `size` is
odd — 1, 3, 5, or 7. Each entry matches an IntGrid value: `0` matches anything, `+v` requires the
cell to equal `v`, and `-v` requires it not to equal `v`.

LDtk evaluates these rules on load and writes the result into each layer instance's
`autoLayerTiles`. Nothing in this server evaluates them, so a rule you add by hand shows up only
after LDtk reopens the project.

## Development

Unit tests cover the pure logic: typed-field encoding, project tree manipulation, and schema
loading.

```bash
cargo test
```

The end-to-end smoke test drives the server over stdio against the samples in `samples/`, and
asserts that typed entity fields, tile painting, separate-level-file round-trips, and multi-world
editing all work. Build the debug binary first, because the script runs `target/debug/ldtk-mcp`:

```bash
cargo build
python3 scripts/smoke_test.py
python3 scripts/read_queries_test.py
```

Changes must pass both lint gates. Formatting uses nightly rustfmt, because `rustfmt.toml` sets
options that are still nightly-only, such as `imports_granularity` and `group_imports`:

```bash
cargo +nightly fmt --all
cargo +stable clippy --all-targets --all-features
```

## Current limitations

- `paint_tiles` targets `Tiles` layers. Drive AutoLayer visuals through the IntGrid instead.
- New levels append to the root `levels`, or to the first world in multi-world projects.
- The JSON-schema pass in `validate_project` is best-effort, because the official schema is loose.
  The structural checks remain the authoritative gate.
- The server exposes no MCP prompts, and it can't author AutoLayer rules.

## Roadmap

- **MCP prompts.** Parameterized workflow guides served through `list_prompts` and `get_prompt`:
  walking a new level from `open_project` to `save_project`, adding an IntGrid collision layer
  plus the AutoLayer it drives, and reviewing edits before a save.
- **AutoLayer rule authoring.** Tools to create rule groups and rules against the shape documented
  earlier, so an agent can set up "draw tile T wherever the IntGrid holds value V" without opening
  LDtk.
- **In-process rule evaluation** (stretch). Computing `autoLayerTiles` from the IntGrid and the
  rules would let `render_level` show real tiles instead of IntGrid value colors, closing the gap
  described in *Visual feedback*.

## License

Released under the [MIT License](LICENSE).
