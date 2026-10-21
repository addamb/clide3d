"""Details the LS-M1 mech through the Clide3D MCP tools, as an agent would:
one detail_pass per level, then a few kitbash hand placements, and renders
the same film views as the original pictures.

Run the editor with CLIDE3D_MCP_PORT=7891, then:
  python3 kitbash_check/build_mech.py before /abs/mech_copy.json   # render the original
  python3 kitbash_check/build_mech.py after  /abs/mech_copy.json   # passes, placements, render
Pictures go next to this script (mech_*_before.png / mech_*_after.png).
The original mech.json is never written: the copy is opened and the
detailed scene is saved as mech_kitbash.json next to the copy."""
import json, os, re, sys, time, urllib.request, base64

OUT = os.path.dirname(os.path.abspath(__file__))
PORT = os.environ.get("PORT", "7891")


def call(tool, args={}, quiet=False):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/mcp", data=body, headers={"Content-Type": "application/json"})
    t = time.time()
    r = json.loads(urllib.request.urlopen(req, timeout=900).read())["result"]
    texts = [c["text"] for c in r["content"] if c["type"] == "text"]
    if r.get("isError"):
        print("ERROR", tool, json.dumps(args)[:300], texts)
        if tool not in ("kitbash", "detail_pass"):
            sys.exit(1)
        return None
    out = json.loads(texts[0]) if texts and texts[0].startswith("{") else texts
    # Long calls answer with a job; wait for it.
    while isinstance(out, dict) and str(out.get("status", "")).startswith(("running", "waiting in line", "done, answer pending")):
        job = out.get("job") or int(re.search(r"id (\d+)", out.get("next", "")).group(1))
        out = call("job_status", {"id": job, "wait": 45}, quiet=True)
        if isinstance(out, dict) and "job" not in out and str(out.get("status", "")).startswith(("running", "waiting", "done, answer")):
            out["job"] = job
    if not quiet:
        brief = {k: out[k] for k in ("group", "placed", "copies", "mirrored", "triangles_drawn", "unique_triangles", "panels", "plates", "hatches", "kinds", "skipped", "made", "failed", "missed", "seconds", "saved", "along_borders", "wanted") if isinstance(out, dict) and k in out}
        print(f"{tool} {time.time() - t:.1f}s", json.dumps(brief)[:700])
    return out


FRONT = {"views": [{"yaw": 0, "pitch": 4}], "center": [0.55, 3.55, 0.3], "radius": 4.2, "aspect": 0.85, "size": 1360, "samples": 256, "time_limit": 480, "frame": 0}
DETAIL = {"views": [{"yaw": 28, "pitch": 14, "fov": 30}], "center": [0.7, 5.75, 0.4], "radius": 1.55, "aspect": 1.5, "size": 1500, "samples": 256, "time_limit": 480, "frame": 0}


def render(tag):
    for name, look in (("front", FRONT), ("detail", DETAIL)):
        call("render_film", dict(look, save_path=f"{OUT}/mech_{name}_{tag}.png"))


ARMOR = ["Torso", "Pauldron.L", "Pauldron.R", "Forearm.L", "Forearm.R", "Pelvis", "Thigh.L", "Thigh.R", "Shin.L", "Shin.R", "MissilePod"]
LEFT = ["Torso", "Pauldron.L", "Forearm.L", "Pelvis", "Thigh.L", "Shin.L", "MissilePod"]


def detail():
    paint = {"metal": "gunmetal_local", "dark": "gunmetal_local"}
    # Secondary: plates and seams on the big flat faces, a few hatches.
    call("detail_pass", {"pass": "panel", "targets": LEFT, "mirror": True, "seed": 7, "hatches": 3, "materials": paint})
    # Vents, boxes, lights and handles on the torso and legs.
    call("detail_pass", {"pass": "greeble", "targets": ["Torso", "Pelvis", "Shin.L", "Thigh.L", "Forearm.L"], "mirror": True, "seed": 11, "density": 1.6, "size": [0.2, 0.6], "parts": [{"kind": "vent", "style": "louver", "weight": 2}, {"kind": "vent", "style": "grille"}, {"kind": "greeble", "style": "junction"}, {"kind": "greeble", "style": "canister"}, {"kind": "greeble", "style": "pipes"}, {"kind": "connector"}], "paint": "gunmetal_local", "color": [0.2, 0.2, 0.21], "materials": paint})
    call("detail_pass", {"pass": "greeble", "targets": ["Torso", "Pelvis", "Shin.L", "Thigh.L", "Forearm.L"], "mirror": True, "seed": 12, "density": 1.4, "size": [0.18, 0.5], "parts": [{"kind": "greeble", "style": "box", "weight": 2}, {"kind": "handle", "style": "bar"}, {"kind": "vent", "style": "slots"}, {"kind": "light", "style": "round", "weight": 0.5}, {"kind": "latch"}], "materials": paint})
    # Tertiary: bolts along every plate border and hard edge.
    call("detail_pass", {"pass": "bolt", "targets": LEFT, "mirror": True, "seed": 7, "paint": "gunmetal_local", "color": [0.24, 0.24, 0.25], "materials": paint})
    # Hoses and cable bundles across the hips and knees.
    call("detail_pass", {"pass": "cable", "targets": ["Pelvis", "Thigh.L", "Shin.L"], "mirror": True, "seed": 5, "materials": paint})
    # Hand placements (points found with surface_point, view front/right/top):
    # pistons behind the knees, hoses at the hips, lamps, handles, latches,
    # spare track links, exhaust stacks and an antenna.
    call("kitbash", {"part": "piston", "targets": ["Thigh.L", "Shin.L"], "from": [1.02, 2.85, -0.32], "to": [1.14, 1.75, -0.36], "radius": 0.07, "mirror": True, "materials": paint})
    call("kitbash", {"part": "piston", "targets": ["Thigh.L", "Shin.L"], "from": [1.0, 2.75, 0.62], "to": [1.15, 2.12, 0.69], "radius": 0.055, "mirror": True, "materials": paint})
    call("kitbash", {"part": "hose", "targets": ["Pelvis", "Thigh.L"], "from": [0.55, 3.45, 0.42], "via": [[0.85, 3.25, 0.5]], "to": [0.98, 3.0, 0.38], "radius": 0.035, "clips": 1, "mirror": True})
    call("kitbash", {"part": {"kind": "light", "style": "cage"}, "target": "Torso", "at": [0.72, 4.45, 0.98], "normal": [0, 0, 1], "mirror": True, "glow": [1.0, 0.85, 0.55]})
    call("kitbash", {"part": {"kind": "light", "style": "rect"}, "target": "Shin.L", "at": [1.15, 1.75, 0.67], "normal": [0, 0, 1], "size": [0.22, 0.07, 0.1], "mirror": True, "glow": [1.0, 0.9, 0.7]})
    call("kitbash", {"part": {"kind": "handle", "style": "bar"}, "target": "Torso", "at": [0.0, 4.75, 0.66], "materials": paint})
    call("kitbash", {"part": {"kind": "handle", "style": "bar"}, "target": "Shin.L", "at": [1.44, 1.6, 0.67], "normal": [0, 0, 1], "along": [0, 1, 0], "mirror": True, "materials": paint})
    call("kitbash", {"part": {"kind": "latch", "style": "draw"}, "target": "Torso", "at": [0.86, 5.3, 0.3], "mirror": True, "materials": paint})
    call("kitbash", {"part": "track_link", "target": "Shin.L", "at": [1.72, 1.35, 0.05], "normal": [1, 0, 0], "along": [0, 1, 0], "count": 3, "mirror": True, "materials": paint})
    call("kitbash", {"part": {"kind": "exhaust", "style": "stack"}, "target": "Torso", "at": [0.55, 6.45, -0.15], "normal": [0, 1, 0], "size": [0.15, 0.55, 0.15], "mirror": True})
    call("kitbash", {"part": {"kind": "antenna", "style": "blade"}, "target": "Torso", "at": [0.0, 6.3, -0.3], "normal": [0, 1, 0], "size": [0.08, 0.5, 0.08]})

if __name__ == "__main__":
    what, scene = sys.argv[1], sys.argv[2]
    call("open_scene", {"path": scene})
    if what == "after":
        detail()
        call("save_scene", {"path": os.path.join(os.path.dirname(scene), "mech_kitbash.json")})
        print(json.dumps(call("kitbash", {"action": "list"}, quiet=True))[:1500])
    if what == "preview":
        detail()
        for name, look in (("front", FRONT), ("detail", DETAIL)):
            r = urllib.request.Request(f"http://127.0.0.1:{PORT}/mcp", data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "render_film", "arguments": dict(look, samples=24, size=900, time_limit=120)}}).encode(), headers={"Content-Type": "application/json"})
            res = json.loads(urllib.request.urlopen(r, timeout=900).read())["result"]
            for c in res["content"]:
                if c["type"] == "image":
                    open(f"{OUT}/preview_{name}.png", "wb").write(base64.b64decode(c["data"]))
    elif what != "none":
        render(what)
    if what == "after":
        out = call("export_glb", {"path": os.path.join(os.path.dirname(scene), "mech_kitbash.glb")}, quiet=True)
        print("export", json.dumps({k: out.get(k) for k in ("triangles", "scatter_copies", "meshes")}))
