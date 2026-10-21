# Paint acceptance checks

`sheet_<preset>.png` shows spheres (back row) and bevelled boxes (front row).
Columns run left to right at amounts **0, 0.3, 0.7, 1**.

| Preset | Sheet |
| --- | --- |
| Factory new | [View](sheet_factory_new.png) |
| Military paint | [View](sheet_military_paint.png) |
| Battle worn | [View](sheet_battle_worn.png) |
| Desert dusty | [View](sheet_desert_dusty.png) |
| Muddy | [View](sheet_muddy.png) |
| Burnt wreck | [View](sheet_burnt_wreck.png) |
| Dusty khaki | [View](sheet_dusty_khaki.png) |
| Olive drab | [View](sheet_olive_drab.png) |
| Gunmetal | [View](sheet_gunmetal.png) |
| Worn steel | [View](sheet_worn_steel.png) |

 All use the same
camera, lighting, sampling and display transform. Muddy uses a black rubber
base to demonstrate mud contrast; the other sheets use khaki. Separate sheets
cover factory_new, military_paint, battle_worn, desert_dusty, muddy and
burnt_wreck. The four color/metal palette sheets use smart-layer opacity
for the same four blend amounts. Sizes are meters; the paint-grain map tiles at 120 mm and the handling
scratch map at 450 mm (each tile contains many smaller features), rather than stretching one pattern over an entire asset.

The mech check copies mech2/mech.json without writing the original. It
reconnects legacy `damage_*` material copies to their original shared names,
removes generated runoff, preserves authored damage and markings, resets
old tan paint to dusty khaki, and applies military paint, battle wear and
desert dust, plus low mud on the track finishes. The full scene, including kitbash parts, terrain, rocks and
soldier, is rendered through the same native pipeline used by `render_film`.
`mech_before.json` and `mech_after.json` are local, ignored scene copies.

Before/after pairs use identical cameras and neutral daylight. Front and
70 mm shoulder views reproduce the subject/framing of the source pictures;
the exact original camera recipes were not saved with the source JSON.
The adjacent render metadata records the reproducible camera and lighting.
`comparison_front.png` and `comparison_detail.png` place the control on the
left and the new finish on the right. Frame 0 evaluates the saved standing pose.
The source and untouched copy have SHA-256
`6cc3216128fdf5aa7e4c4efbdc2e30b20ac427fe55efe411b1d0c9700d1e002b`.

Reproduce:

```sh
cargo test --release paint_material_sheet -- --ignored --nocapture
PAINT_SCENE=/absolute/path/to/mech2/mech.json \
  cargo test --release paint_mech_films -- --ignored --nocapture
cargo test --release paint_comparison_sheets -- --ignored --nocapture
```

`PAINT_SAMPLES` defaults to 128; `PAINT_AFTER_ONLY=1` skips control renders.
The normal test suite verifies coverage progression, repeatability, shared
material editing, geometric edge masks independent of shading normals,
point target ownership, concise material replies and color/normal/ORM bake
and GLB round trips.

Wear now edits **shared materials**. All objects using those materials receive
the finish, and later base-material edits continue to apply. Reapplying wear also reconnects
legacy `damage_<id>_<slot>_<base>` copies when the base exists (unused old
entries are retained in the library). Reapplication
replaces that preset's layers and runoff. `factory_new` removes wear layers
and generated runoff, preserving explicit stamps. Editing tools return counts
and changed fields; a `set_material` call without a name still lists the library.

Validation: `cargo build --release`, `cargo test --release` (271 passed,
35 ignored visual/benchmark checks), and
`cargo clippy --release --all-targets -- -D warnings` pass. The material
sheet and mech visual checks are run separately through Metal.

Round 2 investigates the actual blob-built, sharp, resolution-150 missile pod.
[Original chip probe](missile_chip_before.png) and
[corrected chip probe](missile_chip_after.png) isolate primer/steel masks:
white means a chip, dark means retained paint. `missile_pod_fixture.json`
retains the source pod's geometry so its regression needs no external scene.

The original probe shows broad ragged bands over rounded bevels and lid
borders; its central side face is clean. Thus the observed problem was not
uniform failure of every planar vertex. Triangle-normal curvature divided by
short QEF edges was nevertheless unstable under sub-cell geometry noise:
during investigation the translated, blob-built 150-cell box regression measured **9.39%**
face-center chip coverage with the old detector and **0%** with the SDF
measurement (final edge-region coverage **12.26%**). The stress test adds bounded
3 mm position noise within a roughly 8 mm cell; this is a robustness test,
not a claim that every source pod vertex has that error. The unmodified pod
also verifies zero chip coverage across **3,060** central side vertices.

Sharp hard-surface clay masks now measure the continuous CSG distance field, using signed
second differences over a 2.5–12.5 mm radius rather than dividing mesh
normal changes by triangle lengths. The stencil is independent of fine/rest
tessellation and ignores paint relief and tiny triangles. An angular deadband
rejects floating-point noise on planes. Bake weathering and runoff source
selection use the same field measurement. Mesh-only kit parts retain their
geometric detector. Smooth organic clay retains mesh curvature so sculpted
folds still contribute. Analytic human models also keep the mesh detector to
avoid repeated evaluation of nested body covers. Chip breakup uses 25 mm clusters and 8 mm detail;
noise multiplies the geometric mask and cannot start chips on clean faces.
A sparse 120 mm handling field gates those flakes so every panel does not
receive an identical dotted outline. Brown primer and dark warm steel replace
the cool exposed colors. Bare steel uses the higher threshold and lower
roughness, leaving a matte primer margin at the most abraded chips.

The larger grain/scratch texture tiles and stronger relief survive film
filtering at mech scale. At an 8 mm footprint the close-up regression records
mean tangent-normal magnitude **0.088** and roughness **0.592–0.765**.
Upward dust opacity increased from 0.24 to 0.42; vertical paint stays mostly
free of dust, and baked normal/ORM and GLB round-trip checks remain in place.
The physical stencil limits the sampling footprint; rounded bevel geometry,
mesh interpolation and object scaling still affect the apparent band width.

Reproduce the diagnostic (same `PAINT_SCENE` as above):

```sh
PAINT_SCENE=/absolute/path/to/mech2/mech.json PAINT_SAMPLES=32 \
  cargo test --release paint_missile_chip_probe -- --ignored --nocapture
cargo test --release paint_checks -- --nocapture
```

Visual assessment: both final comparison images were opened and inspected
at the end of the second full-scene iteration. The right-hand finish is more
convincing field-worn military paint: desaturated chalky armor, fine relief
across broad faces, scratches on the shoulder side, dusty upper surfaces,
and intermittent warm primer/steel chips at real rims. The broad blue-grey
camouflage patches are gone. The first new full render still outlined too
many panel borders; the final handling gate roughly halves the box test's
edge coverage while leaving centers clean. The front view keeps the armor
readable rather than covering the whole silhouette in a dark dotted outline.

Still missing: the relief is an approximation using existing stone-grain and
scratched-metal maps, not a measured sprayed-paint/orange-peel scan. Wear is
procedurally placed rather than authored from the vehicle's specific service
history; fingerprints and individual tool/boot contact marks would improve
it. Broad modeled fillets, simplified panel construction, emissive optics
and the placeholder soldier still limit overall realism. This is a clear
surface improvement toward the worn military-vehicle reference, not proof
of parity with a hand-authored Call of Duty hero asset.
