"""Re-poses and re-mirrors the LS-M1 mech with the pose tools, as an agent
would, and renders the `fire` clip to show two-bone hoses and pistons.

Run the editor with CLIDE3D_MCP_PORT=7892, then:
  python3 pose_check/build_pose.py /abs/copy/of/mech.json
Pictures go next to this script. The copy is opened and the result is
saved as mech_pose.json next to the copy (the original is never written).

Steps:
  1. fire_before.png, legs_before.png, arm_before.png: 8 frames of `fire`
     as the scene was (one-bone hose and pistons: the hose's gun end leaves
     the cannon, the rods leave the shins).
  0. a knee brace and a raised cannon are keyed into `fire` (so the joints
     the hose and pistons cross really move), then the before sheets.
  2. pose in place: the left forearm is aimed down (pose_bone carries the
     forearm armor, hand, autocannon and their kitbash parts), the
     autocannon is turned to hang straight down (align), the right upper
     arm swings out a little (bazooka and all).
  3. the ammo hose (chest -> left hand) is placed again from the drum to
     the cannon's feed port; its ends land on the drum (chest) and the
     autocannon (hand.L), so it becomes two-bone. The leg pistons become
     two-bone (thigh -> shin) with kitbash span.
  4. the right leg (thigh, shin, track) is rebuilt with mirror_object from
     the left one, linked, replacing the old negative-scale copies, with
     their panels, bolts, greebles, stencils and two-bone pistons.
  5. fire_after.png, legs_after.png (the pistons from behind),
     arm_after.png (the hose from behind) and a film frame.
"""
import json, os, re, sys, time, urllib.request

OUT = os.path.dirname(os.path.abspath(__file__))
PORT = os.environ.get("PORT", "7892")


def call(tool, args={}, quiet=False):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}}).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/mcp", data=body, headers={"Content-Type": "application/json"})
    t = time.time()
    r = json.loads(urllib.request.urlopen(req, timeout=900).read())["result"]
    texts = [c["text"] for c in r["content"] if c["type"] == "text"]
    if r.get("isError"):
        print("ERROR", tool, json.dumps(args)[:300], texts)
        sys.exit(1)
    out = json.loads(texts[0]) if texts and texts[0].startswith("{") else texts
    # Long calls answer with a job; wait for it.
    while isinstance(out, dict) and str(out.get("status", "")).startswith(("running", "waiting in line", "done, answer pending")):
        job = out.get("job") or int(re.search(r"id (\d+)", out.get("next", "")).group(1))
        out = call("job_status", {"id": job, "wait": 45}, quiet=True)
        if isinstance(out, dict) and "job" not in out and str(out.get("status", "")).startswith(("running", "waiting", "done, answer")):
            out["job"] = job
    if not quiet:
        print(f"{tool} {time.time() - t:.1f}s", json.dumps(out)[:600])
    return out


WHOLE = {"view": "three_quarter", "center": [0.0, 3.7, 0.0], "radius": 4.3, "size": 384}
LEGS = {"view": {"yaw": 160, "pitch": 8}, "center": [1.0, 2.2, -0.5], "radius": 1.8, "size": 384}
ARM = {"view": {"yaw": 120, "pitch": 10}, "center": [1.6, 3.6, -0.8], "radius": 2.4, "size": 384}


def sheets(tag):
    call("render_animation", dict(WHOLE, clip="fire", frames=8, save_path=f"{OUT}/fire_{tag}.png"))
    call("render_animation", dict(LEGS, clip="fire", frames=8, save_path=f"{OUT}/legs_{tag}.png"))
    call("render_animation", dict(ARM, clip="fire", frames=8, save_path=f"{OUT}/arm_{tag}.png"))


def world_point(obj, local):
    """A local point of an update_object reply's object, world (rest)."""
    pivot = PIVOTS[obj["name"]]
    axes = obj["world_axes_at_rest"]
    out = list(obj["pivot_world"])
    for i, k in enumerate("xyz"):
        d = (local[i] - pivot[i]) * abs(obj["scale"][i])
        out = [out[j] + axes[k][j] * d for j in range(3)]
    return out


PIVOTS = {"Autocannon": [2.21, 2.607143, 0.25]}


def main(path):
    call("open_scene", {"path": path})
    # The clip gets a knee brace and a raised cannon in the burst first, so
    # before and after show the same motion.
    keys = [{"bone": "thigh.L", "frame": f, "rotate": r, "mirror": True} for f, r in ((0, [0, 0, 0]), (3, [-18, 0, 0]), (12, [0, 0, 0]), (24, [0, 0, 0]))]
    keys += [{"bone": "shin.L", "frame": f, "rotate": r, "mirror": True} for f, r in ((0, [0, 0, 0]), (3, [38, 0, 0]), (12, [0, 0, 0]), (24, [0, 0, 0]))]
    keys += [{"bone": "upper_arm.L", "frame": f, "rotate": r} for f, r in ((0, [0, 0, 0]), (4, [-30, 0, 12]), (10, [-30, 0, 12]), (18, [0, 0, 0]), (24, [0, 0, 0]))]
    call("animate", {"target": "Frame", "clip": "fire", "keys": keys})
    sheets("before")

    # 2. Pose in place: the left forearm aims down (carrying its armor,
    # hand, cannon and their parts), the cannon hangs straight down.
    call("pose_bone", {"bone": "forearm.L", "aim": [2.18, 2.7, 0.0]})
    cannon = call("update_object", {"target": "Autocannon", "align": {"forward": [0.0, -1.0, 0.12], "up": [0.0, 0.12, 1.0]}})
    call("pose_bone", {"bone": "upper_arm.R", "rotate": [0, 0, -7], "space": "world"})

    # 3. Two-bone ammo hose (drum on the chest -> the cannon's feed port in
    # the left hand) and leg pistons (thigh -> shin).
    feed = world_point(cannon, [1.88, 1.86, -0.35])
    start = [1.28, 4.6, -1.55]
    via = [[1.75, 4.0, -1.55], [(1.9 + feed[0]) / 2 + 0.25, (4.0 + feed[1]) / 2, (-1.55 + feed[2]) / 2 - 0.2]]
    call("kitbash", {"action": "remove", "group": "ammo_hose"})
    call("kitbash", {"part": {"kind": "corrugated_tube", "radius": 0.11}, "targets": ["Drum", "Autocannon"], "group": "ammo_hose", "from": start, "via": via, "to": feed})
    for g in ("leg_pistons_thigh.L", "leg_pistons2_thigh.L"):
        call("kitbash", {"action": "span", "target": g, "bones": ["thigh.L", "shin.L"]})

    # 4. The right leg from the left one (with its parts and pistons).
    for part in ("Thigh.L", "Shin.L", "Track.L"):
        call("mirror_object", {"target": part, "replace": True, "linked": True})

    call("save_scene", {"path": os.path.join(os.path.dirname(path), "mech_pose.json")})
    sheets("after")
    call("playback", {"target": "Frame", "clip": "fire"})
    call("render_film", {"views": [{"yaw": 140, "pitch": 8}], "center": [0.9, 3.4, -0.6], "radius": 3.3, "aspect": 1.0, "size": 1000, "samples": 128, "time_limit": 400, "frame": 4, "save_path": f"{OUT}/film_fire_frame4.png"})


if __name__ == "__main__":
    main(sys.argv[1])
