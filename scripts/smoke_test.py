#!/usr/bin/env python3
"""End-to-end stdio tests for ldtk-mcp against the bundled LDtk samples.

Covers: level creation + IntGrid, typed entity fields, level fields, tile painting,
separate level files (.ldtkl) round-trip, and multi-world editing.
"""

import base64
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SAMPLES = os.path.join(ROOT, "samples")
# Support assets (atlas images, .ldtkl level bodies) live in a nested dir.
SUPPORT = os.path.join(SAMPLES, "samples")
BIN = os.path.join(ROOT, "target/debug/ldtk-mcp")

PASS, FAIL = 0, 0


def check(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  PASS {name}")
    else:
        FAIL += 1
        print(f"  FAIL {name} {detail}")


class Session:
    def __init__(self):
        self.proc = subprocess.Popen(
            [BIN],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        self._send(
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "smoke", "version": "0"},
                },
            },
            True,
        )
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"}, False)
        self._id = 1

    def _send(self, obj, expect):
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()
        if expect:
            return json.loads(self.proc.stdout.readline())

    def call(self, name, args):
        self._id += 1
        resp = self._send(
            {
                "jsonrpc": "2.0",
                "id": self._id,
                "method": "tools/call",
                "params": {"name": name, "arguments": args},
            },
            True,
        )
        if "error" in resp:
            return ("ERROR", resp["error"]["message"])
        return ("OK", resp["result"]["content"][0]["text"])

    def rpc(self, method, params):
        """Raw JSON-RPC request; returns the full response dict (result or error)."""
        self._id += 1
        return self._send(
            {"jsonrpc": "2.0", "id": self._id, "method": method, "params": params},
            True,
        )

    def call_full(self, name, args):
        """tools/call returning the full result dict (for inspecting non-text content)."""
        return self.rpc("tools/call", {"name": name, "arguments": args})

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=5)


def workdir():
    return tempfile.mkdtemp()


def copy_into(dst_dir, *rel_from_samples):
    """Copy files/dirs from SAMPLES into dst_dir preserving names; return first dst path."""
    first = None
    for rel in rel_from_samples:
        src = os.path.join(SAMPLES, rel)
        dst = os.path.join(dst_dir, os.path.basename(rel))
        if os.path.isdir(src):
            shutil.copytree(src, dst)
        else:
            shutil.copy(src, dst)
        if first is None:
            first = dst
    return first


def find_layer(proj, kind):
    for l in proj["defs"]["layers"]:
        if l["__type"] == kind:
            return l["identifier"]
    return None


def test_typed_entity_fields():
    print("Typed entity fields (Entities.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Entities.ldtk")
    proj = json.load(open(f))
    level = proj["levels"][0]["identifier"]
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        status, msg = s.call(
            "place_entities",
            {
                "level": level,
                "layer": "GameEntities",
                "entities": [
                    {
                        "identifier": "Chest",
                        "cx": 5,
                        "cy": 5,
                        "fields": {"content": ["Gold", "Trout"], "requireKey": True},
                    }
                ],
            },
        )
        check("place_entities w/ fields", status == "OK", msg)
        # Read the entities back through the new query-depth tool before saving.
        status, listing = s.call(
            "get_entities", {"level": level, "layer": "GameEntities"}
        )
        check("get_entities runs", status == "OK", listing)
        if status == "OK":
            data = json.loads(listing)
            ents = [e for grp in data for e in grp["entities"]]
            chest = next(
                (
                    e
                    for e in ents
                    if e["identifier"] == "Chest" and e["cx"] == 5 and e["cy"] == 5
                ),
                None,
            )
            check("get_entities returns placed chest", chest is not None, listing[:200])
            if chest:
                check(
                    "get_entities decodes content field",
                    chest["fields"].get("content") == ["Gold", "Trout"],
                    chest["fields"],
                )
                check("get_entities exposes iid", bool(chest.get("iid")), chest)
        # Tier 2.5 single-instance ops: operate on a throwaway entity so the (5,5)
        # Chest stays intact for the on-disk field assertions below.
        s.call(
            "place_entities",
            {
                "level": level,
                "layer": "GameEntities",
                "entities": [{"identifier": "Chest", "cx": 1, "cy": 1}],
            },
        )
        st, listing = s.call("get_entities", {"level": level, "layer": "GameEntities"})
        tmp = None
        if st == "OK":
            ents = [e for grp in json.loads(listing) for e in grp["entities"]]
            tmp = next((e for e in ents if e["cx"] == 1 and e["cy"] == 1), None)
        if tmp and tmp.get("iid"):
            iid = tmp["iid"]
            st, msg = s.call(
                "move_entity", {"level": level, "entity_iid": iid, "cx": 8, "cy": 9}
            )
            check("move_entity", st == "OK", msg)
            st, got = s.call("get_entity", {"level": level, "entity_iid": iid})
            if st == "OK":
                g = json.loads(got)
                check(
                    "move_entity updates grid",
                    g["cx"] == 8 and g["cy"] == 9,
                    (g.get("cx"), g.get("cy")),
                )
            st, msg = s.call("delete_entity", {"level": level, "entity_iid": iid})
            check("delete_entity", st == "OK", msg)
            st, got = s.call("get_entity", {"level": level, "entity_iid": iid})
            check("entity gone after delete", st == "ERROR", got)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    saved = json.load(open(f))
    lvl = next(l for l in saved["levels"] if l["identifier"] == level)
    ents = next(
        li for li in lvl["layerInstances"] if li["__identifier"] == "GameEntities"
    )["entityInstances"]
    chest = next(
        e for e in ents if e["__grid"] == [5, 5] and e["__identifier"] == "Chest"
    )
    fields = {fi["__identifier"]: fi for fi in chest["fieldInstances"]}
    check(
        "content __value",
        fields["content"]["__value"] == ["Gold", "Trout"],
        fields["content"]["__value"],
    )
    check(
        "content realEditorValues",
        fields["content"]["realEditorValues"]
        == [
            {"id": "V_String", "params": ["Gold"]},
            {"id": "V_String", "params": ["Trout"]},
        ],
        fields["content"]["realEditorValues"],
    )
    check("requireKey __value", fields["requireKey"]["__value"] is True)
    check(
        "requireKey realEditorValues",
        fields["requireKey"]["realEditorValues"]
        == [{"id": "V_Bool", "params": [True]}],
        fields["requireKey"]["realEditorValues"],
    )


def test_paint_tiles():
    print("Tile painting (Typical_TopDown_example.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Typical_TopDown_example.ldtk")
    proj = json.load(open(f))
    # Find a Tiles layer instance with a tileset, plus its level.
    target = None
    for lvl in proj["levels"]:
        for li in lvl.get("layerInstances") or []:
            if li["__type"] == "Tiles" and li.get("__tilesetDefUid"):
                target = (lvl["identifier"], li["__identifier"])
                break
        if target:
            break
    if not target:
        check("tiles layer available", False, "no Tiles layer with tileset found")
        return
    level, layer = target
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        status, msg = s.call(
            "paint_tiles",
            {
                "level": level,
                "layer": layer,
                "replace": True,
                "tiles": [{"cx": 0, "cy": 0, "t": 0}, {"cx": 1, "cy": 0, "t": 1}],
            },
        )
        check("paint_tiles", status == "OK", msg)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    saved = json.load(open(f))
    lvl = next(l for l in saved["levels"] if l["identifier"] == level)
    tiles = next(li for li in lvl["layerInstances"] if li["__identifier"] == layer)[
        "gridTiles"
    ]
    check("two tiles painted", len(tiles) == 2, len(tiles))
    check("tile ids", sorted(t["t"] for t in tiles) == [0, 1])
    check(
        "tile has src/px/d",
        all(len(t["src"]) == 2 and len(t["px"]) == 2 and "d" in t for t in tiles),
    )


def test_external_levels():
    print("Separate level files round-trip (SeparateLevelFiles.ldtk)")
    wd = workdir()
    f = copy_into(wd, "SeparateLevelFiles.ldtk")
    # The .ldtkl bodies live in the nested support dir; place them beside the .ldtk
    # so the relative externalRelPath resolves.
    shutil.copytree(
        os.path.join(SUPPORT, "SeparateLevelFiles"),
        os.path.join(wd, "SeparateLevelFiles"),
    )
    proj = json.load(open(f))
    level = proj["levels"][0]["identifier"]
    ext_rel = proj["levels"][0]["externalRelPath"]
    intgrid = find_layer(proj, "IntGrid")
    # A different external level we'll delete, to verify its .ldtkl is unlinked on save.
    victim = proj["levels"][1]["identifier"]
    victim_rel = proj["levels"][1]["externalRelPath"]
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        status, msg = s.call(
            "set_intgrid",
            {
                "level": level,
                "layer": intgrid,
                "rects": [{"cx": 0, "cy": 0, "w": 3, "h": 3, "value": 1}],
            },
        )
        check("set_intgrid on external level", status == "OK", msg)
        # Read the IntGrid back and confirm the fill round-trips.
        status, grid = s.call("get_intgrid", {"level": level, "layer": intgrid})
        check("get_intgrid runs", status == "OK", grid)
        if status == "OK":
            g = json.loads(grid)
            check(
                "get_intgrid round-trips fill",
                sum(1 for v in g["csv"] if v != 0) >= 9,
                sum(1 for v in g["csv"] if v != 0),
            )
            check("get_intgrid reports dimensions", g["cWid"] > 0 and g["cHei"] > 0, g)
        # Delete a separate external level; its body should be removed on save.
        check(
            "victim .ldtkl exists before delete",
            os.path.exists(os.path.join(wd, victim_rel)),
        )
        status, msg = s.call("delete_level", {"level": victim})
        check("delete external level", status == "OK", msg)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    main = json.load(open(f))
    main_lvl = next(l for l in main["levels"] if l["identifier"] == level)
    check("main file layerInstances nulled", main_lvl["layerInstances"] is None)
    check(
        "deleted level gone from main",
        all(l["identifier"] != victim for l in main["levels"]),
    )
    check(
        "deleted .ldtkl unlinked on save",
        not os.path.exists(os.path.join(wd, victim_rel)),
        victim_rel,
    )
    body = json.load(open(os.path.join(wd, ext_rel)))
    li = next(x for x in body["layerInstances"] if x["__identifier"] == intgrid)
    check(
        ".ldtkl intGrid updated",
        sum(1 for v in li["intGridCsv"] if v != 0) >= 9,
        sum(1 for v in li["intGridCsv"] if v != 0),
    )


def find_multi_world_sample():
    """Return (path, proj) for the first sample using the `worlds[]` array, else None."""
    for name in sorted(os.listdir(SAMPLES)):
        if not name.endswith(".ldtk"):
            continue
        try:
            proj = json.load(open(os.path.join(SAMPLES, name)))
        except (json.JSONDecodeError, IsADirectoryError, UnicodeDecodeError):
            continue
        if any(w.get("levels") for w in (proj.get("worlds") or [])):
            return os.path.join(SAMPLES, name), proj
    return None


def test_multi_world():
    print("Multi-world editing")
    found_sample = find_multi_world_sample()
    if not found_sample:
        print("  SKIP no multi-world (worlds[]) sample available")
        return
    src, proj = found_sample
    wd = workdir()
    f = os.path.join(wd, os.path.basename(src))
    shutil.copy(src, f)
    # Find a world level with an IntGrid layer.
    target = None
    for w in proj.get("worlds", []):
        for lvl in w.get("levels", []):
            for li in lvl.get("layerInstances") or []:
                if li["__type"] == "IntGrid":
                    target = (lvl["identifier"], li["__identifier"])
                    break
            if target:
                break
        if target:
            break
    if not target:
        check("world IntGrid available", False, "none found")
        return
    level, layer = target
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        status, listing = s.call("list_levels", {})
        check("list_levels sees world levels", status == "OK" and level in listing)
        status, msg = s.call(
            "set_intgrid",
            {
                "level": level,
                "layer": layer,
                "rects": [{"cx": 0, "cy": 0, "w": 2, "h": 2, "value": 1}],
            },
        )
        check("set_intgrid in world level", status == "OK", msg)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    saved = json.load(open(f))
    found = False
    for w in saved.get("worlds", []):
        for lvl in w.get("levels", []):
            if lvl["identifier"] == level:
                li = next(
                    x for x in lvl["layerInstances"] if x["__identifier"] == layer
                )
                found = sum(1 for v in li["intGridCsv"] if v != 0) >= 4
    check("world level intGrid updated", found)


def test_level_lifecycle():
    print("Level lifecycle (Typical_TopDown_example.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Typical_TopDown_example.ldtk")
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        st, msg = s.call(
            "create_level", {"identifier": "LC_Base", "px_wid": 256, "px_hei": 256}
        )
        check("create_level", st == "OK", msg)
        st, msg = s.call(
            "duplicate_level", {"level": "LC_Base", "identifier": "LC_Copy"}
        )
        check("duplicate_level", st == "OK", msg)
        st, msg = s.call(
            "move_level", {"level": "LC_Copy", "world_x": 2048, "world_y": 512}
        )
        check("move_level", st == "OK", msg)
        st, msg = s.call(
            "resize_level", {"level": "LC_Copy", "px_wid": 128, "px_hei": 128}
        )
        check("resize_level", st == "OK", msg)
        st, lvl = s.call("get_level", {"level": "LC_Copy"})
        check("get_level after resize", st == "OK", lvl)
        if st == "OK":
            g = json.loads(lvl)
            check(
                "resized dimensions",
                g["pxWid"] == 128 and g["pxHei"] == 128,
                (g.get("pxWid"), g.get("pxHei")),
            )
            ig = next((L for L in g["layers"] if L["type"] == "IntGrid"), None)
            if ig:
                check(
                    "intgrid reflowed to new width",
                    ig["cWid"] == 128 // ig["gridSize"],
                    ig,
                )
        st, msg = s.call("delete_level", {"level": "LC_Copy"})
        check("delete_level", st == "OK", msg)
        st, listing = s.call("list_levels", {})
        check(
            "deleted level gone", st == "OK" and "LC_Copy" not in listing, listing[:200]
        )
        check("base level remains", "LC_Base" in listing, listing[:200])
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()


def test_world_tools():
    print("World tools (Typical_TopDown_example.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Typical_TopDown_example.ldtk")
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        st, msg = s.call(
            "create_world",
            {
                "identifier": "NewWorld",
                "world_layout": "GridVania",
                "world_grid_width": 128,
            },
        )
        check("create_world", st == "OK", msg)
        st, msg = s.call(
            "set_world_layout", {"world": "NewWorld", "world_layout": "Free"}
        )
        check("set_world_layout", st == "OK", msg)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    saved = json.load(open(f))
    worlds = saved.get("worlds") or []
    nw = next((w for w in worlds if w["identifier"] == "NewWorld"), None)
    check("world persisted", nw is not None, [w.get("identifier") for w in worlds])
    if nw:
        check("world layout updated", nw["worldLayout"] == "Free", nw["worldLayout"])
        check("world grid width set", nw["worldGridWidth"] == 128, nw["worldGridWidth"])


def test_flood_fill():
    print("IntGrid flood fill (Typical_TopDown_example.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Typical_TopDown_example.ldtk")
    proj = json.load(open(f))
    intgrid = find_layer(proj, "IntGrid")
    if not intgrid:
        check("intgrid layer available", False, "none found")
        return
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        st, msg = s.call(
            "create_level", {"identifier": "FF_Test", "px_wid": 128, "px_hei": 128}
        )
        check("create_level", st == "OK", msg)
        # Seed a closed border so the interior fill is bounded.
        st, dims = s.call("get_intgrid", {"level": "FF_Test", "layer": intgrid})
        check("get_intgrid", st == "OK", dims)
        g = json.loads(dims)
        cw, ch = g["cWid"], g["cHei"]
        rects = [
            {"cx": 0, "cy": 0, "w": cw, "h": 1, "value": 1},
            {"cx": 0, "cy": ch - 1, "w": cw, "h": 1, "value": 1},
            {"cx": 0, "cy": 0, "w": 1, "h": ch, "value": 1},
            {"cx": cw - 1, "cy": 0, "w": 1, "h": ch, "value": 1},
        ]
        st, msg = s.call(
            "set_intgrid", {"level": "FF_Test", "layer": intgrid, "rects": rects}
        )
        check("seed border", st == "OK", msg)
        # Fill the interior from a center cell.
        st, msg = s.call(
            "flood_fill_intgrid",
            {
                "level": "FF_Test",
                "layer": intgrid,
                "cx": cw // 2,
                "cy": ch // 2,
                "value": 2,
            },
        )
        check("flood_fill_intgrid", st == "OK", msg)
        st, after = s.call("get_intgrid", {"level": "FF_Test", "layer": intgrid})
        if st == "OK":
            csv = json.loads(after)["csv"]
            interior = (cw - 2) * (ch - 2)
            check(
                "interior filled",
                sum(1 for v in csv if v == 2) == interior,
                (sum(1 for v in csv if v == 2), interior),
            )
            check(
                "border preserved",
                sum(1 for v in csv if v == 1) == cw * ch - interior,
                sum(1 for v in csv if v == 1),
            )
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()


def test_define_from_scratch():
    print("Definition authoring (Entities.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Entities.ldtk")
    proj = json.load(open(f))
    # An existing Entities layer to place our new entity on.
    ent_layer = find_layer(proj, "Entities")
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"

        st, msg = s.call(
            "create_tileset_def",
            {
                "identifier": "T3_Atlas",
                "rel_path": "atlas3.png",
                "px_wid": 64,
                "px_hei": 64,
                "tile_grid_size": 16,
            },
        )
        check("create_tileset_def", st == "OK", msg)

        st, msg = s.call(
            "create_enum", {"identifier": "T3_Loot", "values": ["Gold", "Gems"]}
        )
        check("create_enum", st == "OK", msg)

        # Look up the new tileset uid for the entity tile binding.
        st, defs = s.call("describe_defs", {})
        ts_uid = None
        if st == "OK":
            ts = next(
                (
                    t
                    for t in json.loads(defs)["tilesets"]
                    if t["identifier"] == "T3_Atlas"
                ),
                None,
            )
            ts_uid = ts["uid"] if ts else None
        check("new tileset visible in describe_defs", ts_uid is not None, defs[:200])

        st, msg = s.call(
            "create_entity_def",
            {
                "identifier": "T3_Pickup",
                "width": 16,
                "height": 16,
                "tileset_uid": ts_uid,
                "tile_id": 0,
            },
        )
        check("create_entity_def (tile)", st == "OK", msg)

        st, msg = s.call(
            "create_layer_def",
            {
                "identifier": "T3_Walls",
                "type": "IntGrid",
                "int_grid_values": [
                    {"value": 1, "identifier": "wall", "color": "#FF0000"}
                ],
            },
        )
        check("create_layer_def (IntGrid)", st == "OK", msg)

        # Extend the IntGrid palette on the existing layer def (append + upsert).
        st, msg = s.call(
            "add_intgrid_values",
            {
                "layer": "T3_Walls",
                "values": [
                    {"value": 2, "identifier": "Tree", "color": "#2E7D32"},
                    {"value": 3, "identifier": "Fence", "color": "#8D6E63"},
                    {"value": 1, "identifier": "wall", "color": "#FF3333"},
                ],
            },
        )
        check("add_intgrid_values", st == "OK", msg)
        st, defs = s.call("describe_defs", {})
        if st == "OK":
            walls_def = next((L for L in json.loads(defs)["layers"] if L["identifier"] == "T3_Walls"), None)
            vals = walls_def.get("intGridValues") if walls_def else None
            ids = {v["value"]: v.get("identifier") for v in (vals or [])}
            check("intgrid palette extended", ids.get(2) == "Tree" and ids.get(3) == "Fence", ids)
            colors = {v["value"]: v.get("color") for v in (vals or [])}
            check("intgrid value upserted in place", len(vals or []) == 3 and colors.get(1) == "#FF3333", vals)

        # New levels must include the backfilled layer; so must existing ones.
        st, msg = s.call(
            "create_level", {"identifier": "T3_Level", "px_wid": 64, "px_hei": 64}
        )
        check("create_level", st == "OK", msg)
        st, lvl = s.call("get_level", {"level": "T3_Level"})
        if st == "OK":
            layers = json.loads(lvl)["layers"]
            walls = next((L for L in layers if L["identifier"] == "T3_Walls"), None)
            check(
                "new layer backfilled into new level",
                walls is not None and walls["type"] == "IntGrid",
                layers,
            )

        # Add a field to the new entity and prove the encode path works end to end.
        st, msg = s.call(
            "add_entity_field",
            {
                "entity": "T3_Pickup",
                "identifier": "loot",
                "field_type": "Enum",
                "enum_id": "T3_Loot",
            },
        )
        check("add_entity_field (enum)", st == "OK", msg)

        st, msg = s.call(
            "place_entities",
            {
                "level": "T3_Level",
                "layer": ent_layer,
                "entities": [
                    {
                        "identifier": "T3_Pickup",
                        "cx": 1,
                        "cy": 1,
                        "fields": {"loot": "Gold"},
                    }
                ],
            },
        )
        check("place_entities w/ new entity+field", st == "OK", msg)
        st, listing = s.call("get_entities", {"level": "T3_Level", "layer": ent_layer})
        if st == "OK":
            ents = [e for grp in json.loads(listing) for e in grp["entities"]]
            pickup = next((e for e in ents if e["identifier"] == "T3_Pickup"), None)
            check(
                "placed entity decodes enum field",
                pickup is not None and pickup["fields"].get("loot") == "Gold",
                listing[:300],
            )

        # Invalid enum value should be rejected by the encode path.
        st, msg = s.call(
            "place_entities",
            {
                "level": "T3_Level",
                "layer": ent_layer,
                "entities": [
                    {
                        "identifier": "T3_Pickup",
                        "cx": 2,
                        "cy": 2,
                        "fields": {"loot": "Diamond"},
                    }
                ],
            },
        )
        check("invalid enum value rejected", st == "ERROR", msg)

        # Everything we authored should still validate cleanly against the schema.
        st, text = s.call("validate_project", {})
        check("validate runs", st == "OK", text)
        check("no structural issues", "no structural issues" in text, text[:300])
        check("no schema warnings", "no warnings" in text, text[:400])
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()


def test_safety():
    print(
        "Safety: preview_changes / undo / redo / revert_unsaved (Typical_TopDown_example.ldtk)"
    )
    wd = workdir()
    f = copy_into(wd, "Typical_TopDown_example.ldtk")
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        st, text = s.call("preview_changes", {})
        check(
            "preview clean before edits",
            st == "OK" and "matches the file on disk" in text,
            text[:200],
        )

        st, msg = s.call(
            "create_level", {"identifier": "Safety_Test", "px_wid": 128, "px_hei": 128}
        )
        check("create_level", st == "OK", msg)
        st, text = s.call("preview_changes", {})
        check(
            "preview lists added level",
            st == "OK" and "Safety_Test" in text and "added" in text,
            text[:300],
        )

        st, msg = s.call("undo", {})
        check("undo runs", st == "OK", msg)
        st, text = s.call("preview_changes", {})
        check(
            "preview clean after undo",
            st == "OK" and "matches the file on disk" in text,
            text[:200],
        )

        st, msg = s.call("redo", {})
        check("redo runs", st == "OK", msg)
        st, text = s.call("preview_changes", {})
        check(
            "preview lists level again after redo",
            st == "OK" and "Safety_Test" in text,
            text[:300],
        )

        st, msg = s.call("revert_unsaved", {})
        check("revert_unsaved runs", st == "OK", msg)
        st, listing = s.call("list_levels", {})
        check(
            "reverted level gone",
            st == "OK" and "Safety_Test" not in listing,
            listing[:200],
        )
        # Revert clears history, so there is nothing left to undo.
        st, msg = s.call("undo", {})
        check("undo empty after revert", st == "ERROR", msg)

        # Nothing was saved, so the file on disk must be untouched.
        check("save (no-op edits persisted)", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    saved = json.load(open(f))
    check(
        "file has no Safety_Test level",
        all(l["identifier"] != "Safety_Test" for l in saved["levels"]),
    )


PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def test_render_and_resources():
    print("Visual feedback: render_level + tileset resources (Typical_TopDown_example.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Typical_TopDown_example.ldtk")
    # The atlas images live in the nested support dir; place them beside the .ldtk so the
    # relative tileset relPaths (atlas/...) resolve, mirroring the external-levels test.
    shutil.copytree(os.path.join(SUPPORT, "atlas"), os.path.join(wd, "atlas"))
    proj = json.load(open(f))
    level = proj["levels"][0]["identifier"]
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"

        resp = s.call_full("render_level", {"level": level, "max_px": 512})
        ok = "result" in resp
        check("render_level runs", ok, resp.get("error"))
        if ok:
            content = resp["result"]["content"]
            note = next((c for c in content if c.get("type") == "text"), None)
            image = next((c for c in content if c.get("type") == "image"), None)
            check("render note present", note is not None and "Rendered" in note.get("text", ""), note)
            check("image content returned", image is not None and image.get("mimeType") == "image/png", image)
            if image:
                raw = base64.b64decode(image["data"])
                check("image is a valid PNG", raw[:8] == PNG_MAGIC, raw[:8])
                check("image is non-trivial", len(raw) > 100, len(raw))

        # Layer filter: rendering a single layer still produces a valid PNG.
        ig = find_layer(proj, "IntGrid")
        if ig:
            resp = s.call_full("render_level", {"level": level, "layers": [ig], "scale": 1})
            img = next((c for c in resp.get("result", {}).get("content", []) if c.get("type") == "image"), None)
            check("layer-filtered render returns PNG",
                  img is not None and base64.b64decode(img["data"])[:8] == PNG_MAGIC, img is not None)

        # Tileset images are exposed as resources.
        listing = s.rpc("resources/list", {})
        uris = [r["uri"] for r in listing.get("result", {}).get("resources", [])]
        tileset_uri = next((u for u in uris if u.startswith("ldtk://tileset/")), None)
        check("resources/list exposes a tileset", tileset_uri is not None, uris)
        if tileset_uri:
            read = s.rpc("resources/read", {"uri": tileset_uri})
            contents = read.get("result", {}).get("contents", [])
            blob = contents[0] if contents else {}
            check("resources/read returns image blob",
                  blob.get("mimeType") == "image/png" and "blob" in blob, blob.get("mimeType"))
            if "blob" in blob:
                check("tileset blob is a valid PNG", base64.b64decode(blob["blob"])[:8] == PNG_MAGIC)

        # A level preview is also readable as a templated resource.
        read = s.rpc("resources/read", {"uri": f"ldtk://level/{level}/preview.png"})
        contents = read.get("result", {}).get("contents", [])
        check("level preview resource renders a PNG",
              bool(contents) and base64.b64decode(contents[0]["blob"])[:8] == PNG_MAGIC, read.get("error"))
    finally:
        s.close()


def test_validate():
    print("Validation (Entities.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Entities.ldtk")
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        status, text = s.call("validate_project", {})
        check("validate runs", status == "OK", text)
        check("structural OK", "no structural issues" in text, text[:200])
        check("schema section present", "Schema" in text, text[:200])
        print("   " + text.replace("\n", "\n   ")[:600])
    finally:
        s.close()


if __name__ == "__main__":
    subprocess.run(["cargo", "build", "--quiet"], cwd=ROOT, check=True)
    test_typed_entity_fields()
    test_paint_tiles()
    test_external_levels()
    test_multi_world()
    test_level_lifecycle()
    test_world_tools()
    test_flood_fill()
    test_define_from_scratch()
    test_safety()
    test_render_and_resources()
    test_validate()
    print(f"\n{PASS} passed, {FAIL} failed")
    sys.exit(1 if FAIL else 0)
