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
| `create_level` | Append a new empty level; layer instances are built from the project's layer defs. |
| `set_intgrid` | Set an IntGrid layer via a full `csv` and/or rectangle fills. Clears AutoLayer tiles so LDtk regenerates them. |
| `place_entities` | Place entity instances on an Entity layer using grid coordinates, with optional typed `fields`. |
| `set_entity_fields` | Set typed field values on an existing entity instance (by iid). |
| `set_level_fields` | Set typed custom field values on a level. |
| `paint_tiles` | Paint tiles on a Tile layer by grid coordinate and tile id (pixel `src` computed from tileset geometry). |
| `clear_tiles` | Remove tiles from a Tile layer, entirely or within a grid rectangle. |
| `validate_project` | Authoritative structural checks plus best-effort JSON-schema warnings. |
| `save_project` | Write the in-memory project (and any `.ldtkl` files) back to disk. |

### Typed fields

`place_entities` (via `fields`), `set_entity_fields`, and `set_level_fields` accept JSON values
keyed by field identifier and encode them the way LDtk expects: both the convenience `__value` and
the authoritative `realEditorValues` wrappers (`V_Int`/`V_Float`/`V_Bool`/`V_String`). Supported
types: Int, Float, Bool, String, Multilines, FilePath, Color (`"#rrggbb"`), Enum (validated),
Point (`{cx,cy}`), Tile (`{x,y,w,h}`), and EntityRef (target entity iid). Arrays are supported when
the field def is an array.

Edits stay in memory until you call `save_project`.

### Recommended workflow

`open_project` → `describe_defs` → `create_level` → `set_intgrid` / `place_entities` →
`validate_project` → `save_project`.

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

## Smoke test

```bash
python3 smoke_test.py
```

Drives the server over stdio against the bundled samples and asserts typed entity fields, tile
painting, separate-level-file round-trips, and multi-world editing all work.

## Current limitations

- `paint_tiles` targets `Tiles` layers; AutoLayer visuals should still be driven via IntGrid.
- New levels are appended to the root `levels`, or to the first world in multi-world projects.
- `validate_project`'s JSON-schema pass is best-effort (the official schema is loose); the
  structural checks remain the authoritative gate.
