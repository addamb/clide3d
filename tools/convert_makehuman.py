#!/usr/bin/env python3
"""Converts MakeHuman's CC0 assets (base mesh, joint markers, targets) into
assets/makehuman/human.bin.gz, the compact form Clide3D embeds (see
src/human.rs for the reader and the file layout).

Usage: convert_makehuman.py <base.obj> <targets dir> <out.bin.gz>

The sources are MakeHuman's makehuman/data/3dobjs/base.obj and
makehuman/data/targets (github.com/makehumancommunity/makehuman).

Only data is used (the assets are CC0, see assets/makehuman/LICENSE.md);
no MakeHuman program code. Units: MakeHuman decimeters become meters and the
mesh is lifted so the ground joint is at y = 0. Target deltas are stored
in steps of 0.05 mm.
"""
import os, struct, sys

obj, tdir, out = sys.argv[1:4]
verts, body_faces, groups = [], [], {}
group = None
for line in open(obj):
    if line.startswith("v "):
        verts.append([float(x) for x in line.split()[1:4]])
    elif line.startswith("g "):
        group = line.split()[1]
    elif line.startswith("f "):
        idx = [int(t.split("/")[0]) - 1 for t in line.split()[1:]]
        groups.setdefault(group, set()).update(idx)
        if group == "body":
            body_faces.append(idx)
# Joint markers and eyeballs as point lists, so they follow the targets.
markers = {g: sorted(v) for g, v in groups.items() if g.startswith("joint-") or g in ("helper-l-eye", "helper-r-eye")}
ground = sum(verts[i][1] for i in markers["joint-ground"]) / len(markers["joint-ground"])
M = lambda p: [p[0] * 0.1, (p[1] - ground) * 0.1, p[2] * 0.1]

used = set(i for f in body_faces for i in f) | set(i for v in markers.values() for i in v)

def adult(rel):
    if any(w in rel for w in ("-baby", "-child")):
        return False
    # Height and proportion targets exist for every weight/muscle mix; the
    # average one is enough, since weight and muscle have their own targets.
    if rel.startswith(("macrodetails/height/", "macrodetails/proportions/")):
        return "averagemuscle-averageweight" in rel
    return True

targets = []
for root, _, files in os.walk(tdir):
    for f in sorted(files):
        if not f.endswith(".target"):
            continue
        rel = os.path.relpath(os.path.join(root, f), tdir)[:-len(".target")]
        if not adult(rel):
            continue
        entries = []
        # Only points on the body surface, and only moves of 0.05 mm or more.
        for line in open(os.path.join(root, f)):
            if line.startswith("#") or not line.strip():
                continue
            i, dx, dy, dz = line.split()
            q = [round(float(d) * 0.1 / 5e-5) for d in (dx, dy, dz)]
            if any(abs(v) > 32767 for v in q):
                raise SystemExit(f"{rel}: delta too large")
            if int(i) in used and max(abs(v) for v in q) >= 1:
                entries.append((int(i), *q))
        if entries:
            targets.append((rel, entries))

b = bytearray(b"MHB1")
b += struct.pack("<I", len(verts))
for p in verts:
    b += struct.pack("<3f", *M(p))
b += struct.pack("<I", len(body_faces))
for f in body_faces:
    f = (f + [0xFFFFFFFF] * 4)[:4]
    b += struct.pack("<4I", *f)
b += struct.pack("<H", len(markers))
for name, idx in sorted(markers.items()):
    n = name.encode()
    b += struct.pack("<B", len(n)) + n + struct.pack("<H", len(idx)) + struct.pack(f"<{len(idx)}I", *idx)
b += struct.pack("<H", len(targets))
for name, entries in targets:
    n = name.encode()
    b += struct.pack("<H", len(n)) + n + struct.pack("<I", len(entries))
    for e in entries:
        b += struct.pack("<H3h", *e)
import gzip
open(out, "wb").write(gzip.compress(bytes(b), 9))
sizes = {}
for name, entries in targets:
    sizes[name.split("/")[0]] = sizes.get(name.split("/")[0], 0) + len(entries) * 8
for k, v in sorted(sizes.items(), key=lambda kv: -kv[1]):
    print(f"  {k}: {v / 1e6:.2f} MB")
print(f"{len(verts)} vertices, {len(body_faces)} body faces, {len(markers)} markers, {len(targets)} targets, {len(b) / 1e6:.1f} MB raw")
