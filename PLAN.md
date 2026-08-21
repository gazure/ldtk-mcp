# Implementation plan — remaining work

This plan targets the architecture in `src/`: a single in-memory `Project { root: Value }`, an
`LdtkServer` with `#[tool_router]` and `#[rmcp::tool_handler]`, and tools that return
`Result<String, ErrorData>` through the `with_project` helper.

## Shipped

- **Tier 5 — safety.** `preview_changes` (semantic diff in `src/diff.rs`), auto-snapshotting on
  every mutating tool, `undo`, `redo`, and `revert_unsaved`. The undo history caps at 20 steps.
- **Resources.** `ldtk://tileset/{uid}` and `ldtk://level/{level}/preview.png` are served from a
  hand-written `impl ServerHandler` alongside the generated `call_tool` and `list_tools`.
- **Tier 4 — visual feedback.** `render_level` and the pure-Rust rasterizer in `src/render.rs`:
  IntGrid value colors, real tiles blitted from decoded tilesets with flip handling, entity
  sprites and boxes, magenta placeholders for undecodable tilesets, and PNG encoding.

## Remaining

Build prompts first — they're a small, self-contained surface — then AutoLayer rule authoring.

### MCP prompts (workflow guides)

`get_prompt` returns a `GetPromptResult` of guidance messages, parameterized and argument-driven:

- `new_level` (args: `identifier`, `kind` = platformer or topdown, `width`, `height`) — walks
  `open_project` → `describe_defs` → `create_level` → `set_intgrid` / `place_entities` →
  `validate_project` → `save_project`.
- `add_collision_layer` — create an IntGrid layer def plus an AutoLayer driven by it.
- `paint_with_autolayer` — the "edit the IntGrid, let the rules render" workflow. Ties into the
  AutoLayer rule work.
- `review_before_save` — call `preview_changes`, then `validate_project`, then `save_project`.

Enable the capability in `get_info` with `.enable_prompts()`, and hand-write `list_prompts` and
`get_prompt` in the existing `impl ServerHandler` block, next to the resource methods. Keep the
prompt text in a `src/prompts.rs` table — id, description, args, render fn — so `list_prompts` and
`get_prompt` share one source of truth.

Tests: extend `scripts/smoke_test.py` with `prompts/list` and `prompts/get` round-trips, and unit
test that every advertised prompt renders with its declared arguments.

---

## AutoLayer rule authoring

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
`checker`, and defer Stamp, Perlin, and modulo. Ship the authoring tools first, and gate the
evaluator behind a follow-up.

### Tests
- `add_auto_rule` rejects mismatched `pattern`/`size` and even sizes.
- Round-trip: author a group+rule, serialize, re-read, assert structure matches LDtk's shape
  (compare against a sample rule's key set).
- If the evaluator lands: a 3×3 wall-border IntGrid produces the expected `autoLayerTiles`.

---

## Cross-cutting

- **Quality gate**: every change must pass `cargo +nightly fmt` and `cargo +stable clippy`, per
  `CLAUDE.md`, plus `cargo test` and `python3 scripts/smoke_test.py`.
- **Sample assets**: rule tests need a small tileset PNG. Add one under `samples/`, and keep test
  scripts in `scripts/`.
- **README**: add each new tool to the tool table, document the prompts, and drop the "no prompts,
  no rule authoring" note from *Current limitations* as each lands.
- **`get_info` instructions**: extend the workflow hint as new tools land.
