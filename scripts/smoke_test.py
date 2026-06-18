#!/usr/bin/env python3
"""End-to-end stdio tests for ldtk-mcp against the bundled LDtk samples.

Covers: level creation + IntGrid, typed entity fields, level fields, tile painting,
separate level files (.ldtkl) round-trip, and multi-world editing.
"""
import json, os, shutil, subprocess, sys, tempfile

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
            [BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, text=True,
        )
        self._send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                    "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                               "clientInfo": {"name": "smoke", "version": "0"}}}, True)
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"}, False)
        self._id = 1

    def _send(self, obj, expect):
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()
        if expect:
            return json.loads(self.proc.stdout.readline())

    def call(self, name, args):
        self._id += 1
        resp = self._send({"jsonrpc": "2.0", "id": self._id, "method": "tools/call",
                           "params": {"name": name, "arguments": args}}, True)
        if "error" in resp:
            return ("ERROR", resp["error"]["message"])
        return ("OK", resp["result"]["content"][0]["text"])

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
        status, msg = s.call("place_entities", {
            "level": level, "layer": "GameEntities",
            "entities": [{"identifier": "Chest", "cx": 5, "cy": 5,
                          "fields": {"content": ["Gold", "Trout"], "requireKey": True}}],
        })
        check("place_entities w/ fields", status == "OK", msg)
        # Read the entities back through the new query-depth tool before saving.
        status, listing = s.call("get_entities", {"level": level, "layer": "GameEntities"})
        check("get_entities runs", status == "OK", listing)
        if status == "OK":
            data = json.loads(listing)
            ents = [e for grp in data for e in grp["entities"]]
            chest = next((e for e in ents if e["identifier"] == "Chest" and e["cx"] == 5 and e["cy"] == 5), None)
            check("get_entities returns placed chest", chest is not None, listing[:200])
            if chest:
                check("get_entities decodes content field",
                      chest["fields"].get("content") == ["Gold", "Trout"], chest["fields"])
                check("get_entities exposes iid", bool(chest.get("iid")), chest)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    saved = json.load(open(f))
    lvl = next(l for l in saved["levels"] if l["identifier"] == level)
    ents = next(li for li in lvl["layerInstances"] if li["__identifier"] == "GameEntities")["entityInstances"]
    chest = next(e for e in ents if e["__grid"] == [5, 5] and e["__identifier"] == "Chest")
    fields = {fi["__identifier"]: fi for fi in chest["fieldInstances"]}
    check("content __value", fields["content"]["__value"] == ["Gold", "Trout"], fields["content"]["__value"])
    check("content realEditorValues",
          fields["content"]["realEditorValues"] == [
              {"id": "V_String", "params": ["Gold"]}, {"id": "V_String", "params": ["Trout"]}],
          fields["content"]["realEditorValues"])
    check("requireKey __value", fields["requireKey"]["__value"] is True)
    check("requireKey realEditorValues",
          fields["requireKey"]["realEditorValues"] == [{"id": "V_Bool", "params": [True]}],
          fields["requireKey"]["realEditorValues"])


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
        status, msg = s.call("paint_tiles", {
            "level": level, "layer": layer, "replace": True,
            "tiles": [{"cx": 0, "cy": 0, "t": 0}, {"cx": 1, "cy": 0, "t": 1}],
        })
        check("paint_tiles", status == "OK", msg)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    saved = json.load(open(f))
    lvl = next(l for l in saved["levels"] if l["identifier"] == level)
    tiles = next(li for li in lvl["layerInstances"] if li["__identifier"] == layer)["gridTiles"]
    check("two tiles painted", len(tiles) == 2, len(tiles))
    check("tile ids", sorted(t["t"] for t in tiles) == [0, 1])
    check("tile has src/px/d", all(len(t["src"]) == 2 and len(t["px"]) == 2 and "d" in t for t in tiles))


def test_external_levels():
    print("Separate level files round-trip (SeparateLevelFiles.ldtk)")
    wd = workdir()
    f = copy_into(wd, "SeparateLevelFiles.ldtk")
    # The .ldtkl bodies live in the nested support dir; place them beside the .ldtk
    # so the relative externalRelPath resolves.
    shutil.copytree(os.path.join(SUPPORT, "SeparateLevelFiles"), os.path.join(wd, "SeparateLevelFiles"))
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
        status, msg = s.call("set_intgrid", {
            "level": level, "layer": intgrid,
            "rects": [{"cx": 0, "cy": 0, "w": 3, "h": 3, "value": 1}],
        })
        check("set_intgrid on external level", status == "OK", msg)
        # Read the IntGrid back and confirm the fill round-trips.
        status, grid = s.call("get_intgrid", {"level": level, "layer": intgrid})
        check("get_intgrid runs", status == "OK", grid)
        if status == "OK":
            g = json.loads(grid)
            check("get_intgrid round-trips fill", sum(1 for v in g["csv"] if v != 0) >= 9, sum(1 for v in g["csv"] if v != 0))
            check("get_intgrid reports dimensions", g["cWid"] > 0 and g["cHei"] > 0, g)
        # Delete a separate external level; its body should be removed on save.
        check("victim .ldtkl exists before delete", os.path.exists(os.path.join(wd, victim_rel)))
        status, msg = s.call("delete_level", {"level": victim})
        check("delete external level", status == "OK", msg)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    main = json.load(open(f))
    main_lvl = next(l for l in main["levels"] if l["identifier"] == level)
    check("main file layerInstances nulled", main_lvl["layerInstances"] is None)
    check("deleted level gone from main", all(l["identifier"] != victim for l in main["levels"]))
    check("deleted .ldtkl unlinked on save", not os.path.exists(os.path.join(wd, victim_rel)), victim_rel)
    body = json.load(open(os.path.join(wd, ext_rel)))
    li = next(x for x in body["layerInstances"] if x["__identifier"] == intgrid)
    check(".ldtkl intGrid updated", sum(1 for v in li["intGridCsv"] if v != 0) >= 9,
          sum(1 for v in li["intGridCsv"] if v != 0))


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
        status, msg = s.call("set_intgrid", {
            "level": level, "layer": layer,
            "rects": [{"cx": 0, "cy": 0, "w": 2, "h": 2, "value": 1}],
        })
        check("set_intgrid in world level", status == "OK", msg)
        check("save", s.call("save_project", {})[0] == "OK")
    finally:
        s.close()

    saved = json.load(open(f))
    found = False
    for w in saved.get("worlds", []):
        for lvl in w.get("levels", []):
            if lvl["identifier"] == level:
                li = next(x for x in lvl["layerInstances"] if x["__identifier"] == layer)
                found = sum(1 for v in li["intGridCsv"] if v != 0) >= 4
    check("world level intGrid updated", found)


def test_level_lifecycle():
    print("Level lifecycle (Typical_TopDown_example.ldtk)")
    wd = workdir()
    f = copy_into(wd, "Typical_TopDown_example.ldtk")
    s = Session()
    try:
        assert s.call("open_project", {"path": f})[0] == "OK"
        st, msg = s.call("create_level", {"identifier": "LC_Base", "px_wid": 256, "px_hei": 256})
        check("create_level", st == "OK", msg)
        st, msg = s.call("duplicate_level", {"level": "LC_Base", "identifier": "LC_Copy"})
        check("duplicate_level", st == "OK", msg)
        st, msg = s.call("move_level", {"level": "LC_Copy", "world_x": 2048, "world_y": 512})
        check("move_level", st == "OK", msg)
        st, msg = s.call("resize_level", {"level": "LC_Copy", "px_wid": 128, "px_hei": 128})
        check("resize_level", st == "OK", msg)
        st, lvl = s.call("get_level", {"level": "LC_Copy"})
        check("get_level after resize", st == "OK", lvl)
        if st == "OK":
            g = json.loads(lvl)
            check("resized dimensions", g["pxWid"] == 128 and g["pxHei"] == 128, (g.get("pxWid"), g.get("pxHei")))
            ig = next((L for L in g["layers"] if L["type"] == "IntGrid"), None)
            if ig:
                check("intgrid reflowed to new width", ig["cWid"] == 128 // ig["gridSize"], ig)
        st, msg = s.call("delete_level", {"level": "LC_Copy"})
        check("delete_level", st == "OK", msg)
        st, listing = s.call("list_levels", {})
        check("deleted level gone", st == "OK" and "LC_Copy" not in listing, listing[:200])
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
        st, msg = s.call("create_world", {
            "identifier": "NewWorld", "world_layout": "GridVania", "world_grid_width": 128,
        })
        check("create_world", st == "OK", msg)
        st, msg = s.call("set_world_layout", {"world": "NewWorld", "world_layout": "Free"})
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
    test_validate()
    print(f"\n{PASS} passed, {FAIL} failed")
    sys.exit(1 if FAIL else 0)
