"""Builds the env_check crypt room through the Clide3D MCP tools, as an agent
would: kit pieces, scatters, a hero and a monster. Run the editor with
CLIDE3D_MCP_PORT=7891, then `python3 env_check/build_crypt.py` and render with
`python3 env_check/build_crypt.py render` (see README "Build a crypt room")."""
import math, os
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


import sys
LOOK = {"views": [{"view": "iso", "center": [0, 0.3, -0.5], "radius": 4.3}], "frame": 1, "size": 1600, "aspect": 1.7778, "samples": 256, "time_limit": 300,
        "floor": False, "environment": "night", "lights": [{"type": "sun", "direction": [-0.4, 1, 0.6], "color": [0.55, 0.65, 0.9], "intensity": 0.08, "size": 25}],
        "background": "environment", "tonemap": "aces", "bloom": {"strength": 0.15, "threshold": 1.2}, "vignette": 0.4, "exposure": 1.5, "white_balance": 4200,
        "fog": {"density": 0.04, "height": 0.3, "falloff": 1.0, "anisotropy": 0.5}, "device": "auto"}

if len(sys.argv) > 1 and sys.argv[1] == "render":
    out = call("render_film", dict(LOOK, save_path=OUT + "/crypt_iso.png"))
    # Long renders answer with a job id; wait for it.
    job = out.get("job") if isinstance(out, dict) else None
    while isinstance(out, dict) and out.get("status") == "running":
        out = call("job_status", {"id": job, "wait": 45})
    print(out)
    sys.exit(0)
call("new_scene")
call("kit", {"piece": "floor", "name": "floor", "length": 8, "depth": 8, "at": [0, 0, 0], "damage": 0.35, "seed": 1})
call("kit", {"piece": "wall", "name": "wall_n1", "at": [-3, 0, -4], "seed": 11})
call("kit", {"piece": "doorway", "name": "doorway", "length": 4, "opening": 1.6, "opening_height": 1.7, "at": [0, 0, -4], "seed": 12})
call("kit", {"piece": "wall", "name": "wall_n2", "at": [3, 0, -4], "seed": 13})
for i, z in enumerate([-3, -1, 1, 3]):
    call("kit", {"piece": "wall", "name": f"wall_w{i}", "at": [-4, 0, z], "turn": 90, "seed": 20 + i, "damage": 0.2 + 0.1 * i})
for i, x in enumerate([-3, -1, 1, 3]):
    call("kit", {"piece": "wall", "name": f"wall_s{i}", "at": [x, 0, 4], "height": 0.5 + 0.35 * ((i * 7) % 3), "damage": 0.8, "seed": 30 + i})
    call("kit", {"piece": "wall", "name": f"wall_e{i}", "at": [4, 0, -x], "turn": 90, "height": 0.4 + 0.3 * ((i * 5) % 3), "damage": 0.8, "seed": 40 + i})
call("kit", {"piece": "corner", "name": "corner_nw", "at": [-4, 0, -4], "seed": 50})
call("kit", {"piece": "corner", "name": "corner_ne", "at": [4, 0, -4], "turn": -90, "seed": 51})
call("kit", {"piece": "corner", "name": "corner_sw", "at": [-4, 0, 4], "turn": 90, "height": 1.6, "damage": 0.6, "seed": 52})
call("kit", {"piece": "corner", "name": "corner_se", "at": [4, 0, 4], "turn": 180, "height": 0.9, "damage": 0.7, "seed": 53})
# Beyond the doorway: a corridor lit from far down.
call("kit", {"piece": "floor", "name": "corridor_floor", "length": 2, "depth": 3, "at": [0, 0, -5.5], "seed": 14, "snap": 0})
call("kit", {"piece": "wall", "name": "corridor_l", "length": 3, "at": [-1.25, 0, -5.75], "turn": 90, "seed": 15, "snap": 0})
call("kit", {"piece": "wall", "name": "corridor_r", "length": 3, "at": [1.25, 0, -5.75], "turn": 90, "seed": 16, "snap": 0})
call("kit", {"piece": "torch", "name": "corridor_torch", "at": [-1.0, 1.8, -6.3], "turn": 90, "seed": 17})
call("kit", {"piece": "pillar", "name": "pillar_l", "at": [-2, 0, -1], "seed": 60})
call("kit", {"piece": "pillar", "name": "pillar_r", "at": [2, 0, -1], "seed": 61, "damage": 0.45})
call("kit", {"piece": "rubble", "name": "rubble", "width": 1.8, "count": 45, "at": [-2.9, 0, -2.7], "seed": 70})
call("kit", {"piece": "rubble", "name": "rubble_small", "width": 1.1, "height": 0.3, "count": 22, "at": [2.7, 0, 2.6], "seed": 71})
call("kit", {"piece": "torch", "name": "torch_l", "at": [-1.9, 1.9, -3.75], "seed": 80})
call("kit", {"piece": "torch", "name": "torch_r", "at": [1.9, 1.9, -3.75], "seed": 81})
call("kit", {"piece": "torch", "name": "torch_w", "at": [-3.75, 1.9, 0], "turn": 90, "seed": 82})
call("kit", {"piece": "brazier", "name": "brazier", "at": [0.3, 0, 0.9], "seed": 90})
call("kit", {"piece": "chain", "name": "chain_1", "at": [-3.72, 2.6, -1.6], "length": 1.1, "turn": 90, "seed": 91})
call("kit", {"piece": "chain", "name": "chain_2", "at": [-3.72, 2.5, -1.3], "length": 0.8, "turn": 90, "seed": 92})
# Sources to scatter, set aside under the floor.
src = dict(at=[0, -20, 0])
call("kit", {"piece": "rock", "name": "src_rock_a", "size": [0.32, 0.2, 0.26], "seed": 100, **src})
call("kit", {"piece": "rock", "name": "src_rock_b", "size": [0.2, 0.14, 0.18], "facets": 9, "seed": 101, **src})
call("kit", {"piece": "bones", "name": "src_femur", "seed": 102, **src})
call("kit", {"piece": "bones", "name": "src_skull", "kind": "skull", "seed": 103, **src})
call("kit", {"piece": "bones", "name": "src_ribs", "kind": "ribs", "seed": 104, **src})
call("kit", {"piece": "candle", "name": "src_candle", "seed": 105, **src})
call("kit", {"piece": "candle", "name": "src_candles", "count": 3, "seed": 106, **src})
clear = [[0.3, 0.9, 1.2], [0, -3, 1.0], [-2, -1, 0.5], [2, -1, 0.5]]
on = ["floor", "rubble", "rubble_small"]
call("scatter", {"name": "rocks", "sources": ["src_rock_a", "src_rock_b"], "on": on, "size": [7.4, 7.4], "density": 1.6, "min_distance": 0.28, "scale": [0.5, 1.4], "align": 0.6, "sink": 0.02, "avoid": clear, "seed": 1}, quiet=False)
call("scatter", {"name": "bones", "sources": ["src_femur", "src_skull", "src_ribs"], "on": on, "center": [-0.5, 0.8], "size": [6.5, 5.5], "density": 0.9, "min_distance": 0.32, "tilt": 8, "align": 0.8, "weights": [3, 1, 1], "avoid": clear, "avoid_objects": ["pillar_l", "pillar_r"], "seed": 2}, quiet=False)
call("kit", {"piece": "sarcophagus", "name": "sarcophagus", "at": [-2.4, 0, 1.4], "turn": 90, "seed": 95})
call("scatter", {"name": "candles", "sources": ["src_candle", "src_candles"], "on": on + ["sarcophagus"], "center": [-2.4, 1.4], "radius": 1.5, "count": 9, "min_distance": 0.45, "scale": [0.8, 1.3], "avoid_objects": ["pillar_l"], "seed": 3}, quiet=False)
call("scatter", {"target": "candles", "mode": "add", "sources": ["src_candle", "src_candles"], "on": on, "center": [0.2, -3.4], "size": [6.5, 0.8], "count": 6, "min_distance": 0.5, "scale": [0.8, 1.3], "avoid": [[0, -3.4, 0.9]], "seed": 4}, quiet=False)



# ---- the hero: a human base, dressed with cover blobs, a sword on the right hand
call("create_human", {"name": "Hero", "human": {"sex": 1, "muscle": 0.75, "weight": 0.55, "height": 0.6}, "skin_color": [0.72, 0.55, 0.45], "resolution": 160, "head_resolution": 96})
def norm(v): l = math.sqrt(sum(x*x for x in v)); return [x/l for x in v]
def add(a, b, k=1.0): return [a[i] + b[i]*k for i in range(3)]
def cross(a, b): return [a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0]]
leather = [0.30, 0.1, 0.07]
outfit = [
  {"name": "tunic", "type": "box", "center": [0, 1.3, 0], "size": [0.6, 0.62, 0.42], "op": "cover", "offset": 0.012, "smooth": 0.03, "color": leather, "material": "leather", "color_blend": 0},
  {"name": "skirt", "type": "box", "center": [0, 0.98, 0], "size": [0.44, 0.28, 0.36], "op": "cover", "offset": 0.03, "smooth": 0.05, "color": [0.22, 0.07, 0.05], "material": "leather", "color_blend": 0},
  {"name": "pants", "type": "box", "center": [0, 0.6, 0], "size": [0.56, 0.5, 0.4], "op": "cover", "offset": 0.008, "smooth": 0.02, "color": [0.14, 0.12, 0.1], "material": "cloth", "color_blend": 0},
  {"name": "boots", "type": "box", "center": [0, 0.19, 0.04], "size": [0.6, 0.38, 0.5], "op": "cover", "offset": 0.014, "smooth": 0.03, "color": [0.09, 0.07, 0.055], "material": "leather", "color_blend": 0},
  {"name": "belt", "type": "box", "center": [0, 1.07, 0], "size": [0.5, 0.07, 0.4], "op": "cover", "offset": 0.026, "color": [0.07, 0.05, 0.04], "material": "leather", "color_blend": 0},
  {"name": "bracer", "type": "capsule", "a": [0.38, 1.29, 0.02], "b": [0.52, 1.16, 0.22], "radius": 0.065, "op": "cover", "offset": 0.012, "color": [0.42, 0.42, 0.44], "material": "metal", "color_blend": 0, "mirror": True},
  {"name": "glove", "type": "sphere", "center": [0.55, 1.12, 0.27], "radius": 0.09, "op": "cover", "offset": 0.006, "color": [0.08, 0.06, 0.05], "material": "leather", "color_blend": 0, "mirror": True},
  {"name": "pauldron", "type": "ellipsoid", "center": [0.21, 1.53, 0.0], "radii": [0.12, 0.075, 0.12], "color": [0.5, 0.5, 0.53], "material": "metal", "blend": 0.01, "mirror": True, "bone": "upperarm.L"},
]
call("edit_clay", {"target": "Hero", "add": outfit})
call("edit_clay", {"target": "Hero_head", "add": [
  {"name": "hair", "type": "ellipsoid", "center": [0, 1.84, -0.0], "radii": [0.115, 0.1, 0.135], "op": "cover", "offset": 0.008, "smooth": 0.01, "color": [0.1, 0.07, 0.045], "material": "hair", "color_blend": 0},
  {"name": "beard", "type": "ellipsoid", "center": [0, 1.66, 0.1], "radii": [0.07, 0.05, 0.06], "op": "cover", "offset": 0.006, "color": [0.1, 0.07, 0.045], "material": "hair", "color_blend": 0},
]})
hand = [-0.55, 1.12, 0.27]
d = norm([-0.143, -0.138, 0.207])
side = norm(cross(d, [0, 1, 0]))
sword = [
  {"name": "grip", "type": "capsule", "a": add(hand, d, -0.07), "b": add(hand, d, 0.09), "radius": 0.016, "color": [0.12, 0.08, 0.05], "material": "leather", "blend": 0.002},
  {"name": "pommel", "type": "sphere", "center": add(hand, d, -0.09), "radius": 0.026, "color": [0.6, 0.5, 0.3], "material": "metal", "blend": 0.003},
  {"name": "guard", "type": "capsule", "a": add(add(hand, d, 0.11), side, -0.11), "b": add(add(hand, d, 0.11), side, 0.11), "radius": 0.016, "color": [0.6, 0.5, 0.3], "material": "metal", "blend": 0.004},
  {"name": "blade", "type": "capsule", "a": add(hand, d, 0.12), "b": add(hand, d, 0.98), "radius": 0.022, "radius_b": 0.006, "color": [0.78, 0.78, 0.8], "material": "polished_metal", "blend": 0.004},
]
call("create_object", {"shape": "Clay", "name": "Hero_sword", "parent": "Hero", "bone": "hand.R", "blobs": sword, "resolution": 96})
call("animate", {"target": "Hero", "clip": "stance", "keys": [
  {"frame": 1, "bone": "upperarm.L", "rotate": [0, 0, -30]},
  {"frame": 1, "bone": "upperarm.R", "rotate": [0, 0, 30]},
  {"frame": 1, "bone": "forearm.L", "rotate": [-20, 0, 0]},
  {"frame": 1, "bone": "forearm.R", "rotate": [-35, 0, 0]},
]})

# ---- the monster: a gaunt 2.4 m ghoul with horns, claws and glowing eyes
call("create_human", {"name": "Ghoul", "human": {"sex": 1, "weight": 0.0, "muscle": 0.55, "height": 1.0, "proportions": 0.2, "age": 70}, "skin_color": [0.25, 0.28, 0.29], "resolution": 160, "head_resolution": 96})
claw = lambda n, a, b: {"name": n, "type": "capsule", "a": a, "b": b, "radius": 0.009, "radius_b": 0.0015, "color": [0.12, 0.1, 0.08], "material": "gloss", "blend": 0.004, "mirror": True, "bone": "hand.L"}
call("edit_clay", {"target": "Ghoul", "add": [
  {"name": "loincloth", "type": "box", "center": [0, 1.3, 0.0], "size": [0.44, 0.34, 0.4], "op": "cover", "offset": 0.02, "smooth": 0.05, "color": [0.11, 0.09, 0.07], "material": "cloth", "color_blend": 0},
  {"name": "rag", "type": "box", "center": [0, 1.1, 0.1], "size": [0.2, 0.3, 0.03], "rotate": [8, 0, 0], "color": [0.1, 0.085, 0.065], "material": "cloth", "blend": 0.03},
  claw("claw1", [0.59, 1.485, 0.444], [0.575, 1.44, 0.5]),
  claw("claw2", [0.676, 1.427, 0.488], [0.672, 1.37, 0.55]),
  claw("claw3", [0.711, 1.387, 0.463], [0.715, 1.33, 0.52]),
  claw("claw4", [0.73, 1.376, 0.421], [0.74, 1.32, 0.47]),
]})
call("edit_clay", {"target": "Ghoul_head", "add": [
  {"name": "horn", "type": "chain", "points": [[0.06, 2.4, 0.07], [0.13, 2.49, 0.02], [0.19, 2.52, -0.07], [0.2, 2.46, -0.16]], "size": [0.055, 0.055, 0.055], "taper": 0.12, "color": [0.72, 0.66, 0.55], "material": "clay", "blend": 0.015, "mirror": True},
  {"name": "brow", "type": "ellipsoid", "center": [0, 2.315, 0.145], "radii": [0.075, 0.022, 0.03], "color": [0.23, 0.26, 0.27], "material": "skin", "blend": 0.02},
]})
for e in ["Ghoul_eye.L", "Ghoul_eye.R"]:
    call("update_object", {"target": e, "material": "glow", "color": [1.0, 0.15, 0.05]})
call("animate", {"target": "Ghoul", "clip": "lurch", "keys": [
  {"frame": 1, "bone": "spine", "rotate": [18, 0, 0]},
  {"frame": 1, "bone": "chest", "rotate": [12, 0, 0]},
  {"frame": 1, "bone": "neck", "rotate": [-20, 0, 0]},
  {"frame": 1, "bone": "upperarm.L", "rotate": [-35, 0, -10]},
  {"frame": 1, "bone": "upperarm.R", "rotate": [-45, 0, 15]},
  {"frame": 1, "bone": "forearm.L", "rotate": [-30, 0, 0]},
  {"frame": 1, "bone": "forearm.R", "rotate": [-25, 0, 0]},
  {"frame": 1, "bone": "thigh.L", "rotate": [-20, 0, 0]},
  {"frame": 1, "bone": "shin.L", "rotate": [30, 0, 0]},
]})
call("update_object", {"target": "Hero", "translate": [1.1, 0, 1.0], "rotate": [0, -151, 0]})
call("update_object", {"target": "Ghoul", "translate": [-0.4, 0, -1.7], "rotate": [0, 29, 0]})
call("playback", {"target": "Hero", "frame": 1, "clip": "stance"})
call("playback", {"target": "Ghoul", "frame": 1, "clip": "lurch"})
call("save_scene", {"path": OUT + "/crypt_mcp.json"})
