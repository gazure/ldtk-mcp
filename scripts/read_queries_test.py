#!/usr/bin/env python3
"""Test bounded read tools over MCP stdio. Run cargo build first."""

import json
from pathlib import Path
import tempfile
import unittest

from smoke_test import Session


class ReadQueriesTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="ldtk-read-tests-")
        self.addCleanup(self.directory.cleanup)
        entities = [
            {
                "iid": str(i),
                "__identifier": "Chest",
                "__grid": [i % 20, i // 20],
                "__tags": ["loot"] if i % 2 == 0 else [],
                "fieldInstances": [
                    {"__identifier": "gold", "__value": i},
                    {"__identifier": "description", "__value": "treasure"},
                ],
            }
            for i in range(205)
        ]
        grid = [0] * 10_000
        grid[101:103] = [2, 2]
        project = {
            "defs": {
                "layers": [{"identifier": "Terrain", "intGridValues": [{"value": 2, "identifier": "Wall"}]}]
            },
            "levels": [{
                "identifier": "Test",
                "layerInstances": [
                    {"__identifier": "A", "iid": "layer-a", "__type": "Entities", "entityInstances": entities[:103]},
                    {"__identifier": "B", "iid": "layer-b", "__type": "Entities", "entityInstances": entities[103:]},
                    {"__identifier": "Terrain", "iid": "terrain", "__type": "IntGrid", "__cWid": 100,
                     "__cHei": 100, "__gridSize": 16, "intGridCsv": grid},
                    {"__identifier": "Generated", "__type": "AutoLayer", "autoLayerTiles": [{"t": 1}] * 5000},
                ],
            }],
        }
        path = Path(self.directory.name) / "test.ldtk"
        path.write_text(json.dumps(project))
        self.session = Session()
        self.addCleanup(self.session.close)
        status, message = self.session.call("open_project", {"path": str(path)})
        self.assertEqual(status, "OK", message)

    def read(self, name, **args):
        status, message = self.session.call(name, {"level": "Test", **args})
        self.assertEqual(status, "OK", message)
        self.assertLessEqual(len(message.encode()), 65_536)
        return json.loads(message)

    def test_pages_cover_all_entities_once_across_layers(self):
        offset = 0
        ids = []
        while True:
            page = self.read("get_entities", offset=offset)
            self.assertEqual(page["total"], 205)
            self.assertLessEqual(page["returned"], 100)
            ids.extend(e["iid"] for layer in page["layers"] for e in layer["entities"])
            if page["next_offset"] is None:
                break
            self.assertGreater(page["next_offset"], offset)
            offset = page["next_offset"]
        self.assertEqual(ids, [str(i) for i in range(205)])

    def test_filters_projection_and_layer_iid(self):
        page = self.read("get_entities", layer="layer-a", identifier="Chest", tags=["loot"],
                         rect={"cx": 0, "cy": 0, "w": 4, "h": 2}, fields=["gold"], limit=2, offset=1)
        self.assertEqual(page["total"], 4)
        entities = page["layers"][0]["entities"]
        self.assertEqual([e["iid"] for e in entities], ["2", "20"])
        self.assertEqual(entities[0]["fields"], {"gold": 2})
        self.assertEqual(page["next_offset"], 3)
        empty = self.read("get_entities", identifier="Missing")
        self.assertEqual(empty["layers"], [])
        self.assertIsNone(empty["next_offset"])
        page = self.read("get_entities", fields=[], limit=1)
        self.assertEqual(page["layers"][0]["entities"][0]["fields"], {})

    def test_large_grid_requires_region_or_compact_encoding(self):
        for name in ["get_intgrid", "get_layer"]:
            status, message = self.session.call(name, {"level": "Test", "layer": "Terrain"})
            self.assertEqual(status, "ERROR")
            self.assertIn("4096", message)
        args = {"layer": "terrain", "rect": {"cx": 1, "cy": 1, "w": 2, "h": 2}}
        csv = self.read("get_intgrid", **args)
        self.assertEqual(csv["csv"], [2, 2, 0, 0])
        self.assertEqual(csv["values"][0]["identifier"], "Wall")
        rle = self.read("get_intgrid", **args, encoding="rle")
        self.assertEqual(rle["runs"], [[2, 2], [0, 2]])
        sparse = self.read("get_intgrid", layer="Terrain", encoding="sparse")
        self.assertEqual(sparse["cells"], [[1, 1, 2], [2, 1, 2]])
        full_rle = self.read("get_intgrid", layer="Terrain", encoding="rle")
        self.assertEqual(full_rle["runs"], [[0, 101], [2, 2], [0, 9897]])

    def test_invalid_queries_return_errors(self):
        queries = [
            ("get_entities", {"limit": 501}),
            ("get_entities", {"offset": -1}),
            ("get_entities", {"layer": "Terrain"}),
            ("get_intgrid", {"layer": "A"}),
            ("get_intgrid", {"layer": "Terrain", "encoding": "unknown"}),
            ("get_intgrid", {"layer": "Terrain", "rect": {"cx": 99, "cy": 0, "w": 2, "h": 1}}),
        ]
        for name, args in queries:
            with self.subTest(name=name, args=args):
                status, _ = self.session.call(name, {"level": "Test", **args})
                self.assertEqual(status, "ERROR")

    def test_large_fields_can_be_excluded_to_fit_the_byte_budget(self):
        path = Path(self.directory.name) / "test.ldtk"
        project = json.loads(path.read_text())
        entity = project["levels"][0]["layerInstances"][0]["entityInstances"][0]
        entity["fieldInstances"][1]["__value"] = "x" * 65_536
        path.write_text(json.dumps(project))
        self.assertEqual(self.session.call("open_project", {"path": str(path)})[0], "OK")
        status, message = self.session.call("get_entities", {"level": "Test", "limit": 1})
        self.assertEqual(status, "ERROR")
        self.assertIn("select fewer fields", message)
        page = self.read("get_entities", limit=1, fields=["gold"])
        self.assertEqual(page["layers"][0]["entities"][0]["fields"], {"gold": 0})

    def test_unrequested_auto_tiles_do_not_consume_the_content_budget(self):
        layer = self.read("get_layer", layer="Generated")
        self.assertEqual(layer["autoLayerTileCount"], 5000)
        self.assertNotIn("autoLayerTiles", layer)
        status, _ = self.session.call("get_layer", {
            "level": "Test", "layer": "Generated", "include_auto_tiles": True,
        })
        self.assertEqual(status, "ERROR")


if __name__ == "__main__":
    unittest.main()
