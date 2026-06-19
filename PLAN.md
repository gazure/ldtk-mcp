# Implementation Plan — Tiers 4–6, Resources & Prompts

Status: planning. Targets the existing architecture in `src/` (single in-memory
`Project { root: Value }`, `LdtkServer` with `#[tool_router]` + `#[rmcp::tool_handler]`,
tools returning `Result<String, ErrorData>` via the `with_project` helper).

Recommended build order: **Tier 5 (safety) → Resources/Prompts → Tier 4 (visual) → Tier 6 (rules)**.
Rationale: safety is cheap and de-risks everything after it; resources are a small surface that
Tier 4 partly reuses; Tier 4 is the headline feature; Tier 6 is the most complex and benefits from
the preview existing so its output can be eyeballed.

---

## Tier 5 — Safety: pending-diff preview + snapshot/rollback

Smallest, highest-leverage. No new dependencies.

### Data model (`project.rs`)
Add to `Project`:
- `baseline: Value` — clone of `root` as of last load/save. Set in `load()` (after
  `merge_external_levels`) and reset at the end of `save()`.
- `undo_stack: Vec<Value>` — capped (e.g. 25) ring of pre-mutation `root` snapshots.
- `redo_stack: Vec<Value>` — optional; populated by `undo`, cleared on any new mutation.

### Auto-snapshot
Wrap mutating tools so a snapshot is pushed before they run. Cleanest hook: a
`with_project_mut(&self, f)` variant that clones `root` into `undo_stack` (truncating to the cap)
before calling `f`, and leaves read-only tools on the existing `with_project`. Migrate the ~18
mutating tools to it. Avoid double-snapshotting within a single tool call.

### New tools
- `preview_changes` — semantic diff of in-memory state vs the **on-disk file** (re-read it; fall
  back to `baseline` if unreadable). Output is a human-readable summary, not raw JSON:
  - levels added / removed / renamed,
  - per-level per-layer deltas: IntGrid non-zero cell count Δ, gridTile count Δ, entity count Δ,
  - defs added/removed (layers, entities, enums, tilesets, fields).
  Implement a `fn diff_summary(old: &Value, new: &Value) -> Vec<String>` in a new `src/diff.rs`,
  reusing the counting logic already in `get_level`. Pure function → easy unit tests.
- `undo` — pop `undo_stack` into `root` (push current onto `redo_stack`); report what reverted.
- `redo` — inverse.
- `revert_unsaved` — restore `root` from `baseline` (discard all edits since last save). Clears
  both stacks.

### Tests
- `diff_summary` over hand-built old/new trees (level add, cell-count change, def add).
- undo/redo restores byte-identical `root`; cap evicts oldest.

---

## MCP Resources & Prompts

rmcp 0.16 exposes overridable `ServerHandler` methods: `list_resources`, `read_resource`,
`list_resource_templates`, `list_prompts`, `get_prompt`. The `#[tool_handler]` macro only generates
`call_tool`/`list_tools`, so these can be hand-written in the **same** `impl ServerHandler` block.
Enable capabilities in `get_info`:
`ServerCapabilities::builder().enable_tools().enable_resources().enable_prompts().build()`.

### Resources (static + templated)
- `ldtk://project.json` — live project (`Project::main_file_json()`), `text`, mime
  `application/json`. Reflects unsaved edits → lets clients inspect state without a tool call.
- `ldtk://schema.json` — the bundled `JSON_SCHEMA.json` (`include_str!`), `text`.
- Resource template `ldtk://tileset/{uid}` — tileset image as a **blob** (base64, `image/png`);
  read the file at `tileset.relPath` resolved against the project dir. Shared with Tier 4.
- Optional template `ldtk://level/{id}/preview.png` — renders via the Tier 4 rasterizer (defer
  until Tier 4 lands).

`read_resource` parses the URI, locks state, returns `ReadResourceResult` with the right
`ResourceContents` (`text` vs `BlobResourceContents`). `list_resources` enumerates the two static
URIs; `list_resource_templates` advertises the tileset/level templates.

### Prompts (workflow guides)
`get_prompt` returns a `GetPromptResult` of guidance messages. Parametrized, argument-driven:
- `new_level` (args: `identifier`, `kind` = platformer|topdown, `width`, `height`) — walks
  open→describe_defs→create_level→set_intgrid/place_entities→validate→save.
- `add_collision_layer` — create an IntGrid layer def + an AutoLayer driven by it.
- `paint_with_autolayer` — the "edit IntGrid, let rules render" workflow (ties into Tier 6).
- `review_before_save` — call `preview_changes`, then `validate_project`, then `save_project`.

Keep prompt text in a `src/prompts.rs` table (id, description, args, render fn) so `list_prompts`
and `get_prompt` share one source of truth.

### Tests
- `read_resource` for `project.json`/`schema.json` returns non-empty text with correct mime.
- URI parsing rejects unknown schemes/uids.
- Extend `scripts/smoke_test.py` with `resources/list`, `resources/read`, `prompts/list`,
  `prompts/get` round-trips.

---

## Tier 4 — Visual feedback: level-preview PNG + tileset images

The differentiator. Pure-Rust raster, no native/GPU deps.

### Dependencies
- `png` (pure Rust; pulls `miniz_oxide`, also pure Rust) for both **decoding** tileset source
  images and **encoding** the preview. Single dependency covers both directions.

### New module `src/render.rs`
Software rasterizer over an RGBA `Vec<u8>` framebuffer.

1. **Target size** — render at `scale` px per grid cell (param, default chosen so the long edge
   caps at ~1024px; expose `max_px` too). Nearest-neighbor; no AA needed for pixel art.
2. **Background** — fill with level `__bgColor` (hex → RGBA via the existing `fields::hex_to_int`).
3. **Layers, bottom-to-top** — `layerInstances` is top-first, so iterate in reverse. Apply
   `__opacity` per layer.
   - **IntGrid**: fill each non-zero cell with its `intGridValues` color (from the layer def; reuse
     `intgrid_value_defs`).
   - **Tiles / AutoLayer**: blit real pixels from the decoded tileset. For each tile in
     `gridTiles` / `autoLayerTiles`, copy the `src`→`px` rect honoring flip bits `f`
     (0/1/2/3 = none/X/Y/both) with alpha compositing. Cache decoded tilesets by uid in a
     `HashMap<i64, DecodedImage>` for the duration of the render.
   - **Entities**: filled rect in `__smartColor` (fallback to def `color`); if the def has a
     `tileRect`, blit that tile instead. Optional 1px border for legibility.
4. **Missing/undecodable tileset** → draw a flat magenta placeholder cell and continue (never
   fail the whole render); collect a `warnings: Vec<String>`.
5. **Encode** framebuffer → PNG bytes → base64.

### New tool `render_level`
Args: `level`, optional `scale`/`max_px`, optional `layers` filter.
Returns image content (the tool fn returns `Vec<Content>` / `CallToolResult` with
`Content::image(base64, "image/png")`) plus a short text note (dimensions, scale, any warnings).
This is the perceive→act→verify payload — the agent sees the result of its edits inline.

### Tileset images as resources
Implemented in the Resources section above (`ldtk://tileset/{uid}` blob). The decoder added here is
reused there.

### Notes / limitations to document
- AutoLayer tiles only appear in the preview if `autoLayerTiles` is populated. Our edit tools clear
  it (LDtk regenerates on load), so a freshly-edited IntGrid won't show auto-tiles **until** either
  LDtk regenerates them or Tier 6's optional in-process evaluator runs. Call this out in README.
- IntGrid color fills give a useful structural preview even without tiles.

### Tests
- Geometry/flip math unit-tested on a tiny synthetic tileset (decode → blit → known pixels).
- PNG round-trips (encode then decode, assert dimensions + a few sampled pixels).
- Smoke test: `render_level` over a sample returns a valid PNG (magic-byte check, non-trivial size).

---

## Tier 6 — AutoLayer rule authoring (last, most complex)

Concrete shape confirmed from `samples/AutoLayers_1_basic.ldtk`:

```
layerDef.autoRuleGroups: [{ uid, name, color, icon, active, isOptional, rules: [...],
                            usesWizard, requiredBiomeValues, biomeRequirementMode }]
rule: { uid, active, size, tileRectsIds: [[tileId,...]], alpha, chance, breakOnMatch,
        pattern: [i64; size*size], flipX, flipY, xModulo, yModulo, xOffset, yOffset,
        tileXOffset, tileYOffset, tileRandom{X,Y}{Min,Max}, checker, tileMode ("Single"|"Stamp"),
        pivotX, pivotY, outOfBoundsValue, invalidated, perlinActive, perlinSeed, perlinScale,
        perlinOctaves }
```

`pattern` is row-major `size*size`: `0` = any, `+v` = cell IntGrid value must equal `v`,
`-v` = must **not** equal `v`. `size` is odd (1/3/5/7), centered on the target cell.

### `project.rs` methods
- `create_rule_group(layer, name) -> uid` — append to the layer def's `autoRuleGroups`
  (validate the layer is IntGrid or AutoLayer; allocate uid via `alloc_uid`).
- `add_auto_rule(layer, group, spec) -> uid` — build a rule with **all** schema fields defaulted
  (mirroring the field-default pattern already used by `build_field_def` / `create_layer_def`),
  overriding from `spec`: `size`, `pattern`, `tile_ids` (→ `tileRectsIds`), `chance`, `flipX/Y`,
  `checker`, `tileMode`, `breakOnMatch`. Validate `pattern.len() == size*size` and `size` odd.
- `delete_rule` / `delete_rule_group`, and `set_rule_active` / `set_group_active`.
- A `tileset_def_uid` must be bound to the layer (auto layers use `autoTilesetDefUid`); validate
  referenced tile ids fit the tileset.

### New tools
- `create_rule_group` (layer, name).
- `add_auto_rule` (layer, group, size, pattern, tile_ids, chance?, flip?, checker?, tile_mode?,
  break_on_match?).
- `list_rules` (layer) — enumerate groups/rules for inspection.
- `delete_rule`, `set_rule_active` — editing/toggling.
- Convenience helper documented in prompts: "place tile T wherever IntGrid value == V" expands to
  `size:1, pattern:[V], tile_ids:[[T]]`.

### Rendering interaction
Authoring rules does **not** populate `autoLayerTiles` (LDtk does that on load). For the preview to
reflect new rules without round-tripping through LDtk, an **optional in-process rule evaluator**
could compute `autoLayerTiles` from the IntGrid + rules. Scope this as a **stretch goal / v2** —
implement pattern matching for `size` 1/3, `Single` tile mode, `chance`, `flip`, `breakOnMatch`,
`checker`; defer Stamp/Perlin/modulo. Ship Tier 6 authoring first; gate the evaluator behind a
follow-up.

### Tests
- `add_auto_rule` rejects mismatched `pattern`/`size` and even sizes.
- Round-trip: author a group+rule, serialize, re-read, assert structure matches LDtk's shape
  (compare against a sample rule's key set).
- If the evaluator lands: a 3×3 wall-border IntGrid produces the expected `autoLayerTiles`.

---

## Cross-cutting

- **Quality gate**: every change must pass `cargo +nightly fmt` and `cargo +stable clippy`
  (per CLAUDE.md), plus `cargo test` and `python3 scripts/smoke_test.py`.
- **Sample assets**: Tier 4/6 tests need a tiny tileset PNG — add one under `samples/`
  (per CLAUDE.md: sample levels in `samples/`, test scripts in `scripts/`).
- **README**: add new tools to the tool table, document resources/prompts, and note the
  auto-tile preview caveat.
- **`get_info` instructions**: extend the workflow hint to mention `render_level` (verify loop) and
  `preview_changes`/`undo` (safety).
```
