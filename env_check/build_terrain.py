"""A cave or outdoor ground check through the Clide3D MCP tools: a cracked,
ridged terrain with a sculpted mound and painted moss, a boulder, stones
scattered onto it and a brazier. Run the editor with CLIDE3D_MCP_PORT=7891,
then `python3 env_check/build_terrain.py` (it clears the scene)."""
import os
OUT = os.path.dirname(os.path.abspath(__file__))
import json, sys, urllib.request, base64, time
def call(tool, args={}, image=None, quiet=True):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).encode()
    req = urllib.request.Request("http://127.0.0.1:7891/mcp", data=body, headers={"Content-Type": "application/json"})
    t = time.time()
    r = json.loads(urllib.request.urlopen(req, timeout=700).read())["result"]
    texts = [c["text"] for c in r["content"] if c["type"] == "text"]
    if r.get("isError"):
        print("ERROR", tool, args.get("name", ""), texts); sys.exit(1)
    for c in r["content"]:
        if c["type"] == "image" and image:
            open(image, "wb").write(base64.b64decode(c["data"]))
    out = json.loads(texts[0]) if texts and texts[0].startswith("{") else texts
    if not quiet: print(tool, json.dumps(out)[:1500])
    else: print(f"{tool} {args.get('name', args.get('piece',''))}: ok {time.time()-t:.1f}s", (out.get('triangles') if isinstance(out, dict) else ''))
    return out

call("new_scene")
call("create_object", {"shape": "Terrain", "name": "cave_floor", "terrain": {"size": [10, 10], "resolution": 256, "height": 0.35, "frequency": 0.4, "ridged": 0.4, "cracks": 0.06, "crack_scale": 0.7, "seed": 3}}, quiet=False)
call("edit_terrain", {"target": "cave_floor", "strokes": [
  {"brush": "raise", "points": [[-2.5, -1.5]], "radius": 1.8, "strength": 0.9},
  {"brush": "noise", "points": [[-2.5, -1.5]], "radius": 1.8, "strength": 0.15, "scale": 0.25},
  {"brush": "flatten", "points": [[0.5, 0.5], [3, 2]], "radius": 0.9, "strength": 0.8},
  {"brush": "paint", "points": [[2.5, -2.5], [3.5, -1]], "radius": 1.2, "strength": 0.7, "color": [0.2, 0.26, 0.14]},
]}, quiet=False)
call("kit", {"piece": "rock", "name": "boulder", "size": [1.4, 0.9, 1.1], "strata": 0.6, "at": [1.5, 0.0, -2.2], "seed": 4})
call("kit", {"piece": "rock", "name": "src_rock", "size": [0.3, 0.2, 0.25], "at": [0, -10, 0], "seed": 5})
call("kit", {"piece": "rock", "name": "src_pebble", "size": [0.12, 0.07, 0.1], "facets": 9, "at": [0, -10, 0], "seed": 6})
call("scatter", {"name": "stones", "sources": ["src_rock", "src_pebble"], "on": ["cave_floor"], "size": [9.5, 9.5], "density": 3, "min_distance": 0.18, "scale": [0.6, 1.5], "align": 1, "sink": 0.03, "weights": [1, 3], "avoid": [[0.5, 0.5, 0.8]], "seed": 7}, quiet=False)
call("kit", {"piece": "brazier", "name": "camp", "at": [0.5, 0, 0.6], "snap": 0})
call("render_film", {"views": [{"view": "iso", "center": [0, 0, 0], "radius": 4}], "size": 1280, "aspect": 1.5, "samples": 128, "floor": False, "environment": "golden_hour", "lights": "none", "background": "environment", "tonemap": "agx", "save_path": OUT + "/terrain.png"}, quiet=False)
