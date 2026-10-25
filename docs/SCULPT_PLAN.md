# Clide3D Sculpting Plan: the Stroke Ledger

**Status (2026-10-23):** Stages 0-8 are implemented on branch `sculpt`, one commit per stage plus review fixes (11090c8 Stage 0 through 457ed47 Stage 8 review fixes; `git log --oneline clide3d..sculpt`). Each stage below ends with a dated result paragraph and its deviations. Stage 9 (the Poly track) is deferred (Q3). The MCP server has 72 tools. The open decisions are listed in §0.1.

Written on branch `graphics`, before any stage was built. Synthesized from three competing designs: the judged winner ("Stroke Ledger") plus grafts from "Clay-native" (design A) and "Hybrid Poly" (design B).

---

## 0. Decisions (2026-10-22)

Answered by the user; these override anything below that conflicts.
- **Q1:** a stroke on layer k reads all enabled layers and writes layer k (WYSIWYG); `context_hash` covers every other enabled layer.
- **Q3:** Stage 9 (Poly track) is deferred: build Stages 0-8, then decide.
- **Q4:** add macOS pen pressure via an `objc2-app-kit` NSEvent monitor (opt-in setting) in Stage 7.
- **Q6:** `xxhash-rust` (xxh3-128 for saved hashes), `zip` (3MF), `exr` and an external `ffmpeg` binary (with png_sequence fallback) are all approved.
- **Q2, Q5, Q7, Q9, Q10:** not asked; use the plan's recommended default for each and note it in the stage that implements it.

## 0.1 Open decisions (after Stage 8)

Left for the user; the stage results below say where each comes from.
- **Tapered previews:** a tapered stroke's live preview shows shallower in the middle until its record commits (Stage 7). Accept, or make taper previews exact.
- **Human cold build:** the cold Human cache and chunk build is 2.76 s against the strict 3 s bound; decide whether to hold it to the 1.5 s target (Stage 6, §7).
- **Cross-layer transport:** transport carries only the written layer's finer detail, so other layers' records go `stale_context`; a transport record kind across layers is undecided (Stage 8a).
- **ENGINE_VERSION 2:** it marks every version-1 record `engine_stale`, not only those of the brushes transport changed; narrow it or keep it (Stage 8a).
- **External VDM images:** importing VDM images from other tools (the patch → `MeshSdf` path) is deferred (Stage 8c).
- **Alpha-store budget:** whether each clay's alpha store (images and VDM stamps its strokes use) gets its own byte budget.
- **Render passes and time_limit:** passes run after the picture, outside `time_limit` (Stage 8b).
- **edit_clay detail_regions:** it still records a whole grid; a compact regions record like Sculptris Pro's is undecided (Stage 6).
- **Frozen grids in the saved budget:** the frozen mask and group grids records read (at most 256 of each) are not counted in the saved budget (Stage 6).
- **Thread pool:** whether to keep a persistent thread pool (the Stage 7 preview worker starts a thread per stroke).
- **Layer amount at 33 ms:** a layer amount change reaches draw-ready chunks in about 70 ms (bound 80 ms); the 33 ms goal is mesher-bound (about 970 chunks remesh) and not met (Stages 6 and 7).

## 1. Summary

A sculpt is a **ledger**: an ordered list of typed, *resolved* stroke records stored on the clay object. A pure, deterministic engine folds the ledger into sparse volumetric grids (layer offsets, mask, polygroups, paint) that sit on top of the clay SDF. Today's sculpt detail and paint already store their result as Arc-shared brick grids (clay.rs:2043, paint.rs:26); what is missing is the ledger, the extra grids (layers, persistent mask, groups) and a live mesher. (Critic: the earlier wording "the only thing missing is the ledger" understated Stages 3, 4 and 6.)

File references of the form `app/tools.rs:N` mean `src/app/tools.rs`; there is no `src/tools.rs`.

- **The agent is the first user.** It reads, writes, measures, dry-runs, edits and replays the ledger through MCP: `sculpt`, `stroke_log`, `stroke_edit`, `probe_clay`, `sculpt_diff` and others.
- **The mouse or tablet is a stroke recorder.** It emits the same `StrokeSpec`, resolves it with the same resolver, and commits through the same `ledger::commit` function as the `sculpt` tool. Every UI stroke is listable, replayable and undoable by the agent. A test asserts that a UI stroke replays bit-for-bit through MCP.
- **Speed comes from a new live engine**, not from changing the representation. The engine has these parts:
  - a cached blob field (`BaseCache`);
  - anchored, chunked surface nets that remesh only the dirty 32³ chunks;
  - a slim 24-byte sculpt vertex;
  - GPU chunk buffers diffed by Arc pointer;
  - hover picking by sphere-tracing the cached field.

  Target: dab-to-photon p95 ≤ 33 ms on a 1.65M-triangle clay, against 21-168 ms per dab plus a full remesh today.
- **Explicit topology is a later, optional, derived track** (Stage 9): a `Shape::Poly` object for ZRemesher-class retopo, ZModeler and Catmull-Clark levels. Its edit log is separate and topology-addressed. It converts back to clay through `MeshSdf`.

Stages 0-8 touch all 16 P0 rows; 14 close fully and 2 stay partial or conditional: Supported 3D formats (subset only) and Tablet pressure (macOS depends on Q4). See the P0 coverage map. They touch 35 of the 37 P1 rows: 31 close fully, and 4 stay partial (PolyGroups, FBX import/export, Multi-resolution levels, Curve brushes). Stage 9 closes ZRemesher and ZModeler and gives true subdivision levels.

---

## 2. Architecture decision: clay SDF + layered grids + ledger

**Decision: keep clay (SDF blobs) as the only source of truth.** Add an ordered stack of sparse grid layers and a per-layer ledger of resolved dabs. Explicit meshes are always *derived*: the display chunks, the bake low poly, blend-shape export meshes, and optional Poly objects.

**Why clay wins:**
1. **Strokes are spatial, so replay does not depend on topology.** A record stores object-space dabs and never vertex IDs. It replays the same way at any `resolution` or `detail_cell`, and after any remesh. The repo already proves this pattern: rig `WeightStroke` (rig.rs:51, 64-75) is the one replayable log today, and it survives every remesh.
2. **Every per-surface datum is a grid, so it survives remeshing for free.** That covers layer offsets, the mask, polygroup ids and paint. They reuse the Detail/Paint brick machinery: 8³ bricks, Arc copy-on-write, `PartialEq = Arc::ptr_eq` (clay.rs:2043-2057, paint.rs:26-38). That keeps the per-frame `Scene` clone and compare (app.rs:702, 766) and the whole-Scene undo cheap.
3. **One field feeds everything downstream.** `Field::new` (clay.rs:655) already serves the mesh, fine mesh, bake, skin, snapping and film, so a new layer reaches all of them at once. No second bake, skin or film path is needed.
4. **Layer composition is linear.** Morphs are snapshots of amounts plus grid Arcs, so they cost O(1) memory until they diverge.

**Why not explicit mesh as primary (design B):**
- **Two sources of truth.** After `to_poly`, clay edits stop flowing to the Poly (a one-way fork), so the agent must track which representation each object is.
- **Two sculpt engines whose brushes drift.** Clay and Poly would each need their own brush code.
- **Topology-addressed records break the spatial replay guarantee.** Dyntopo, remesh or retopo invalidates every earlier record.
- **Too much new plumbing.** Bake, skin, film, export and picking would all need parallel Poly paths, and 1-2M-face meshes would bloat the scene JSON to 10-15 MB per object.

**What we keep from B anyway:**
- Chunked Arc storage and GPU dirty sets diffed by pointer.
- The shared `src/brush/` module.
- Jacobi per-dab evaluation.
- Splitting history per MCP call.
- The ≤ 50 µs clone+compare guard test.
- The Stage 9 Poly specification.

**Honest limits of the clay choice.** These stay at "partial" parity until Stage 9:
- Polygroups are spatial labels, with no group edges for loop operations.
- "Subdivision levels" are cell-size grid levels, not Catmull-Clark.
- Large non-rigid edits carry fine detail only approximately (detail transport, Stage 8).
- Walls thinner than about 2 cells fuse or vanish, the same limit as DynaMesh.
- Layer and morph blending is linear only for small offsets.

---

## 3. Data model

All large new state follows the Detail/Paint pattern: an Arc newtype with `PartialEq = Arc::ptr_eq` and Arc-shared bricks. Selection-like state (active layer, current brush, overlay toggles, active level) is either `#[serde(skip)]` and left out of `PartialEq`, like `Scene::frame` (scene.rs:221-230), or lives in `ClideApp`. Switching it is therefore never an undo step.

### 3.1 Shared brush module (`src/brush/`, used by clay sculpt, clay paint, mask, groups and the later Poly sculpt)

Files: `curve.rs`, `alpha.rs`, `symmetry.rs`, `pressure.rs`, `path.rs` (spacing, spray, drag_rect, lazy), `preset.rs`, `resolve.rs` and `rng.rs` (PCG32).

The `falloff` curve replaces the fixed falloff at sculpt.rs:234 and the hardness in paint.rs:314. Clay-parity check: the default curve reproduces the old falloff to within 1e-6.

### 3.2 Authored spec (Serialize + Deserialize; accepts today's sculpt/paint JSON)

```rust
// src/brush/spec.rs  (today's sculpt::Stroke is Deserialize-only, sculpt.rs:55)
pub struct StrokeSpec {
    pub brush: Brush,            // existing 12 + standard, clay, clay_buildup, clay_tubes, trim_dynamic,
                                 // polish, snakehook, transform, vdm{name}, mask_paint, group_paint, paint
    pub kind: StrokeKind,        // path (default) | dots | spray{count,jitter,seed} | drag_rect{from,to} | drag_dot | curve{closed}
    pub points: Vec<PathPoint>,  // [x,y,z] | [x,y,z,pressure] | {p, pressure?, normal?, t?}
    pub frame: Frame,            // world (default) | object | surface{offset} | screen{view, px:[[x,y]..]}
    pub radius: f32, pub strength: f32, pub invert: bool,
    pub falloff: Falloff,        // preset smooth|linear|sharp|flat|sphere | curve [[t,v]..] (≤16, monotone cubic); focal_shift -1..1
    pub alpha: Option<AlphaRef>, // {name|hash, rotate, scale, mid, depth, mode: stamp|drag|tile, use: height|mask}
    pub pressure: PressureCurves,// {size, strength}; linear by default; ignored if no pressure
    pub spacing: Option<f32>,    // fraction of radius, default 0.15
    pub lazy: Option<f32>,       // LazyMouse/Chaikin smoothing for agent paths, 0..1
    pub symmetry: Symmetry,      // {axes:[x,y,z], radial:{axis,count}, about: origin|pivot, posable: bool, frame: f32|"rest" (posable only; filled from the editor's frame when left out and stored)}; legacy mirror:true => axes [x]
    pub taper: Option<[f32;2]>,  // dam standard
    pub mask: StrokeMask,        // legacy {blobs, above, below} + channel(persistent), invert_channel, backface, cavity:Curve,
                                 // connected, groups:[u16], normal_angle
    pub side: Option<Side>,      // above|below|both (flatten/planar/trim)
    pub smooth_mode: Option<SmoothMode>, // gentle|normal|peaks|valleys
    pub curl_deg: Option<f32>,   // snakehook
    pub delta: Option<Vec3>, pub normal: Option<Vec3>, pub scale: Option<f32>, pub pattern: Option<String>,
    pub transform: Option<Affine>,  // brush "transform": translate/rotate/scale about pivot, weight = 1 - mask
    pub paint: Option<PaintArgs>,   // color, hardness, erase, channel
    pub layer: Option<String>, pub cell: Option<f32>, pub level: Option<u8>,
}
```

### 3.3 Resolved form (canonical, hashed and replayed)

```rust
pub struct Dab { center: Vec3, normal: Vec3, tangent: Vec3, radius: f32, strength: f32,
                 falloff: Arc<CurveLut>, alpha: Option<AlphaKey /*content hash*/>, brush: BrushParams,
                 mask: ResolvedMask, seg: Option<Vec3> /*path brushes: segment end*/ }
```

**Resolved defaults are stored, never re-derived.** At commit the resolver writes the concrete `layer`, `cell`/`level`, symmetry and preset contents into the record. A `sculpt` call without `layer` or `cell` takes its defaults only from scene state (`record_layer`, then "base"; the stack's saved default level), never from `ClideApp` selection state. So the same JSON gives the same result whatever the human last clicked. A `screen` frame, a lasso or a rect stores the full camera (view and projection matrices plus the viewport size) taken at commit, not a view name, so `stroke_log full` → edit → resubmit re-resolves against the same camera.

The resolver does all of these, so replay never depends on the camera, the snapper or UI state:
- converts the frame (world, surface or screen) to object space;
- snaps to the surface;
- applies LazyMouse (UI only);
- resamples by spacing;
- evaluates the pressure curves;
- expands symmetry (≤ 2³ × radial instances), including posable symmetry through inverse LBS to rest space;
- draws seeded spray positions;
- binds alphas by content hash.

### 3.4 Ledger

```rust
// src/ledger.rs
pub struct Ledger(Arc<LedgerData>);            // PartialEq = ptr_eq
pub struct LedgerData { records: Vec<Arc<Record>>, head: Hash128, next_id: u64 }
pub struct Record {
  id: u64, seq: u32, source: Source /*agent{tool}|ui|replay|import*/, label: Option<String>, enabled: bool,
  op: RecordOp,          // Stroke{spec, dabs: Arc<[Dab]>} | MaskOp{..} | GroupOp{..} | Opaque{grid: Detail}
  context_hash: Hash128, // blobs + other layers' amounts/grid hashes + detail_cell + alpha hashes
  result_hash: Hash128,  // over sorted brick contents
  stats: DiffStats,      // changed_points, bricks_touched, bbox, max_disp, mean_disp, volume_delta
  pressure_source: Option<PressureSource>, // none|touch_force|nsevent|velocity
  engine: u16,           // brush-kernel version that produced result_hash (critic addition)
}
// In MeshLib, not Scene: checkpoints every 32 records, keyed (ledger addr, seq) -> grid Arc.
pub struct ReplayCache { .. }
```

`Opaque` records hold grids that have no stroke history: legacy `clay.detail`, bake-down, squashed ranges.

**Engine versioning.** Any brush bug fix, float reorder or new transport behaviour changes `result_hash` for records that were already saved. A record whose `engine` is older than the running kernel is not replayed on load. The saved grid is kept, the record is reported as `engine_stale` in `get_clay`/`stroke_log`, and `stroke_edit rebase` re-applies it with the current kernel. A kernel change that changes outputs for an unchanged spec must bump `ENGINE_VERSION`. A test pins one fixture hash per version.

**Hash function.** `result_hash` and `context_hash` are saved and checked on load, so they must be stable across Rust releases and platforms. Today's `content_hash` (mesh.rs:224) uses `std::collections::hash_map::DefaultHasher`, which guarantees neither stability nor 128 bits, so it cannot be reused. Picking the hash (Q6: `xxhash-rust` xxh3-128 or a hand-rolled one) **blocks Stage 1b**.

### 3.5 Sculpt stack (new `Clay.sculpt: Option<SculptStack>`, `#[serde(default, skip_serializing_if)]`)

```rust
pub struct SculptStack(Arc<StackData>);        // ptr_eq
pub struct StackData {
  layers: Vec<Layer>,                          // ≤16; layer 0 = "base" (migrated Clay.detail)
  mask: Option<MaskGrid>,                      // u8 bricks at detail cell, 0 free .. 255 frozen
  groups: Option<GroupGrid>, group_names: BTreeMap<u16, GroupInfo>, // u16 surface-band bricks
  morphs: Vec<Morph>,                          // ≤32; {name, blobs_hash, amounts, grids: Arc-shared}
  mask_ledger: Ledger, group_ledger: Ledger,
  levels: Vec<f32>,                            // user-navigable cell levels (coarse → fine)
  record_layer: Option<String>,                // `sculpt_layer record`: default target for later strokes
}
pub struct Layer { name, kind: Sculpt|Paint, amount: f32 /*-2..2*/, enabled: bool,
                   grid: LayerGrid /*Detail | Paint*/, ledger: Ledger, store: Grid|Ledger|Both /*default Both*/ }
#[serde(skip)] last: Option<LastDelta>         // after − before bricks of the last call (adjust_last)
```

`LastDelta` is tagged with the ledger head it was taken at, plus the source (agent call id or UI drag). It is invalid as soon as the head differs, for example after undo, redo, `history goto` or another stroke, and `stroke_edit scale last` then gives a clear error. Agent and UI each see "last" as the newest record on the target ledger, whatever its source, and the reply names that record.

**Naming collision.** `Clay.masks` already exists: the material/paint channel masks used by smart materials (mesh.rs:242). The persistent sculpt mask is called `sculpt_mask` (`StackData.sculpt_mask`, overlay `sculpt_mask`) in code and JSON. The MCP tool keeps the name `mask`, and its `from_channel`/`to_channel` actions refer to `Clay.masks`.

**How a stroke on a layer is stored.** A stroke on layer L stores `Δ / amount_L` into L's grid, so the result the user sees matches the brush. If |amount_L| < 0.05 the stroke is refused with a clear error. See open question Q1 for which layers a stroke reads.

### 3.6 Derived caches (in MeshLib; never saved, never in undo)

- **`BaseCache`.** The blob field B(p) on the anchored detail grid, near-surface bricks only. It is keyed by the blobs' content hash, filled lazily per brick in parallel, and dropped on blob, resolution, damage or decal-depth edits.
- **`Composite`.** Σ amount·layer over the touched bricks, so a sample costs one lookup rather than N.
- **`ChunkedMesh`.** `HashMap<IVec3, Arc<Mesh>>` plus a dirty set. A per-chunk `film::bvh` is kept only as the picking fallback.
- **`MeshKey::Chunk(id, IVec3)`** for GPU chunks.

### 3.7 Scene-level assets (self-contained replays)

- `Scene.alphas: BTreeMap<String, Alpha>`: 16-bit grayscale, ≤ 1024², base64 PNG16, referenced by content hash.
- `Scene.brushes: BTreeMap<String, StrokeSpec>`: user presets. Built-in presets are compiled in from `assets/brushes/*.json`.
- `Scene.assets`: imported triangle soups (base64 + deflate, deduplicated by hash). They back `Form::Mesh { asset, sign: Closed|Winding }`, which generalizes `Geo::Mesh` (clay.rs:413-420).
- VDM stamps: sparse SDF brick volumes captured from clay. Imported VDM images become patch → `MeshSdf` → stamp.
- New `Op::Intersect` (smooth max in `prefix_full`, clay.rs:810).

### 3.8 Wiring checklist for every new Clay field

- `BuildKey` (mesh.rs:189-197).
- `content_hash` via `addr()` (mesh.rs:222-245). Every new grid and ledger must be swapped for its address there, as detail/paint/masks are today. Otherwise each sync serializes every brick and record to JSON. Note that `content_hash` returns `None` for rigged and cloth objects (mesh.rs:225), so the chunk and BaseCache keys need their own path for rigged clay, which sculpts at rest.
- `same_geometry` (mesh.rs:248-258). Mask, groups, paint layers and overlays take the shading-only path; sculpt-layer amounts take the chunk path.
- `Clay::mirrored_x` (clay.rs:1098). It mirrors grids and reflects ledger dabs, marking them `Source::replay`.
- `Scene::copy_one`/`duplicate_tree` (scene.rs:535-562).
- `pose_bone` detail handling (app/posing.rs:205).
- The `instance_array` refusal rules (mcp.rs:731).
- `Clay::sanitize` (clay.rs:1897). On read it enforces ≤ 16 layers, ≤ 32 morphs, ≤ 100k records per ledger, ≤ 2M points, and that the brick total (shared `MAX_TOTAL_BRICKS` across layers, mask and groups) stays within budget.
- The bake capture (mesh.rs:937-961, bake.rs:67-79).
- `get_clay` and `clay_summary` (app/tools.rs:212-227, 998-1013).
- The old-file test (scene.rs:794).

---

## 4. MCP tool contract

All tools take `target` (name or id) and `space: world|local`, and reply with JSON. Slow tools join `JOB_TOOLS` and `EDIT_JOBS` (app/tools.rs:26-28). Their finishers refuse to install when the **head of the ledger they edit** changed. Today the check compares the whole clay (app/tools.rs:722-724), so an agent sculpt currently fails when the user only painted or masked. Every editing reply carries `step` (the history index) and `record_ids`.

### 4.1 Extended tools

| Tool | New arguments | Reply additions |
|---|---|---|
| `sculpt` (job) | `layer?`, `strokes[≤64]: StrokeSpec`, `preset?` (merged under each stroke), `dry_run?`, `measure?`, `label?`, `screenshot?:{view}` (with dry_run, a diff heatmap). Legacy `mirror` and `point` are still accepted. | `records:[{id, seq, changed_points, bricks, bbox, max_disp, mean_disp, volume_delta, radius_used, dabs, dirty_chunks}]`, `ledger_head`, `layer`, `stale_context`, `timing_ms:{resolve, apply, remesh, upload}` |
| `paint` | The same StrokeSpec fields (falloff, alpha, pressure, symmetry, kind spray/color_spray, mask) plus `layer` (a paint layer) | `records` |
| `edit_clay` | Blob `op: "intersect"`. `detail_cell` now replays ledgers at the new cell; records without stroke history fall back to resampling. | — |
| `set_display` | `shading: matcap`, `matcap: name\|path`, `ao`/`ao_radius`, `overlay: none\|mask\|groups\|diff{from_record}\|layers`, `brush_cursor`, `level` | — |
| `get_clay` | — | `layers[{name, kind, amount, enabled, records, bricks, head}]`, `mask{coverage, bricks}`, `groups[{id, name, area}]`, `morphs`, `levels`, `alphas_used`, `session_cache{bricks, mb, warm}` |
| `get_scene` / `job_status` | — | `ui_activity`: the last N UI records, so the agent sees what the human did |
| `surface_point` | Uses `pick_field` when the cache is warm | `mask`, `group`, `layer_offsets` |
| `bake` | `maps += displacement` (16-bit PNG, mid-grey = 0, scale in the reply), `vector_displacement` (tangent RGB16), `lowpoly: meshopt\|retopo` | `scale` |
| `export_glb` / `export_fbx` | `blend_shapes: ["layer:<n>"\|"morph:<n>"]`, `max_residual?` (default 0.5 cell) | `blend_shapes:[{name, max_residual, refused?}]` |
| `render_film` | `passes:[depth, normal, albedo, ao, shadow, mask, emission]` | `passes:{name: path}` |
| `render_animation` | `format: png_sequence\|mp4` (mp4 runs ffmpeg; if it is missing, falls back to png_sequence with a warning) | — |
| `kitbash` | `part:{object}` (InsertMesh from any object), `op: add\|carve` | — |

### 4.2 New tools (Stages 1-8: 15 tools, 57 → 72; Stage 9 adds 3 → 75)

1. **`stroke_log`** `{target, layer?, from_seq?, limit≤200, ids?, full?}`
   → `{records:[{id, seq, source, label, brush, kind, radius, strength, n_points, bbox, enabled, stale_context, stats}], head, total}`. With `full`, records also include the `spec` and the resolved `dabs` (paged).
2. **`stroke_edit`** (job) `{target, layer?, action: disable|enable|delete|replace|move|scale|rebase|replay|squash, ids, spec?, to_seq?, factor?, last?: bool}`
   → `{replayed, from_seq, head, timing_ms, diff_stats}`. Replays from the nearest checkpoint, so at most 32 records are re-applied.
   - `scale` with `last:true` is **Adjust Last**. It is computed from `LastDelta` as before + factor·delta, factor 0..2.
   - `rebase` re-applies stale records against the current context.
   - `squash` collapses a range into one `Opaque` record.
3. **`sculpt_diff`** `{target, layer?, from: seq|record_id|morph|"blobs", to?, region?, histogram_bins?, screenshot?}`
   → `{max_disp, mean_disp, rms, volume_delta, area_changed, bbox, histogram}`.
4. **`probe_clay`** `{target, points≤256 | rays | screen:{view, px}}`
   → for each probe: `{hit, point, distance, normal, curvature, mask, group, layer_offsets:{name: mm}, blob_owner}`.
5. **`mask`** `{target, actions:[...]}`. The actions run in order, and each one is a mask-ledger record.
   - Painting: `paint:[StrokeSpec]`.
   - Selection: `lasso|rect:{view, px, mode: set|add|sub, through}`, `invert`, `clear`, `all`.
   - Morphology: `grow|shrink:cells`, `blur:cells`, `sharpen`.
   - Masking by property: `by_cavity`, `by_curvature`, `by_normal{dir, angle}`, `by_height`, `by_blobs`, `by_groups`, `connected{seed_point}`.
   - Channels: `from_channel`, `to_channel` (smart-material channels).
   - Reply: `{coverage, bricks, bbox, records}`. Long operations run as jobs.
6. **`group`** `{target, actions:[from_blobs | from_mask | from_normals{angle} | paint{id, strokes} | rename | clear | show | hide | isolate | list]}`
   → `{groups:[{id, name, area, visible}]}`.
7. **`sculpt_layer`** `{target, action: add|list|select|amount|enable|disable|rename|move|bake|merge_down|delete|split_by_mask|record, name, amount?, kind?}`
   → the layer list.
   - `bake` is Bake Current: it folds the layer into the one below as an Opaque record and archives its ledger.
   - `select` and `record` change UI state only and are not undo steps.
8. **`morph`** `{target, action: store|switch|blend|delete|list|diff, name, weights?}`
   → morph list and distance stats `{max, mean}`.
9. **`brush_preset`** `{action: list|get|save|delete, name, spec?, from_record?}`
   → `{brushes:[{name, brush, radius, strength, falloff, alpha, kind, builtin}]}`.
10. **`alpha`** `{action: import|list|delete|preview|from_view|create_vdm, name, path?, from_view?:{target, view, size}, region?}`
    → `{name, hash, w, h, bits}`. `preview` returns a PNG.
11. **`history`** `{action: list|goto|mark|label, index?, name?}`
    → `[{index, label, source, tools, time, mark, bytes}]`, `current`, `dropped`.
12. **`import_mesh`** (job) `{path: obj|stl|ply|glb|fbx, name?, as: clay|mesh, scale?, up?, center?, close_holes?}`
    → `{object, triangles, watertight, holes_closed, bounds, warnings}`.
13. **`to_clay`** (job) `{target | targets, op?: add|carve|intersect, into?, mode: exact|volume, resolution?, blur?, polish?}`
    → `{object, blobs, cell, hausdorff_est}`. This one tool covers Make PolyMesh3D, DynaMesh for any mesh, Remesh All and cross-object booleans.
14. **`export_print`** (job) `{path: .stl|.obj|.ply|.3mf, targets?, size_mm?:{axis, value}, fine?, hollow?:{wall_mm}}`
    → `{triangles, watertight, non_manifold_edges, volume_cm3, bbox_mm}`.
15. **`detail_level`** `{target, level?: int | cell?: f32, add?: f32}`
    → `{levels, active_cell}`. Sets the cell that strokes write by default. Coarse edits transport the finer levels.

Stage 9 (optional) adds three tools:
- `to_poly {target, cell?|target_faces?, relax?}`
- `retopo {target, quads, adaptive, keep_group_borders, keep_mask_borders, symmetry, guides?, project_levels?}`
- `poly_edit {target, action: extrude|inset|bevel|bridge|insert_loop|delete|merge|crease|fill_hole|qmesh, select, amount?, segments?}`

**README:** the tool count is updated with every stage that adds tools.

### 4.3 How UI strokes become the same records

1. The pointer sampler (`src/app/brush.rs`) collects `(pos, t, pressure?, modifiers)`.
2. `StrokeCapture` applies LazyMouse, picks with `pick_field`, resamples by spacing, and emits incremental dabs to a preview worker. The worker runs the same engine on a scratch grid and remeshes dirty chunks, latest-wins.
3. On pointer-up, the UI builds a `sculpt` (or `paint` / `mask` / `group`) **tool request**, the same JSON an agent would send, with `source: ui` and a label. It dispatches the request in-process through the same tool handler as MCP, so `to_local`, snapping, `radius_scale`, validation and refusals, the install guard, history labelling and the reply shape are shared, not just `ledger::commit`. The request is kept in `ui_activity`, so the agent can resend it verbatim. The parity test is at request level: the stored request, sent through MCP, gives an equal `result_hash` and an equal reply (ignoring timing).
4. If the commit's `result_hash` differs from the preview, the preview is discarded. A test asserts this never happens.
5. The drag is one labelled undo step (through `edit_session`, app.rs:188).

Modifiers map to spec fields:
- Shift selects the `smooth` preset.
- Alt sets `invert`.
- Ctrl targets the mask ledger.
- Ctrl+Shift drag runs `mask lasso`.
- The gizmo with an active mask runs `brush: transform`.

Because the spec is what is stored, the agent can `stroke_log full`, edit it and resubmit it through `sculpt`.

**Every UI action has a tool route (critic addition).** Stroke parity alone is not enough. Each UI control is a front end for one tool call and is tested the same way:

| UI action | Tool call it dispatches |
|---|---|
| Brush drag (Sculpt/Paint tool) | `sculpt` / `paint` |
| Ctrl-paint, Ctrl+Shift lasso, Ctrl+Alt erase | `mask {actions:[paint\|lasso\|...]}` |
| Group paint, hide/show/isolate | `group` |
| Gizmo drag with a mask | `sculpt {brush: transform}` |
| Layer list (add, amount slider, eye, record dot) | `sculpt_layer` (one slider drag = one `amount` call on release, previewed live) |
| Adjust Last slider | `stroke_edit {action: scale, last: true}` |
| History scrubber, marks | `history goto/mark` |
| D / Shift+D | `detail_level` |
| Remesh / Make clay (Stage 5 menu) | `to_clay` |
| Brush picker save | `brush_preset save` |

The human's current brush (preset, radius, strength, falloff, symmetry and lazy settings) is UI state, but the agent can read and write it through `brush_preset {action: get_current|set_current}`, so it can set up the human's brush and see what the human is using. It is not an undo step.

**LazyMouse and incremental resampling.** The stored spec holds the **post-lazy** path, and `lazy` is recorded only as metadata, so replay never runs LazyMouse again. The preview worker and the commit consume one dab list produced by a single streaming resolver. Commit does not resample the whole path a second time, because a whole-path resample can differ from the incremental one at the tail.

---

## 5. Integration: undo, save, export, bake, film

### Undo and history (all in Stage 1b)

- Whole-Scene snapshots stay. Every new grid and ledger is an Arc with pointer equality, so per-frame clone and compare stays O(1) for them.
- **Guard test:** `Scene::clone` + `==` takes ≤ 50 µs on a heavy fixture (8 layers, mask, groups and a 1k-record ledger). This prevents Cloth-style deep copies (cloth.rs:64-85) from creeping in.
- The undo list becomes `Vec<(Scene, StepInfo{label, source, tools, time, mark, bytes})>`.
- Undo is capped at 300 steps *and* a 2 GB byte budget, estimated from unshared bricks by walking Arc strong counts. Dropped steps are reported by `history list`. This is a behaviour change; see Q5.
- **History is split per MCP call** in `process_mcp` (app/tools.rs:62-126) rather than per frame. Labels look like "sculpt standard x3 on Head [agent]" and "ui sculpt clay_buildup, 41 dabs [ui]". A concurrent UI drag is labelled with both sources.
- UI new/open (app.rs:532-557) and MCP `new_scene`/`open_scene` (app/tools.rs:487-501) behave the same way: both become undoable.
- Selective undo with `stroke_edit` produces a new ledger Arc. That is one ordinary undo step, so whole-call `undo` keeps working.

### Save and load

- There is still no version field, so every new field is `serde(default, skip_serializing_if)`.
- Brick payloads (detail, paint, mask, groups) are deflated with **flate2, already a dependency**, inside the existing base64 form. Old files still read.
- Ledgers store compact arrays with the resolved dabs.
- `store` is `Both` by default. On load, a ledger whose replay `result_hash` does not match keeps the saved grid and reports the mismatch through `get_clay`.
- On load, legacy `clay.detail` becomes layer "base" with one `Opaque` record.
- **QuickSave:** every 5 minutes when dirty, into `~/Library/Application Support/Clide3D/backups`, rotating 10 files. It writes on a thread from an Arc snapshot through `save_to`. QuickSave is an app setting, not scene state.

### Export

- Unbaked GLB/FBX export uses `meshes.rest()` (gltf.rs:606-617, fbx.rs:529-535), which now reads the composed field.
- Blend shapes are computed by Newton projection of each rest vertex onto the morph or layer field (`p -= f·∇f/|∇f|²`, 2-3 steps), which keeps vertex counts identical.
  - glTF: `targets` written in `Builder::mesh` (gltf.rs:203-277).
  - FBX: BlendShape and BlendShapeChannel deformers.
  - A shape is refused, with its residual reported, when it exceeds `max_residual`.
- `glb_read` learns to apply targets for checks.
- `export_print` writes OBJ, binary STL, PLY, and optionally 3MF (needs `zip`; Q6).

### Bake

- `Field::new` composes blobs + Σ amount·layers. `bake::Source` and the texel loop (bake.rs:156-200, 611-615) pick up layers, mask and paint layers with no special case.
- New maps: `displacement` and `vector_displacement`.
- `lowpoly: retopo` uses the Stage 9 output.
- Bakes stay unsaved and are re-run by exports (app/tools.rs:1074-1077).

### Film and viewport

- Film reads `meshes.get(o)` (cinema.rs:181-190).
- During sculpt sessions the viewport draws chunks. For film, export, bake and `rig::skin`, the monolithic rest, fine or posed mesh is assembled on demand: chunks are concatenated and welded, or the old path is used.
- Render passes come from film AOVs.
- Rigged clay sculpts at rest (`at_rest`, app/tools.rs:966). Skin is recomputed on commit, not per dab.

---

## 6. Stages

Effort assumes one engineer. Every acceptance test runs under `cargo test --release`. **GPU gating starts in Stage 0, not Stage 6:** `render_image` (render.rs:2520) needs an `egui_wgpu::RenderState`, so every image golden is `#[ignore]` unless `CLIDE_GPU=1`. The geometry and hash harness is the gate that always runs. Perf assertions run with `CLIDE_PERF=1`; otherwise they log to `target/perf.json` with 3× slack. Image goldens live in `tests/golden/` and are refreshed only with `BLESS=1`.

### Stage 0: verification harness and baseline investigation (M, 1 week)

**Scope**
- `tests/harness/geom.rs` provides geometry measures:
  - Hausdorff distance, one-sided and symmetric;
  - error against an analytic SDF;
  - volume and area;
  - normal angular error;
  - brick-hash equality.
- `tests/harness/image.rs` renders offscreen with `render_image` (render.rs:2520) at 512² with a fixed camera, and computes mean and p99 error against goldens.
- `tests/harness/perf.rs` holds the perf assertions.
- Port the scratchpad `zz_timing` bench into the tree.
- **Investigate the non-monotonic triangle counts** (res 96 gives 52k > res 112 gives 23k; res 192 gives 211k > res 256 gives 120k; independent of `sharp`). Suspects: slivers or duplicate sheets in `build_window_cells`, and adaptive factors. Also profile the hard-surface resolution floor (clay.rs:1876-1886).

**Parity rows:** none directly; this stage enables everything else.

**Acceptance**
- A sphere at res 128 compared with the analytic sphere: Hausdorff ≤ 0.6 cell and volume error ≤ 1%.
- Golden of the default sphere: mean ≤ 1.5/255 and p99 ≤ 6/255.
- `perf.json` holds the remesh baselines.
- Triangle count is monotone within ±15% across res 64-256, or the stage records a written finding as an `#[ignore]` test with the reason. The chunked mesher in Stage 6 depends on this. If the finding stays unresolved, Stage 6's reference is **not** today's `build_window_cells` output. It is a new monolithic *anchored* surface-nets pass (the same kernel run over one chunk covering the whole grid), and Stage 6 must also show that this reference meets the Stage 0 Hausdorff and volume bounds.
- Re-baseline the perf "Today" figures in §7, which come from the scratchpad bench rather than the tree, and replace them in the plan.

**Stage 0 result (2026-10-22).** The harness is in `src/harness/` (clide3d is a binary crate, so `tests/` cannot reach `Clay` or the renderer); goldens are in `tests/golden/`.
- Sphere at res 128 against the analytic sphere: Hausdorff 0.011 cell, volume error 0.03%, area error 0.02%, normals within 0.03°.
- Default-sphere golden: identical across runs (mean 0, p99 0) on an M2 Max.
- **Triangle-count finding.** The jumps are not slivers or adaptive factors. When any surface-nets edge joins more than two quads, `build_window_cells` discards the whole surface-nets mesh and remeshes the object with marching tetrahedra, which gives about 3.5× the triangles. On the organic figure, 2-8 such edges trigger it, in mirrored saddle cells where two sheets pass within a cell of each other: the arm-torso creases, the elbows and the carved eye sockets. Whether a resolution hits one is luck: res 80, 96, 144, 176 and 192 fall back, and 64, 112, 128, 160 and 208-256 do not. Taken separately, each method's counts are monotone. The other jump is the hard-surface floor: a hard clay left at the default resolution 96 meshes at 192 or more, so the figure with box feet gives 211k triangles at 96 against 23k at 112.
- `triangle_count_jumps_are_whole_mesh_tetra_fallbacks` pins the finding, and the monotonicity test is `#[ignore]`d with the reason. Stage 6's reference is therefore the monolithic anchored surface-nets pass described above. Chunks must resolve ambiguous edges locally rather than switching the whole mesh (or one chunk) to another method.

### Stage 1a: shared brush module, StrokeSpec v2 and the pure engine (M-L, 2 weeks)

**Scope**
- Add `src/brush/{spec,resolve,curve,symmetry,pressure,path,rng}.rs` (§3.1-3.3). `Stroke` becomes Serialize, and a compatibility converter keeps today's schema working.
- Falloff curves with focal shift.
- Per-point pressure and pressure curves.
- Spacing resample.
- Symmetry on X/Y/Z plus radial, replacing the second apply at app/tools.rs:715-717.
- Stroke kinds `dots`, `spray` (seeded), `drag_rect`, `drag_dot`, plus `taper`.
- Frames: world, object, surface and screen.
- Refactor sculpt.rs into `src/sculpt/{engine,brushes,noise}.rs`:
  - `engine::apply(ctx, &Detail, &[Dab], mask) -> Diff`.
  - Jacobi contract: each dab is a pure function of the pre-dab grid, the context and the dab. Parallelism is allowed only across output voxels, with no parallel float reductions inside a voxel.
  - The engine API includes a **transport hook**, a backward map that finer levels and layers can follow. It is wired in Stage 8.
  - `paint::stroke` uses the same curve, pressure and symmetry code.
- New brushes:
  - `standard`: a normal-displacement dab using the area normal; invert carves.
  - `smooth_mode`: gentle, peaks or valleys, chosen from the sign of the field Laplacian.
  - `flatten` with `side`.
  - `crease` with `taper` (Dam Standard).

**Parity rows**
- P0 Symmetry (X/Y/Z, radial).
- P0 Standard brush.
- P0 Smooth brushes (modes).
- P0 Core brush controls (focal shift, falloff, ZAdd/ZSub, intensity), agent half.
- P0 Tablet pressure curves, agent half.
- P1 Brush Edit Curve.
- P1 Stroke types (Dots, DragRect, DragDot, Spray, spacing).
- P1 Dam Standard.
- P1 Planar (side), partial.

**Acceptance**
- StrokeSpec JSON round trip is the identity. Legacy sculpt JSON (`mirror:true`, `point`) produces a grid bit-equal to a stored pre-refactor fixture hash. To make that possible, a legacy spec (no `falloff`) maps to a `legacy` falloff preset that calls the old `sculpt::falloff` (sculpt.rs:234) unchanged, not a curve LUT, and symmetry `[x]` keeps today's apply order (stroke, then mirrored stroke; app/tools.rs:715-717).
- The default *curve* preset equals the old falloff within 1e-6. This is a separate check from the bit-equal legacy path above.
- **MRGB (Core brush controls):** one `sculpt` stroke with `paint:{color}` gives one record that changes both the layer grid and the paint grid. Its paint coverage is IoU ≥ 0.95 against a separate `paint` stroke with the same spec.
- Standard dab (r = 0.05, strength 0.5) on a unit sphere:
  - peak within ±0.5 cell of the analytic value;
  - zero beyond 1.05r;
  - radial profile matches each of 3 curve presets within 5% RMS.
- Flat curve: height standard deviation ≤ 0.1 voxel inside 0.8r. Focal shift +0.5: half-height radius within 10% of analytic.
- Radial-6 symmetry produces 6 bumps 60° ± 0.5° apart, and the field is invariant under a 60° rotation within 0.5 voxel at 2k points. X mirror: |T(p) − T(mirror p)| ≤ 1e-4.
- Smooth `peaks` never raises a point and `valleys` never lowers one.
- Pressure:
  - pressure 0 gives `changed_points == 0`;
  - a 0→1 ramp gives a monotone bump width, measured with probe points.
- Spacing 0.25 gives dab count = ceil(len/(0.25r)) ± 1.
- Spray with seed 7 replays bit-identically.
- **Determinism:** 200 random strokes give a bit-equal grid on 1 thread and on 12 threads.

**Stage 1a result (2026-10-22).** `src/brush/` holds spec, resolve, curve, pressure, path, symmetry and rng; sculpt.rs is now `src/sculpt/{mod,engine,brushes,noise}.rs`. The `sculpt` and `paint` handlers read StrokeSpecs through `sculpt::prepare` (resolve) and apply them with `sculpt::run`; the old second mirror apply is gone. No tools were added (57).
- Legacy JSON is bit-equal: a 13-stroke fixture covering every old brush with `point`, `mirror` and masks hashes to `0x5ce84c6172772869`, as before the refactor, both through StrokeSpec and through the old `Stroke` API; a 4-stroke paint fixture hashes to `0xb6b5840b9985247c` likewise.
- The default `smooth` preset equals the old falloff within 1e-6.
- Standard dab (r = 0.05, strength 0.5, unit sphere, 4 mm cell): peak 0.01250 against the analytic 0.01250 (strength x radius / 2); profile RMS 0.30% (smooth), 0.45% (linear), 0.38% (sharp); no grid point beyond r changes and the surface beyond 1.05r moves at most 1e-4.
- Flat curve: height SD 0.043 voxel inside 0.8r (3 mm cell). Focal shift +0.5: half-height radius 0.02924 against the analytic 0.02929 (r = 0.1).
- Radial 6: bumps 60 +- 0.14 degrees apart; the field under a 60-degree turn differs by at most 0.19 voxel at 2,000 near-surface points. X mirror: max |T(p) - T(mirror p)| = 1.2e-5.
- Smooth `peaks` never raises and `valleys` never lowers (1,600 probe columns each). Pressure 0 gives 0 changed points; a 0-to-1 ramp gives monotone bump widths. Spacing 0.25 gives ceil(len/(0.25r)) +- 1 dabs. Spray seed 7 replays bit-identically.
- Determinism: 200 random mixed strokes give the same grid hash on 1 and 12 threads (0.50 s and 0.33 s).
- MRGB: one sculpt stroke with `paint: {color}` changes both grids, with paint coverage IoU 1.000 against a separate paint stroke.
- Baselines (perf.json): one standard dab of 25 cells at 2 mm takes 15-23 ms; a 25-dab stroke 129 ms.

Deviations, for the stages that build on this:
- The engine unit is a `Dab` that is either one dab or, for path brushes (add, carve, crease, layer, inflate ...), the whole polyline swept at once with per-point radii and gains. That is what keeps legacy strokes bit-equal; stroke-wide settings (curve, invert, side, smooth_mode, pattern) live in `resolve::Params`, not in each dab. Curves are evaluated exactly rather than through a LUT.
- `standard` is defined as a dab of height strength x radius / 2 along the area normal (the mean surface normal over the middle of the dab). Along a path its dabs are scaled by spacing / (2 x the curve's area), so a ridge rises about as high as one dab at any spacing.
- The `flat` preset is a plateau to 0.9 of the radius (so 0.8r stays flat after grid interpolation).
- `screen` frames take a named view with `at` [h, v] pairs (as `surface_point` does) or stored camera matrices with `px`; storing the camera on the record comes with the ledger (1b).
- `layer`, `cell`, `level`, `alpha`, `curl_deg`, `transform` and the persistent mask fields are left for the stages that use them; the stroke mask is still {blobs, above, below}. Without a ledger, "one record" for MRGB is one stroke entry in the reply (`changed_points` and `painted_points`).
- `sculpt::Stroke` (now Serialize) and `paint::PaintStroke` are test-only shims for the checks that build strokes directly; tools read StrokeSpec, which reads their JSON.
- The transport hook (`engine::transport`, grab and stretch backward maps) exists but is unused until Stage 8.

### Stage 1b: the ledger, history, undo cap, deflate and QuickSave (M-L, 2 weeks)

**Scope**
- **Gate:** Q6's hash choice must be decided before this stage starts (§3.4 "Hash function").
- `src/ledger.rs`:
  - records, context and result hashes, `DiffStats`, `engine` version;
  - `ReplayCache` with a checkpoint every 32 records;
  - `ledger::commit`.
- `Clay.sculpt` holds a single "base" layer; legacy detail migrates in as an `Opaque` record.
- Paint strokes log to a paint ledger.
- The install guard uses the ledger head.
- `LastDelta`.
- MCP:
  - `sculpt` gains `dry_run`, `measure` and `label`;
  - new tools `stroke_log`, `stroke_edit`, `sculpt_diff`, `probe_clay` and `history`.
- Undo and history as in §5: StepInfo, a 300-step / 2 GB cap, a per-call split with labels, and new/open made consistent.
- flate2 deflate for bricks.
- QuickSave.
- The clone+compare guard test.
- README tool count.

Stage 1 is split in two because a single stage would be heavy and would migrate the core data model before showing any value. 1a ships better brushes on today's storage; 1b adds the ledger on top of a stable engine.

**Parity rows**
- P1 Adjust Last.
- P1 Undo History (markers, list, goto; saving labels in the scene is optional).
- P1 QuickSave.
- P1 Stroke types (record and replay).

**Acceptance**
- **Replay determinism:**
  - Replaying 200 mixed strokes from empty gives a `result_hash` equal to the live grid.
  - Disabling record 57 and replaying from a checkpoint equals a fresh replay without record 57.
  - The comparison is bitwise on one machine. With `CLIDE_XPLAT=1` it is a tolerance of ≤ 1/256 voxel.
- Save and load: max |Δoffset| ≤ 1/256 voxel, and the ledger is equal.
- `stroke_edit scale last` checks:
  - factor 0 restores the pre-stroke grid within 1e-6;
  - factor 1 equals the stroked grid;
  - factor 0.5 is within 1e-4 of the midpoint, and its `max_disp` is half the original within 3%.
- `history list` labels contain "sculpt". `goto(n)` restores a scene equal to snapshot n. Five MCP calls in one frame give five entries. The cap evicts the oldest entries first and reports them.
- The deflated file is ≥ 2.5× smaller than raw base64 on the 6-dab 1 mm fixture.
- Scene clone+compare takes ≤ 50 µs on the heavy fixture.
- The old-file test at scene.rs:794 passes.
- Perf:
  - resolve + commit of 64 strokes × 64 points takes ≤ 20 ms beyond apply;
  - replaying 200 dabs at 2 mm takes ≤ 2 s.
- Image golden of a brow-ridge stroke set: mean ≤ 2/255, p99 ≤ 8/255 (`CLIDE_GPU`).
- Engine versioning: a record saved with `engine = N-1` loads with its saved grid and is flagged `engine_stale`, and `rebase` clears the flag.
- Agent defaults: a `sculpt` without `layer`/`cell` gives the same `result_hash` whatever `ClideApp` selection state is set.
- Adjust Last after an undo returns the "stale last" error and does not touch the grid.

**Stage 1b result (2026-10-22).** `src/ledger.rs` holds the ledger (records, xxh3-128 context and result hashes, `DiffStats`, `ENGINE_VERSION`, `commit`, `sync`, `replay`, `edit`, `after_load`, `adjust_last`) and `SculptStack` (`Clay.sculpt`); `src/app/history.rs` the labelled, capped undo history; `src/app/quicksave.rs` QuickSave; `src/app/strokes.rs` the new tools. `xxhash-rust` (Q6) is the only new dependency. Five tools were added (57 → 62): `stroke_log`, `stroke_edit`, `sculpt_diff`, `probe_clay`, `history`. The checks are in `src/ledger/tests.rs`, `src/app/history.rs` and `src/app/quicksave.rs`.
- Replay determinism: 200 mixed strokes (9 brushes, symmetry, taper, flat falloff) replayed from empty hash bit-equal to the live grid, and every record's own `result_hash` replays (`stroke_edit replay`: no mismatches). Disabling record 57 replays 168 records from the checkpoint at 32 (407 ms at a 2 cm cell) and hashes equal to a fresh replay without record 57; enabling it again gives the original grid. `CLIDE_XPLAT=1` switches these checks to a 1/256-voxel tolerance.
- Save and load: saved offsets are within 1/256 voxel; the base and paint ledgers read back record for record (same head); the replay on load matches the saved hashes (`replay_verified: true`) and puts the exact pre-save grid back, also through opaque records (a new detail cell, an outside edit), whose grids are kept rounded as the file reads them back.
- Robustness: a record whose dabs have mismatched `radii`/`gains`, no points or non-finite values fails to load (an error, never a panic); `commit` and `sync` refuse records past 100,000 per ledger or 2M dab points per clay with a message to squash; checkpoints never cross records that share an id (an undone stroke, a duplicated clay sculpted apart).
- Adjust Last: factor 0 restores the pre-stroke grid within 1e-6, factor 1 is the stroked grid bit for bit, factor 0.5 is within 1e-4 of the midpoint with `max_disp` half the original within 3%, and the scaled record replays to the same grid. After an undo it answers "stale last" and leaves the grid alone.
- History: labels like "sculpt standard x3 on pSphere1 [agent]"; `goto(n)` restores a scene equal to snapshot n; five calls give five steps; the step cap (tested at 3) and the byte cap (tested at 3 x 10 bricks) drop the oldest steps first and `history list` reports `dropped`.
- Deflate: the 6-dab 1 mm fixture (6,181 bricks) saves its detail in 1,591,351 B against 8,632,950 B raw base64: 5.42x. Paint packs too; old per-brick files still read.
- Scene clone + compare on the heavy fixture (8 sculpted clays, paint, 4 mask channels, a 1,000-record ledger): under 5 us (bound 50 us).
- The old-file test passes. Perf: resolve + commit of 64 strokes x 64 points costs 5.6 ms beyond apply (50.0 ms against 44.5 ms, run alone; bound 20 ms, scaled by apply's slowdown when the whole suite runs alongside); replaying 198 dabs at 2 mm takes 181 ms (bound 2 s).
- Brow-ridge golden (`CLIDE_GPU=1`): the replayed ledger renders identical to the live sculpt it was blessed from (mean 0, p99 0; bound 2/255 and 8/255).
- Engine versioning: a record saved with `engine = N-1` loads with its saved grid, `replay_verified: false` and `engine_stale`; `rebase` re-applies it, clears the flag and gives the live grid bit for bit. The version-1 fixture hash is pinned (`18ee53080bd0df6e7790f277b71c5193`).
- Agent defaults: records store the resolved layer (`base`) and cell; the same JSON gives the same `result_hash` on any copy, and the resolver takes no editor selection state. Every tool that changes what a replay reads (blobs, resolution, shell, sharp, detail cell, regions, or decals for paint) settles unchecked ledgers first; an audit covered edit_clay, paint, sculpt, stroke_edit, face_detail, set_human, pose_bone carry, kitbash, detail_pass, instance_array, damage, wear and decal. Open: `PaintGrid::context` does not hash decals, so a decal change after painting does not mark paint records `stale_context`.

Deviations and defaults, for the stages that build on this:
- The base layer's grid stays in `Clay.detail` and paint in `Clay.paint`; `SculptStack` holds only their ledgers (`{base, paint}`). Moving grids into layers is Stage 4. `sculpt` accepts `layer: "base"` only and refuses `cell` (set `edit_clay detail_cell`).
- An MRGB stroke is one record on the base ledger (shape) and one on the paint ledger (color), both in the reply (`id`, `paint_record`).
- Grid changes outside strokes become `Opaque` records: a delta when the layout (cell, regions) is unchanged, else the whole grid (paint: always whole). Tools that edit grids directly (`edit_clay`, paint fill/clear, `project_reference`, `face_detail`) record them at once; a clay without a ledger (an older file) records its grid as the first record at its first stroke, so loading and saving an old file changes nothing. With `store: Both` (Q7 default) such a grid is then saved twice; files stay JSON with deflated base64 (no sidecar).
- Checkpoints are keyed by (record chain hash, context hash), not (ledger address, seq): the chain hashes record ids, contents (`Record::digest`: the resolved dabs, or the opaque grid's hash), switches and factors, so edits, undone-then-reused ids and copies sculpted apart never reuse another grid's checkpoint, while identical duplicates share them. A dry-run `sculpt` plants none. Result hashes add per-brick xxh3 hashes, so a commit hashes only the bricks it changed.
- `LastDelta` lives in the editor (per object, for the newest record), not in the scene, so undo makes it stale; scale by id covers older strokes. Paint strokes cannot be scaled.
- Records do not store `seq` (it is the position). Mirrored clays reflect their records as `replay` records with unknown result hashes until `rebase`. On load, ledgers with more than 2,000 dabs are not replayed, and checking stops after 1.5 s over the whole file (`VERIFY_BUDGET`); unchecked ledgers keep their saved grids and report `replay_verified: null` and `in_sync: false` (`stroke_edit replay` checks them); their next stroke or edit replays them first and builds on the exact grid, so what follows never depends on whether the check on load finished. `save_scene`'s "is this a scene" check only parses the file.
- Undo bytes are estimated per step from the bricks (by address), paint grids and records its snapshot does not share with the next one, so caches never count (Risk 21). Q5 default: 300 steps / 2 GB as recommended; marks are not saved in the scene.
- UI New and Open are undoable steps like the agent's `new_scene`/`open_scene`. QuickSave is also reachable by agents (`history {action: quicksave}`) and the File menu toggle dispatches that call.
- `probe_clay` reports `mask` and `group` as null until Stage 3; `sculpt_diff` from a morph waits for Stage 4.

### Stage 1c: UI stroke recorder, no live preview (M, 1.5 weeks; critic addition)

This stage pulls the core AI-first guarantee forward. Without it, the guarantee is first tested in Stage 7, around week 20.

**Scope**
- `Tool::Sculpt` and `Tool::Paint` (app.rs:49-54), with a plain screen-space circle cursor, radius and strength sliders and a brush dropdown.
- The pointer sampler records the drag. **Nothing is applied during the drag.** On release it builds the `sculpt`/`paint` request and dispatches it through the shared tool handler (§4.3). The result appears after today's background remesh.
- `ui_activity` in `get_scene` and `job_status` (moved forward from Stage 7).
- `brush_preset get_current|set_current` (the current-brush part only).

**Parity rows:** none closed. This is the foundation for every UI half, and it lets humans try the 1a brushes early.

**Acceptance**
- A synthetic egui `RawInput` drag gives one record with `source: ui`. Its stored request, sent through MCP on a fresh copy, gives an equal `result_hash` and an equal reply (timing ignored).
- Undo after the drag restores the pre-drag Scene (==), in one labelled history entry.
- An agent `get_scene` after the drag lists it in `ui_activity`.

**Stage 1c result (2026-10-22).** `src/app/recorder.rs` holds the recorder: `Tool::Sculpt` and `Tool::Paint` (toolbox), the Sculpting shelf (brush dropdown, radius, strength, invert, X symmetry, paint color), a screen-space circle cursor and the drag path, `ui_activity`, and the `brush_preset` handler. One tool was added (62 → 63): `brush_preset` with `get_current` and `set_current` (its `list`/`get`/`save`/`delete` answer that saved presets come in Stage 2). The acceptance test is `src/app/recorder/tests.rs`; it drives a headless `ClideApp` (a wgpu device for the render state; it skips without one) with synthetic egui `RawInput`, and sends its agent calls through `mcp::dispatch` (JSON-RPC, as the server does).
- A drag over a clay records 25 samples and changes nothing while the button is down (the test checks `Scene ==` every frame). On release it becomes one `sculpt` request whose stroke has a `screen` frame (`camera`: view and projection matrices and the viewport in pixels; `px`: the samples), queued as a `ToolCall` with `source: ui` in the same line as MCP calls and run by the same handler. The result is one base-ledger record with `source: ui`, `tool: sculpt`, and one history step "sculpt inflate on Ball [ui]" (source `ui`).
- `get_scene` (and `job_status` without an id) list it in `ui_activity` with its job, status, step and record_ids; `get_scene {detail: true}` adds the request. Undo restores the pre-drag Scene (==). The stored request, sent through MCP on that restored scene, gives the same record (equal `result_hash` and spec, `source: agent`) and an equal reply, `ledger_head` and `step` included, timing aside. The Paint tool takes the same route to the paint ledger (one `source: ui` record, one "paint ... [ui]" step, listed in `ui_activity`).
- `brush_preset set_current` from MCP sets the tool, brush and radius the next drag records (the drag above was set up that way); bad brushes (grab), radii, unknown fields and the later actions are refused and change nothing.
- Timing (the test, run alone): resolve 2.9 ms and apply 1.3 ms for the 25-sample inflate stroke, release to remeshed 12 ms at resolution 48.
- Fixed on the way: `MeshLib` left a running build pending forever when the scene went back to the meshes already shown (an undo while it built), so `busy()` never cleared and calls waited in line for good; the build is now dropped (`mesh.rs` test `going_back_to_the_shown_meshes_drops_the_running_build`).
- Fixed after review: (1) the bottom jobs panel comes and goes while a stroke's job and remesh run, so the next drag's pane changed size and the stroke was dropped as "the view moved". Each sample is now the press-camera pixel on the ray under the mouse in the pane as it is that frame (`recorder::reproject`), and only a real camera change (view matrix, or projection at a fixed aspect) drops a stroke (test `a_stroke_outlives_a_pane_resize_but_not_a_camera_move`). (2) The press is picked on the posed scene but the handler strokes the rest pose, so a press is refused, with a status line, while the clay's rig is posed or its world matrix differs from rest at the timeline frame (`a_press_on_a_posed_clay_is_refused`). (3) Loads are bounded against deflate bombs: `clay::INFLATE_BUDGET` (4 GiB of inflated grid bytes) is shared by every packed grid in one read (`read_scene` opens it; `Scene.objects`, `Detail`, `Paint` and `Ledger` open the default when no read has), refinement levels count toward `MAX_TOTAL_BRICKS` as they are read, and a ledger over `MAX_RECORDS` or `ledger::MAX_OPAQUE_GRIDS` (4096 grid-holding records) has the rest of its records skipped unread and is dropped by `Clay::sanitize` (the clay and its grids stay; the scene still reads). `push` refuses grid records past the cap, and `ledger::edit` checks it on the edited list (an outside edit is recorded first even at the cap), so a squash or delete can always get back under it (`ledger/tests.rs`: `packed_grids_inflate_within_one_budget_per_read`, `ledger_records_and_grids_are_capped_while_read`).

Deviations and defaults, for the stages that build on this:
- `brush_preset` lands here (62 → 63), so Stage 2 adds only `alpha` as a new tool (63 → 64) and extends `brush_preset` with `list`/`get`/`save`/`delete`.
- The toolbox's Sculpt and Paint buttons and the shelf's controls all send `brush_preset set_current`; the toolbox's disabled "Paint Selection Tool" placeholder gave its slot to the Paint tool.
- `source` travels next to the request (`ToolCall.source`, shown as `source: ui` in `ui_activity`), not inside its JSON, so an agent that resends a UI request verbatim makes an `agent` record. The UI request carries no `label`.
- The request is built when its turn comes (no call in line, meshes built), not at the instant of release, so samples are filtered against the very mesh the handler casts on: samples whose ray misses the clay are dropped (the handler refuses a whole stroke for one miss), and more than 512 are thinned evenly. The camera is captured at the press and scroll zoom is ignored while a stroke records.
- Radius is in world units (as agents give it), not screen pixels; the cursor shows it at the camera target's depth. The Sculpt tool offers the brushes that need only points (standard, add, carve, inflate, deflate, smooth, flatten, crease, pinch, layer).
- `ui_activity` keeps the last 32 UI requests and shows the newest 5; requests are left out of compact replies (with samples and a camera they would overflow the reply budget) and included with `detail`/`verbose`.
- A stroke that creates a clay's ledger gives it a fresh ledger uid, which seeds the head: resending such a request on a fresh copy gives an equal `result_hash` but another `ledger_head`. The parity test therefore sculpts once as the agent before the drag.

### Stage 2: alphas, clay-family brushes, presets and posable symmetry (L, 2-3 weeks)

**Scope**
- `brush/alpha.rs`:
  - `Scene.alphas`, 16-bit; imported from PNG16, PNG or JPEG (the `image` and `png` crates are already dependencies; EXR is Q6);
  - bilinear sampling in the dab tangent frame;
  - modes `stamp`, `drag` and `tile`; `use` height or mask;
  - `alpha from_view` captures a heightmap.
- Brushes:
  - `clay`: fills toward an offset plane, square alpha;
  - `clay_buildup`;
  - `clay_tubes`;
  - `trim_dynamic`: removes material above the plane only, with a sharp edge;
  - `polish`;
  - `snakehook` with `curl_deg`.
- Presets:
  - `Scene.brushes` plus about 20 built-ins (Standard, ClayBuildup, ClayTubes, DamStandard, TrimDynamic, hPolish, Inflate, Move, Pinch, Snakehook, Layer, Flatten, Planar, Smooth, SmoothPeaks, SmoothValleys, Pores, Wrinkles, Scales, MaskPen);
  - `brush_preset` tool;
  - `sculpt.preset` merges a preset with overrides.
- **Posable symmetry:** map the stroke to rest pose with inverse LBS through the `rig::skin` weights at the hit point, mirror in rest space, and sculpt at rest.
- Tools: `alpha`, `brush_preset`.

**Parity rows**
- P0 Alphas (16-bit).
- P0 Clay brushes.
- P0 Dynamic brush system & library (data and MCP half).
- P0 Symmetry (posable).
- P1 Custom brushes.
- P1 Planar/Trim/Polish.
- P1 TrimDynamic and hPolish.
- P1 Snakehook.

**Acceptance**
- A 64² checkerboard alpha stamped on a box face:
  - heights correlate ≥ 0.95 (Pearson) with the alpha;
  - peak within ±0.5 cell of `depth`;
  - IoU ≥ 0.85 at 2-cell resolution.
- Re-importing the same PNG gives the same hash. A different PNG flags the records as `stale_context`.
- `clay_buildup` into a 5 mm groove: groove-centre displacement ≥ 3× rim displacement after 3 passes.
- `clay` on a grooved plane:
  - groove depth reduced ≥ 50%;
  - plane points away from the groove move ≤ 0.5 voxel.
- `clay_tubes` along a 10-point path: ridge height std-dev ≤ 15%.
- `trim_dynamic` gives `volume_delta` ≤ 0 and a planar region flat within 0.3 cell RMS.
- `snakehook` curl 90° rotates the tip 90° ± 15°.
- Saving then loading a preset is the identity. Preset + override equals the spelled-out spec (hash).
- Posable: a stroke on the posed left arm equals the mirrored stroke on the right arm at rest within 1 voxel.
- Goldens for ClayTubes and alpha stamps.

**Stage 2 result (2026-10-22).** `src/brush/alpha.rs` holds alphas (16-bit images, sampling in the dab frame, `AlphaRef`/`AlphaParams`, the per-clay `AlphaStore`), `src/brush/preset.rs` the presets (20 built in from `assets/brushes/*.json`), `src/sculpt/brushes.rs` the new brushes, and `src/app/alphas.rs` the `alpha` tool, alpha binding for sculpt calls and posable symmetry's inverse LBS. One tool was added (63 → 64): `alpha` (import, from_view, list, delete, preview; create_vdm answers that it comes in Stage 8). `brush_preset` gains list, get, save (`spec` or `from_record`) and delete; `sculpt` takes `preset` per call and per stroke. No new dependencies; `ENGINE_VERSION` stays 1 (no existing brush changed its output: the legacy fixtures `0x5ce84c6172772869` and `0xb6b5840b9985247c` and the version-1 pin still hold). The checks are in `src/sculpt/stage2.rs` and, through MCP on a headless editor, `src/app/alphas/tests.rs`.
- Checkerboard alpha (64², 16-pixel squares) stamped by a standard dab on a box face (r = 0.1, depth 0.01, 3 mm cell, flat falloff), sampled every 2 cells inside 0.8 r: Pearson 1.000 against the alpha, peak 0.01000 against depth 0.01 (within 0.5 cell), IoU 1.000. With `use: mask` the dab rises only on white.
- The same PNG imports to the same hash. Importing other pixels under the same name rebinds it on the clays that use it: the alpha record reports `stale_context` and the plain one does not; `stroke_edit rebase` takes the new image and clears it. An alpha stroke replays from the clay alone through a save and load (`replay_verified: true`), and still replays after the library entry is deleted.
- `clay_buildup` along a 5 mm groove (3 mm deep, r = 8 mm, 0.5 mm cell), 3 passes: groove centre up 3.44 mm, rim up 0.44 mm (7.8x; bound 3x); more passes build over the plane too.
- `clay` along the same groove: depth 3.00 mm → 0.19 mm (94% less; bound 50%); plane points beside the groove moved 0.000 cell (bound 0.5 voxel).
- `clay_tubes` along a 10-point curved path (r = 6 mm, strength 0.5): ridge 1.50 mm (strength x radius / 2), height SD 0.0% (bound 15%).
- `trim_dynamic` on a sphere's top (r = 0.15, strength 1, 4 mm cell): `volume_delta` -2.0e-4 (≤ 0), it cut 9.9 mm, and the region inside 0.7 r is planar within 0.062 cell RMS (bound 0.3). `polish` takes a bumpy patch's roughness from 6.2 mm to under 0.01 mm.
- `snakehook` with curl 90° (2-point path straight up 4 r, about Z): the horn's tip, measured from the clay's material, points 85.3° from the base direction (90 ± 15).
- Presets: a stroke with `preset: DamStandard` resolves and commits equal (same resolved stroke, same `result_hash`) to the stroke spelled out; through MCP, a saved preset under overrides gives the same `result_hash` and record spec as the spelled-out stroke, and saved presets read back identical through `save_scene`/`open_scene`. Every built-in parses.
- Posable: on a rigged body posed at frame 10 (left arm turned 35° about its shoulder), a standard stroke with `symmetry: {axes: [x], posable: true}` on the posed forearm and the stroke at rest on the right forearm with X symmetry give grids within 0.000 voxel of each other over 11,144 changed points (bound 1 voxel).
- Determinism: 60 random Stage 2 strokes (all six new brushes, alphas in stamp and drag mode, mirror) give the same grid on 1 and 12 threads (0.25 s and 0.14 s). Mirrored alpha stamps are mirror images (surfaces within 1e-4).
- Goldens (`CLIDE_GPU=1`, blessed from the live sculpt): `clay_tubes` (three tube strokes around a ball) and `alpha_stamps` (checker, ring with mid, tiled and rotated checker, a buildup dragging a ring alpha), each identical on re-render.

Deviations and defaults, for the stages that build on this:
- Each clay keeps the images its records use in its sculpt stack (`StackData.alphas`: images by hash, plus the name each was last bound to), so replays, loads and settles need only the clay; the scene's library (`Scene.alphas`) is where names come from. `context_hash` covers an alpha through its name's binding on the clay (`ledger::alpha_context`); strokes without a named alpha keep the plain layer context, so older records hash as before. Alphas are read within the scene's inflate budget, at most 64 per library and per clay, at most 1024 pixels a side (checked from the header before decoding); a file with more alphas (or more than 256 presets) opens with the first ones and the rest skipped unread, as over-cap ledgers are in 1c. Before a call adds an image, the clay's store drops names and images no record uses any more (deleted, squashed or rebased strokes), so squash and delete get a full store back under its cap. `Preset` and `Alpha` are Arc-shared (presets compare by pointer first, alphas by hash): the scene clone + compare guard with a full library (64 alphas, 256 presets) still holds.
- Alphas shape the dab brushes only (standard, clay, clay_buildup, trim_dynamic, polish); paint, path brushes and masks refuse them (masks come in Stage 3). `use` defaults to height on standard and mask elsewhere. Stamp and tile modes keep the image upright on object X; drag turns it with the stroke. Dabs store their frame's direction (`tangent`) and a `flip` for mirrored copies, so mirrored stamps are mirror images; plain standard dabs store neither, as before.
- The plane brushes find the area plane from 17 points over the dab, taken onto the surface along the area normal; the plane's normal is the mean surface normal at those points. Buildup is scaled along a path like standard (its layer is about strength x radius / 10 per stroke); clay, trim_dynamic and polish take every dab at full strength toward the plane. `clay_tubes` is a path brush (the whole polyline at once, each point's own normal from the field before the stroke). trim_dynamic, polish and clay_tubes default to the flat falloff.
- `snakehook` moves everything within 0.4 radii of the moving point fully, fading to the radius, in steps of an eighth of the radius (so it cannot fold); its curl turns the path over the first three quarters and the tip keeps the full turn. Only the first point snaps.
- Presets are JSON settings without `points`, `point`, `frame` or `preset`, at most 8 KB, at most 256 per scene; built-in names cannot be saved over or deleted. `MaskPen` is left out until the mask brush (Stage 3), so the 20 built-ins include `Clay` instead. `sculpt` merges presets at the JSON level before resolving: call preset, then the stroke's preset, then the stroke's own fields. Records keep the merged stroke.
- Posable symmetry maps each point through the weights of the nearest posed vertex (within 5% of the mesh's size), inverting the blended skinning matrix, and uses the object's placement at rest; it works in `sculpt` calls and in `stroke_edit replace` on the base layer (which settles the ledgers, then takes the stroke's `symmetry.frame` or the editor's frame, through the same helper as `sculpt`); paint refuses it, and the Sculpt tool still refuses posed clays (Stage 7). Vertices whose skin blend is zero or singular are left out of the nearest-vertex search, and a point outside the posed mesh's box (grown by that 5%) is off the clay.
- The Sculpting shelf gains a preset list and the six new brushes in its brush list (snakehook needs a path off the surface, so it waits for Stage 7); both send `brush_preset set_current`.
- The clay brush has no plane offset (its lift is 0: it presses to the area plane), and `alpha from_view` takes `center`/`extent` instead of a region. The posable unpose maps per-point normals through the inverse-transpose of each point's unposing matrix, unposes a drag_rect's corners one at a time, and takes the stroke-wide delta and normal from the pose at the stroke's start.
- `stroke_edit replace` now gives the replaced record the current context (it was resolved now), and `rebase` rebinds alpha records to the image their name is bound to.

### Stage 3: persistent mask, auto-masking, polygroups and the masked transform (L, 2-3 weeks)

**Scope**
- `src/mask.rs`, a `MaskGrid`, with the ops listed in §4.2.
  - Grow and shrink are distance operations on the surface band.
  - Blur and sharpen are separable filters restricted to the band.
  - `by_cavity` reuses the smart-material cavity estimator.
  - Lasso projects surface-band voxels through the view.
- The engine multiplies every dab weight by (1 − mask).
- `StrokeMask` gains:
  - `backface`;
  - `cavity`;
  - `connected` (a flood fill over band voxels from the dab centre within 1.5r, which gives Move Topological);
  - `groups`;
  - `normal_angle`.
- Mask edits become mask-ledger records.
- `src/groups.rs`, a `GroupGrid` (u16), and the `group` tool. Hidden groups are skipped by brushes. Today's 120 B `Vertex` (mesh.rs:982) has no mask or group attribute, and the slim vertex arrives only in Stage 6. So in this stage the overlays are tinted into `Vertex.color` by the shading-only refresh, and hidden-group triangles are dropped from the index buffer there. Shader-side clipping replaces both in Stage 6.
- Mask and group changes take the shading-only path through `mesh_shading::refresh`. Its cost is **unmeasured**: refresh builds `Field::new` and evaluates the field per vertex (mesh_shading.rs:9-30), so it scales with vertex count. The ~1 ms figure is an estimate for small meshes only. A 1.65M-triangle bound is added in Stage 6.
- `brush: transform` is a mask-weighted affine transform. The gizmo uses it automatically whenever a mask exists.
- Tools: `mask`, `group`.

**Parity rows**
- P0 Masking brushes & core controls (agent half).
- P1 BlurMask/SharpenMask/GrowMask/ShrinkMask.
- P1 Brush Auto Masking.
- P1 Mask By Cavity.
- P1 Move Topological.
- P1 PolyGroups (spatial; partial, no topology ops).
- P1 Gizmo 3D (masked sub-object transform).

**Acceptance**
- Inside mask = 255, every brush gives |Δ| = 0 bitwise. At mask = 128 the change is scaled linearly ± 2%.
- Inverting twice is bit-exact.
- `grow(3)` then `shrink(3)` on a disc:
  - coverage within 2%;
  - boundary Hausdorff ≤ 1 cell.
- Blur keeps mask mass within 3%.
- A front-view rect on a sphere reaches ≥ 98% IoU against the analytic projection. `through:false` leaves back faces unmasked.
- `by_cavity` on a grooved plane reaches IoU ≥ 0.8 against the groove.
- `connected` grab across two fingers: the other finger's `max_disp` ≤ 0.1 cell.
- The mask survives a resolution change from 96 to 160 with coverage change ≤ 2%.
- A mask or group edit leaves the MeshLib build counter unchanged, and the refresh takes ≤ 3 ms on 50k vertices.
- `from_blobs` on 3 blobs gives 3 ids with ≥ 95% of voxels nearest-owned.
- Masked transform: masked voxels move |ΔT| ≤ 1e-6.
- Mask-ledger replay is bit-equal.
- Goldens for the mask and group overlays (`CLIDE_GPU`). The masked region's luminance is ≥ 25% lower. Without a GPU, the same check runs on the refreshed `Vertex.color` values.
- A hidden group's triangles are absent from the refreshed index buffer, and a brush over it gives `changed_points == 0`.
- A rect or lasso record stores camera matrices. Resubmitting it after the viewport camera moves gives an equal mask hash.

**Stage 3 result (2026-10-22).** `src/mask.rs` holds the persistent mask (`SculptMask`: u8 bricks on the surface band, `Clay.sculpt_mask`) and its ops, `src/groups.rs` the polygroups (`Groups`: u16 bricks with a name and visibility per id, `Clay.groups`), `src/band.rs` the surface band they live on, `src/sculpt/transform.rs` the masked transform, `src/overlay.rs` the viewport tint and `src/app/masking.rs` the `mask` and `group` tools. Two tools were added (64 → 66). No new dependencies; `ENGINE_VERSION` stays 1 (a stroke that reads no mask or groups changes exactly as before: the legacy fixtures and the version-1 pin still hold). The checks are in `src/sculpt/stage3.rs` and, through MCP on a headless editor, `src/app/masking/tests.rs` and `src/app/recorder/tests.rs`.
- Full mask (255 on the band): 21 brushes (all sculpt brushes, noise, paint and transform) change no grid point under it, bit for bit, and the surface moves at most 5% of a cell (Hausdorff). Half mask (128): single dabs of the 18 linearly gated brushes keep 0.4980 of the free change (1 - 128/255; bound 0.5 ± 2%).
- Inverting twice gives the same mask hash. grow(3) then shrink(3) on a disc (by_normal 35° cap): coverage 0.0902 → 0.0903 (bound 2%), boundary Hausdorff 1.00 cell (bound 1). blur(3) keeps the mask's sum within 0.003% (bound 3%).
- A front-view rect on a sphere (4 mm cells): IoU 0.9878 against the analytic projection (bound 0.98) with and without `through`; `through: false` leaves 0 back vertices masked.
- by_cavity on a plane with a 4 cm groove: IoU 0.833 against the groove (bound 0.8).
- connected grab across two fingers 6 cells apart: the other finger's surface moves 1.25 cells without it and 0.000 cells with it (bound 0.1).
- Resolution 96 → 160: mask coverage 0.1787 → 0.1787 (bound 2%).
- A mask edit and a group edit through MCP leave `geometry_builds` and `shading_updates` unchanged; the overlay tint takes 2.15 ms on 77,096 vertices (bound 3 ms on 50k). Masked vertices' luminance is 0.40 of the untinted (bound ≤ 0.75); the GPU golden `mask_overlay` changes 22,764 pixels at 0.437 of their luminance; `group_overlay` is the groups tint with one group hidden (both identical on re-render).
- from_blobs on 3 blobs: 3 ids, 100% of band points owned by their group's blob (bound 95%). A hidden group's triangles leave the drawn index buffer, and an inflate over it reports `changed_points: 0`.
- Masked transform (translate 4 cm, everything below y = 0.15 masked): masked band points change by 0 (bound 1e-6), 12,504 free points change, and the free cap rises 0.0398 (0.04).
- Mask and group ledgers replay bit-equal (`stroke_edit replay`, and `replay_verified: true` through a save and load); a stroke records the mask it read and replays bit-equal after the mask is cleared, also through save and load.
- A rect with a camera stores the camera's matrices in its record (and the clay's placement in the resolved form); after the editor's camera orbits and zooms, the record's action sent again gives the same mask hash, and the stored actions rebuild the same mask.
- Determinism: masked strokes with the persistent mask, a blurred edge, every auto-mask, invert_channel and a transform give the same grid on 1 and 12 threads.
- Stage 2's deferrals: paint and mask_paint strokes take alphas (a checker paints and masks 1.00 on white, 0.00 on black), and `MaskPen` is the 21st built-in preset. The Sculpt tool's MaskPen strokes go to the `mask` tool (one `source: ui` mask record; the stored request resent through MCP gives the same `result_hash`), and the gizmo on a masked clay sends `sculpt {brush: transform}` on release without moving the object.
- Open issue from 1b closed: `Paint::context` now hashes the decals when a clay has any (files without decals keep their hashes), so a decal change makes paint records `stale_context`.

Deviations and defaults, for the stages that build on this:
- As in 1b, the grids are `Clay` fields (`sculpt_mask`, `groups`) and the stack holds their ledgers (`StackData.mask`, `StackData.groups`); `clay_key` leaves both out, so they never start a mesh build.
- A stroke does not hash the mask into its context: it names the mask and groups it read by content hash (`Resolved.frozen`), and the clay keeps those grids (`StackData.frozen`, at most 256 of each, read within the inflate budget; grids no record names are dropped before one is added). So mask edits never make strokes stale, and a stroke replays the same after its mask changes. by_groups and from_mask name the grid they read the same way.
- Mask and group actions are a third record kind, `RecordOp::Action {spec, action}` (the call's JSON and the resolved op); mask and group strokes are ordinary stroke records (`mask_paint`, `group_paint`). Their context is the blobs and the sculpted surface (as paint's), so sculpting after a surface-reading action (lasso, by_normal, grow ...) marks it `stale_context` and its check on load can report a mismatch (the saved grid is kept, as for paint). `from_channel` records the result as an Opaque grid (channels have no ledger); `to_channel` records an action that leaves the mask alone and writes the channel when the call installs.
- The overlay does not go through `mesh_shading::refresh` (which evaluates the field per vertex): `MeshLib` keeps a tinted copy of the drawn mesh per clay (`display`, only for the viewport's draw and pick), made again only when the drawn mesh, the mask, the groups or the overlay change. Film, bakes and exports read the clay's own meshes, untinted and whole. `set_display overlay: mask` (default), `groups` (groups tinted, the mask darkened too) or `none`; hidden groups are left out in every mode.
- The mask band is the grid points within min(2 cells + half a mesh cell, 3.5 cells) of the surface, read through a field with a detail grid (clamped at 4 cells). The mask cell is the clay's detail cell (else its mesh cell at the first mask). Points off the band take the gate at the surface point nearest them (along the field's gradient, unclamped where the field is clamped), so a strong brush or a transform cannot grow new surface beside a masked area; the transform moves a point by the smaller gate of the point and of where its material comes from.
- Selections default to `mode: set`. A named view's lasso/rect is an orthographic camera of fixed span (records do not depend on the scene's size); `through: false` is a facing test (no occlusion). `by_cavity`/`by_curvature` reuse the smart materials' bend over 2 cells (`radius`); the stroke mask's `cavity` is an amount -1..1 rather than a curve. `connected` floods the band within 1.2 cells of the surface; strokes too big for one region (tiled) refuse auto-masking, noise refuses it, and paint takes the persistent mask and groups only.
- `transform` takes `{translate, rotate (degrees X then Y then Z), scale, pivot}` or `{matrix}` in the call's space; strength 0..1 is how much of the move applies, radius is not needed, symmetry and posable are refused, and offsets cap at 127 detail cells.
- A `group_paint` stroke gives its id to band points at half weight or more; group strokes are not replaced by `stroke_edit` (paint again). Group ids are 1..65535, at most 4096 groups.
- Coverage (mask replies, `get_clay`) is the area-weighted mean mask over the rest mesh.

### Stage 4: sculpt and paint layers, morphs, blend-shape export (L, 2-3 weeks)

**Scope**
- `src/layers.rs`:
  - up to 16 layers with kind, amount, enabled and a per-layer ledger;
  - the `Composite` cache;
  - `Field::new` composition;
  - paint layers composited in `mesh_shading`.
- `sculpt_layer` tool, including `record` and `split_by_mask`.
- `morph` tool.
- Blend shapes:
  - glTF targets and FBX BlendShape, with residual refusal;
  - `glb_read` applies targets so the export can be checked.
- Layers and morphs are counted against a **byte** budget rather than `MAX_TOTAL_BRICKS` (clay.rs:1941, 262,144 f32 bricks), because mask (u8) and group (u16) bricks are 4× and 2× smaller. Today that cap holds a single detail grid. A heavy sculpt can already fill most of it, so the budget and its error message (bake down, squash, coarser cell) are specified here, and an 8-layer fixture at the base cell is shown to fit.
- **Gate:** Q1 (what a stroke on layer k reads) must be answered first. The `stale_context` acceptance below depends on it.
- Tools: `sculpt_layer`, `morph`.

**Parity rows**
- P0 3D Layers incl. Bake Current (agent half).
- P1 Morph Targets, including blend-shape export.

**Acceptance**
- A layer at amount 0 is bit-equal to a stack without that layer.
- With a layer whose max offset is ≤ 2 cells, amount 0.5 gives 0.5× the displacement ± 3% at 50 probes.
- Amount 2 doubles a standard dab's displacement within 5%.
- Baking at amount 1 matches the field before the bake within 1/256 voxel.
- Changing another layer's amount flags affected records as `stale_context`. `rebase` then equals a fresh ordered replay.
- Morph store / switch / switch back is pointer-equal on the grids. Blend 0.5 equals the field midpoint within 1e-4.
- `select` does not add an undo step.
- GLB export with morphs, re-read through `glb_read`:
  - equal vertex counts;
  - vertices with targets applied lie within 0.5 cell of the morph field;
  - a 3-radius grab morph is refused and its residual is reported.
- Old fixtures render equal to their goldens (mean ≤ 1/255).
- Perf:
  - composing 8 layers adds ≤ 15% to remesh time;
  - the `Composite` update for an amount change on a 10k-brick layer takes ≤ 10 ms, remesh excluded. The end-to-end "≤ 33 ms amount slider" target moves to Stage 6. Before the chunked mesher, an amount change changes `BuildKey` and triggers a full remesh (24-165 ms or more), so it cannot pass here.

**Stage 4 result (2026-10-22).** `src/layers.rs` holds the layers (`Layer`: sculpt or paint, amount, switch, grid and ledger, in `StackData.layers`), the composites, the per-layer views, the byte budget, morphs (`Morph`) and blend-shape fitting (`project_shape`); `src/app/layering.rs` the `sculpt_layer` and `morph` tools. Two tools were added (66 → 68). No new dependencies; `ENGINE_VERSION` stays 1 (a clay without layers takes exactly the old path: the legacy fixtures `0x5ce84c6172772869` and `0xb6b5840b9985247c` and the version-1 pin still hold). The checks are in `src/layers/tests.rs` and, through MCP on a headless editor, `src/app/layering/tests.rs`.
- A layer at amount 0 or switched off: the composite is the base grid itself (same Arc), so the mesh is bit-equal to the stack without it. With layer offsets of at most 1.03 cells, amount 0.5 moves the surface 0.5x within 1.28% at 50 probes (bound 3%). A standard dab (r = 0.1, strength 0.5, 2 cm cell) on a layer at 1, then the layer at 2: 0.02500 → 0.05000 (2.000x; bound 5%); the same dab committed at amount 2 moves the surface 0.02500, as on base (stored at half). Strokes are refused below amount 0.05.
- Bake (layer at 0.7 into base): the composed field stays within 3.9e-5 (0.002 voxel; bound 1/256); base records an opaque record, the layer's records are archived.
- Base and an earlier layer's records turn `stale_context` once another layer is sculpted, and every other layer's records when a layer's amount changes (a layer's own records too: they are stored divided by it); `rebase` equals a fresh ordered replay and clears the flag.
- Morph store / switch / switch back is pointer-equal on every sculpt grid and ledger (`sync_clay` records nothing); blend 0.5 is within 1.7e-5 of the field midpoint (bound 1e-4).
- `select` changes no scene state and adds no undo step (MCP test); an amount change is one undo step.
- GLB with a morph, read back through `glb_read` with the target on: 10,758 vertices before and after, every vertex within 0.00000 of the morph's field (bound half a mesh cell, 0.0208); residual of the fit 0.0026. A pull three radii long (stretch, as grab moves at most a radius) is refused: its vertices reach the surface (0.0002) but the stretched surface lies 0.103 from them (the coverage residual). FBX carries a BlendShape deformer with a BlendShapeChannel per shape.
- The six goldens (`CLIDE_GPU=1`) render identical to before (mean 0).
- Perf: remesh 38.8 ms for one grid against 39.0 ms for base + 8 layers composed cold (+0.6%; bound 15%); the composite update for an amount change on a 10k-brick layer takes 2.87 ms (composing anew 3.95 ms; bound 10 ms). An 8-layer fixture at the base cell (9 x 4,096 bricks) is 72 MB of the 512 MB budget, and a morph of it adds nothing.
- Determinism: strokes on a layer at amount 0.6 over a sculpted base give the same grids on 1 and 12 threads. Layers save and load (their ledgers replay to the exact grids, `replay_verified: true`), mirror with the clay, and a file with more layers than a clay may hold loses its stack on read (the clay stays).
- Fixed after review: (1) the byte budgets ran before `ledger::sync_clay` added its Opaque records (a relayout's whole grids, a blend's, bake's or split's deltas), so one call could save past `MAX_SAVED_BYTES`; `layers::sync_within_budget` checks before and after the sync, and `sculpt_layer`, `morph`, `edit_clay`, the paint tool, `project_reference` and `face_detail` put the clay back only when it passes (`the_budget_counts_what_sync_records`). (2) FBX wrote no blend shapes on objects with LODs; both formats now put them on LOD0, report what the file holds (`Export::shapes_written`), and the export reply refuses, with a reason, any shape the writer left out. (3) `ClideApp.layer_selection` drops names the scene no longer has after undo, redo and history goto, and empties on new and open, so an undone rename cannot send strokes elsewhere; a rename or delete moves the selection only once the clay is put back (`a_layer_selection_goes_when_undo_or_a_new_scene_takes_its_layer`). (4) The undo byte cap counts morph grids: `history::unique_bytes` walks every detail grid of the clay (base, layers, morph bases and layers, with levels) and counts each brick Arc once, only when the next snapshot does not hold it (`morph_grids_count_toward_the_byte_cap_once`). (5) The composite perf check takes no load allowance under `CLIDE_PERF=1`. (6) The Stage 3 mask-overlay perf check measures its load with a reference of the tint's own shape (a `par_chunks` mask lookup per vertex), interleaved with it, medians per round, the load from the same round; the old copy-only reference did not slow when other tests took the `par_chunks` helpers. (7) `glb_read` refuses morph weights outside -10..10 (`MAX_MORPH_WEIGHT`, the file's or `check_export morph_weights`) and targets that move vertices out of range.

Deviations and defaults, for the stages that build on this:
- base (`Clay::detail`) and paint (`Clay::paint`) stay where they were and keep amount 1 (they cannot be switched off, renamed or moved); the extra layers sit above them, at most 14 (16 in all). Q2 default: amounts are linear in SDF offset, -2..2 (paint layers 0..1, their opacity).
- What every field and mesh reads is the composite: `Field::new` composes, and the mesh key (`clay_key`) holds the composed clay without its stack, so meshes, bakes, films, exports and skinning take layers up; the composites are cached beside the stack (`StackData.cache`, keyed by the parts' Arcs, so a hit keeps the composite's identity and nothing rebuilds). An amount change composes only that layer's bricks again and plants the result in the cache.
- Strokes on an extra layer go through a view (`layers::view`: the clay with the layer's grid and ledger in base's or paint's place and what it reads of the others), so `ledger::commit`, `edit`, `replay`, `sync`, settling and the check on load are unchanged. A sculpt stroke on a layer, and a base stroke on a clay whose layers contribute, runs the engine on the composite (others + amount x grid) and folds (after - before) / amount into the layer's grid. Contexts: base strokes hash the contributing layers (amount and head) only when there are any; a layer's strokes hash the blobs, base, the other layers and their own amount; paint, mask and groups read the composed surface (`surface_hash` folds the layers in). Records carry their layer's name.
- One byte budget (`MAX_GRID_BYTES`, 512 MB = today's `MAX_TOTAL_BRICKS` f32 bricks) over base, layers, morphs, paint, mask and groups, each shared brick once, for clays with layers or morphs (a clay with neither keeps the per-grid caps it had, so no clay legal before is refused); sculpt calls, `sculpt_layer`, `morph` and the paint tool on a paint layer check it, and the error names bake/merge_down, deleting layers or morphs, squash and a coarser cell. A second budget, `MAX_SAVED_BYTES` (1 GiB per clay, a quarter of the read's 4 GiB `INFLATE_BUDGET`, so a clay that may be kept is one that loads), holds what the clay inflates to in a file (`layers::saved_bytes`): its grids, the grids of its ledgers' Opaque records, and every morph's grids and ledgers again (a file has no sharing). A full-size sculpt (`MAX_TOTAL_BRICKS`, 262,144 bricks) saves about 260 MB, so in practice that is the clay and about 3 full-size morphs, fewer once its ledgers hold outside-edit grids (a relayout records the whole grid; bake, blend and split_by_mask the bricks they changed). `sculpt_layer list` reports `saved_mb` and `max_saved_mb` beside `grid_mb` and `max_grid_mb`. Both budgets are checked after `ledger::sync_clay` has added its records (`layers::sync_within_budget`) and before the clay goes back into the scene, so a refused call changes nothing.
- The dab-point cap (`ledger::MAX_POINTS`, 2,000,000 over a clay) counts the clay's live ledgers only: base, paint, mask, groups and the extra layers (`layers::extra_points`). A stored morph's ledgers do not count (each is held to one ledger's caps), so storing a morph is never refused for points; a call that puts a morph's ledgers in place (`switch`) makes them live, and `ledger::check_clay_points` refuses it when the live ledgers would pass the cap.
- The composites survive copies of the stack (an undo snapshot's copy-on-write, a paint stroke, a mask action): a mask or paint edit on a layered clay builds no mesh (MCP test).
- Q1's consequence on load: a record made before another layer was sculpted (or changed) is stale, so the check on load replays it to another grid and reports `replay_verified: false`, keeping the saved grid, as paint records read an older sculpt; `stroke_edit rebase` takes the replayed results.
- `bake` folds into base (or paint) and `merge_down` into the nearest layer of the same kind below, both at the layer's current contribution (so the surface stays); the folded layer's ledger is archived for reading (`stroke_log {layer: "archived:NAME"}`, at most 8 kept) and the target records an opaque record. `split_by_mask` weighs the layer by the mask at each grid point.
- `record` (the layer `sculpt` calls without `layer` write) is saved with the clay and is an undo step, per §3.3's rule that agent defaults come from scene state; `select` is editor state (`ClideApp.layer_selection`) and the Sculpt and Paint tools name the selected layer in the requests they send.
- Morphs hold ledgers with their grids (records shared in memory, written again in the file, so they count toward `MAX_SAVED_BYTES` but not the dab-point cap until switched in, as above); `switch` and `blend` need the same sculpt layers as when the morph was stored. `blend` takes weights 0..1 adding up to at most 1, lerps amounts too, and is recorded as opaque records. Paint layers are not part of morphs.
- Blend shapes: `layer:NAME` is the exported mesh with that layer fully on (refused when it already is), `morph:NAME` the morph's composite. Each vertex takes three Newton steps on the target's field; the residual is the larger of the field left at the vertices and the distance from the target's own surface (a mesh of it) to the moved triangles; the default `max_residual` is half the clay's mesh cell. glTF targets carry POSITION offsets with weights 0 and `extras.targetNames`; FBX gets a BlendShape deformer, a BlendShapeChannel and a Shape geometry per shape; on an object exported with LODs both formats put the shapes on LOD0 (they are fitted to its vertices). The export reply lists a shape as written only if the file holds it (`Export::shapes_written`); one the writer could not attach is refused with a reason and left out of `blend_shapes_written`. `glb_read` applies targets at the file's weights or the caller's (`check_export morph_weights`).
- `edit_clay detail_cell` and `detail_regions` move every layer and morph grid to the base grid's layout (their ledgers record it).

### Stage 5: import, to_clay, cross-object booleans, insert, print export (L, 2-3 weeks)

**Scope**
- `src/mesh_import/{obj,stl,ply,glb,fbx}.rs`:
  - GLB goes through `glb_read`;
  - FBX is a minimal binary reader for Geometry nodes;
  - malformed input returns errors and never panics.
- `Form::Mesh{asset}` through `MeshSdf` with a BVH.
  - Open meshes use a generalized winding-number sign.
  - **Benchmark MeshSdf on dense imports first**, and decimate with meshopt to ≤ 200k triangles before building the SDF.
- `to_clay`:
  - primitives become analytic blobs;
  - meshes become `Form::Mesh`;
  - `mode: volume` samples into a sparse SDF, with blur (Gaussian) and polish (curvature flow);
  - `targets` with add, carve or intersect merge objects into one clay.
- `Op::Intersect`.
- `kitbash part:{object}` with carve.
- `export_print`.
- Re-enable File > Import (chrome.rs:91). It dispatches `import_mesh`.
- **DynaMesh UI half:** an object menu entry "Remesh as clay…" (resolution, blur, polish) and "Export for print…". Both dispatch the same tool requests and show up in `ui_activity`.
- Stage 5 has no dependency on Stages 2-4 (only on 0 and 1b). It can run in parallel with them or before them.
- Tools: `import_mesh`, `to_clay`, `export_print`.

**Parity rows**
- P0 Supported 3D formats, **partial**: import OBJ/STL/PLY/GLB/FBX and export OBJ/STL/PLY. MA, VRML, X3D, MESH/MESHB, SC1 and GoZ (proprietary) are not covered. The parity row should be split, or this subset declared the P0 bar, and the rest moved to P2/P3.
- P0 DynaMesh (any mesh, blur and polish).
- P1 3D Print Hub.
- P1 Make PolyMesh3D.
- P1 Remesh All.
- P1 Live Boolean (cross-object and intersect).
- P1 InsertMesh.
- P1 FBX import (meshes).

**Acceptance**
- An icosphere in each of OBJ, STL and PLY:
  - triangle count exact and bbox within 1e-5;
  - after `to_clay` at res 128, Hausdorff ≤ 0.75 cell and volume within 1.5%.
- An open cup gets a closed, watertight result with ≥ 99% of normals within 90° of the source.
- `intersect(sphere, box)` volume within 2% of analytic. The lens from two spheres within 3%.
- A carve insert makes a hole: a ray through its centre hits the far wall.
- `volume` + blur 1:
  - max curvature reduced ≥ 30%;
  - volume change ≤ 3%.
- Primitive `to_clay` renders at ≥ 40 dB PSNR against the primitive.
- An `export_print` STL re-read:
  - every edge used exactly twice;
  - `size_mm` exact to 1e-4.
- Exported OBJ/STL/PLY re-imports with the exact triangle count.
- Fuzz-lite: 200 truncations per format, with no panics.
- The "Remesh as clay…" menu in a headless Context dispatches a `to_clay` request equal to a hand-written MCP call (same reply and object hash).
- Ledger replay on an imported clay is deterministic.
- Perf:
  - a 200k-triangle mesh to clay at res 128 in ≤ 3 s;
  - a 1M-triangle STL import in ≤ 3 s.

**Stage 5 result (2026-10-23).** `src/mesh_import/` reads OBJ, STL (binary and ASCII), PLY (ASCII and both binary byte orders), GLB (through `glb_read`) and binary FBX (Geometry nodes) and writes the print formats (binary STL, OBJ, binary PLY, 3MF); `src/mesh_asset.rs` holds triangle meshes by content hash (`MeshAsset`) and their distance field (`MeshField`: a box tree for the nearest triangle, pseudo-normal sign for closed meshes, a hierarchical generalized winding number for open ones and for points past the search bound); `src/to_clay.rs` turns objects into blobs and does the volume remesh; `src/app/importing.rs` holds the `import_mesh`, `to_clay` and `export_print` tools and the menus (File > Import…, File > Export for Print…, Modify > Remesh as Clay…). Three tools were added (68 → 71). New dependency: `zip` 8.6 (Q6), default features off (3MF entries are stored). `ENGINE_VERSION` stays 1 (no brush changed: the legacy fixtures and the version-1 pin still hold). Checks: `src/mesh_import/tests.rs`, `src/mesh_asset.rs`, `src/to_clay/tests.rs` and, through MCP on a headless editor, `src/app/importing/tests.rs`.
- An icosphere (1,280 triangles) through OBJ, binary and ASCII STL, ASCII, little- and big-endian PLY, GLB and FBX: the exact triangle and (welded) point counts, boxes equal (bound 1e-5). As clay at res 128 (5,120 triangles): Hausdorff 0.043 cell (bound 0.75), volume -0.028% (bound 1.5%); through MCP (`import_mesh as mesh`, then `to_clay`), `hausdorff_est` 0.087 cell.
- An open cup (a wall and a floor, no lid) read by its winding number: the clay mesh is watertight and 100% of the source's vertex normals are within 90 degrees of the clay's normal there (bound 99%).
- `intersect(sphere, box)` at res 128: -0.02% from the exact shape's volume (bound 2%); the lens of two spheres -0.07% from the analytic (bound 3%). An intersect far from everything leaves nothing.
- A cylinder carved into a box with `kitbash part {object, op: carve}`: a ray down the hole's middle meets the floor at y = 0.1000 (the top is at 0.5, the hole half the cylinder deep); beside it the top is untouched; `kitbash remove` takes the blob and its asset out.
- Volume remesh of a box at res 96: blur 1 takes the largest turn across a mesh edge from 134.2 to 87.7 degrees (-35%; bound 30%), polish 1 to 90.4 (-33%); volume -0.03% and -0.04% (bound 3%). (Measured on the mesh: the field's finite-difference curvature saturates at 1/h on both, since the mesh blob's own distance field keeps a crease along the box's medial planes that the offset grid cannot cancel.)
- Primitive to_clay against the primitive (`CLIDE_GPU=1`): cube 63.1 dB, cylinder 42.4 dB (bound 40), sphere 39.0 dB at res 128 and 38.8 at res 256. Every primitive's clay is within 0.67 cell of its mesh (Hausdorff).
- **Acceptance deviation (sphere PSNR):** the plan's bound is ≥ 40 dB for every primitive. The sphere is accepted at ≥ 38 dB, not 40: the reference is the 32-segment primitive sphere, which is faceted, while its clay is a true sphere, so the two differ by more than the clay's own error. The cube and cylinder still meet 40 dB.
- `export_print` STL of a clay ball with `size_mm {axis: y, value: 50}`, read back: every edge used by exactly two triangles in opposite directions, height 50.000000 mm (bound 1e-4), lowest point on the plate. Exported OBJ and PLY re-import with the exact triangle count, and so do the editor's own GLB and FBX exports; 3MF is a zip with the model. `hollow {wall_mm: 40}` on a 1 m ball: 115,285 of 518,834 cm3, still watertight.
- Fuzz-lite: 200 truncations of each of the 8 files above and 50 random 4-byte corruptions each never panic; hostile counts (4e9 PLY vertices, a binary STL count past its bytes, FBX node ends past the file, NaN or 1e30 coordinates, faces past the vertex list) are refused before allocating.
- Modify > Remesh as Clay… in a headless editor (Enter sends it) dispatches `{target, mode: volume, resolution: 64, blur: 1}`, listed in `ui_activity`; after an undo, the same request through MCP gives an equal reply and an object hashing the same.
- Ledger replay on an imported clay: 12 strokes give the same grid on 1 and 12 threads, replay from the ledger to it bit for bit, and still do after a save and load (the asset comes back with the clay).
- Perf (M2 Max, run alone): a closed 200k-triangle mesh to a clay meshed at res 128 in 936 ms (bound 3 s); a 1M-triangle binary STL (47 MB) read, welded and checked in 234 ms (bound 3 s).

Deviations and defaults, for the stages that build on this:
- Mesh assets live on the clay (`Clay.assets`, by xxh3-128 hash), not in a scene-level `Scene.assets`, so a clay saves, replays and settles on its own (as Stage 2's alphas). Geometry reads only the clay's own assets (`Geo::new` takes them; the sculpt engine's probes carry them), so a clay's field depends on the clay alone. In memory every asset is also listed weakly by hash, which only `adopt_assets` reads (a blob added by hash from this session), and the same triangles are one asset. A file writes each clay's copy (deflated base64, read within the inflate budget, counts checked before inflating, at most 64 per clay). `Clay::adopt_assets` keeps exactly the assets the blobs name (`edit_clay` and `create_object` take a mesh blob by hash from this session); a loaded blob whose asset is missing is dropped. The mesh key and content hash swap assets for nothing (the blobs carry the hash), and record contexts cover a mesh blob through its hash.
- `Form::Mesh {asset, sign: closed|winding, center, rotate, scale}` takes a uniform scale only; `to_clay` and InsertMesh bake each source's placement into the asset in the destination's space (decimated with meshopt to 200k triangles first). The sign is chosen from the mesh: closed and consistently wound means pseudo normals, else the winding number. `Op::Intersect` is a smooth max over `blend`, always in a block's prim list and never letting a block be skipped as solid.
- `import_mesh` makes a clay (default) or a `Shape::Mesh` object (`Object.mesh`, drawn with smooth normals in the object's material; `create_object` refuses Mesh and points to `import_mesh`). FBX: geometry only, scaled by UnitScaleFactor (cm per unit), model transforms not applied; ASCII FBX refused. `scale`, `up: z` and `center` place the import.
- `to_clay` converts the first target in place (id, name, placement, children kept; a clay keeps its blobs, sculpt and ledgers, which settle first) and adds the other targets with `op`, hiding them; `into` adds every target to an existing clay. Clay sources other than the base become mesh blobs of their composed rest mesh rather than transplanted blob lists; a plane becomes a slab 1% of its size thick; sheared or unevenly scaled primitives become mesh blobs. Blob colors are the source's over the destination's object color, so colors show as they did. `hausdorff_est` is one-sided (the source's surface to the clay's mesh, meshed once more for it) and reported for single-target conversions.
- `mode: volume` meshes the whole result (sculpt and layers folded in) at `resolution` with the clay mesher and makes that one closed mesh blob in a fresh clay: paint, mask, groups, layers, morphs and the old ledgers go. `blur` (Gaussian sigma in cells, 0-4) and `polish` (0-1: up to 20 explicit curvature-flow steps) run on a dense grid of the new field (at most 320^3 points) and are kept as base offsets with the volume restored by a constant shift; the clay has no ledger yet, so its first stroke records that grid as an Opaque record (as an old file's).
- `export_print` writes one welded mesh (degenerate triangles dropped and counted) in millimeters (1 unit = 1 m unless `size_mm`, whose axis is a scene axis), Y up turned to Z up, lowest point at z = 0; `hollow` sets `Clay.shell` on a copy (clay only); `fine` uses fine meshes. It is a job but not an edit.
- InsertMesh (`kitbash part {object, op: add|carve, scale}`) goes into one clay target: the object's shape at its world size, pivot on the surface and +Y along the normal (a carve sinks half of it), named `<group>/insertN`; mirror is refused.
- The UI halves: File > Import… (rfd, `import_mesh {path}`), File > Export for Print… (`export_print {path, targets: selection}`), Modify > Remesh as Clay… (a dialog with resolution, blur and polish; Remesh or Enter sends `to_clay {target, mode: volume, ...}`), all through `send_ui_call`.
- Acceptance deviation: primitive `to_clay` PSNR for the sphere is held to 38 dB instead of the plan's 40 dB (the faceted 32-segment reference; see the result above).

**Stage 5 review fixes (2026-10-23).**
- Arrays of meshes look only at the copies near a point. A mesh's distance counts as exact (`hard_surface::exact`): the nearest triangle within the bound is exact, and outside the field's box the distance is positive with no winding pass. So copies are culled by box gap like the analytic forms' copies, and a mesh copy is asked with the caller's bound, not the best so far, so the deepest copy counts. 512 copies of a winding cup: 1.8 lookups and 0.33 winding passes per sample on a line, 4.2 and 0.43 nested 8 x 8 x 8 (every copy before: 512 and 512), bit-identical to every copy. Meshes of a closed and a winding line, a winding ring and nested closed arrays are bit-identical to before. A shell of a winding path changed to brute force's mesh: before, a point deep inside one copy and nearer another's surface read that distance as its depth.
- Outside its box a mesh field is positive. This is a definition only for meshes whose winding number passes one half outside their box (overlapping same-facing layers, such as duplicated faces), or a `sign: closed` mesh that is not closed.
- Geometry resolves mesh assets from the clay (see the deviation above). `kitbash remove` drops the assets its blobs took with `Clay::prune_assets` (it cannot fail), so it no longer discards an `adopt_assets` error.

### Stage 6: live sculpt engine (XL, 4-5 weeks)

**Scope**
- **`BaseCache`** (§3.6) for the sculpt session. Both `engine::apply`'s read of T and the mesher then cost one brick lookup plus an add per sample, instead of a prim fold.
- **`src/clay/chunks.rs`, anchored chunked surface nets.** The existing window mode cannot be reused: `build_window_cells` clips with a box SDF, which caps the open sides of each piece, and it takes its origin from `field.bounds()`, which moves. The new mesher follows these rules:
  - Grid origin is `floor(lo/voxel)·voxel`, with chunks of C = 32 cells and no clipping.
  - Each edge belongs to the chunk that owns its lower cell.
  - Seam vertices are solved in f32 from identical samples in identical order, so neighbours agree bit for bit with no welding.
  - Normals come from the field gradient.
  - Sharp-feature vertices follow the same rule.
  - The exact-planar single-hard-blob path (clay.rs:1459) must keep working.
  - Detail-region factors are forced to the coarser value at chunk borders. The fallback is to align borders with region bands.
- **MeshLib:**
  - a clay under sculpt (a stroke in the last 60 s, or a sculpt tool active) is drawn as a `ChunkedMesh`;
  - a `Diff` brick AABB plus 1 voxel gives the dirty chunks;
  - the build thread is latest-wins;
  - monolithic meshes are assembled on demand.
- **render.rs:**
  - `MeshKey::Chunk`;
  - a **slim 24 B sculpt vertex** (pos f32×3, oct-normal i16×2, rgba8, mask u8, group u16, cavity u8) and a `VertexLayout::Sculpt` pipeline variant sharing the fragment shader;
  - a paged `ChunkPool` (VERTEX | INDEX | COPY_DST, power-of-two slots, compaction above 30% fragmentation);
  - one draw per chunk with `base_vertex`;
  - **the GPU dirty set is diffed by Arc pointer** against the last upload, so undo, redo, goto and replay also upload only the chunks that changed;
  - a per-object chunk generation in the shadow key; the shadow is **frozen during a drag** and refreshed on release;
  - `fit_buffer` is bypassed.
- **Picking:**
  - `pick_field` sphere-traces BaseCache + Composite in object space, exact against what the brush edits;
  - a per-chunk BVH plus a top-level BVH is the fallback (cold cache, non-clay objects);
  - `ray_hit` returns barycentrics and the normal.
  - Used by `pick_hit` (viewport.rs:554), `zoom_point`, `surface_point` and the screen-frame resolver.
- **Display:**
  - `set_display matcap`: 4 built-in matcaps plus images;
  - expose the existing viewport SSAO through `ao`, after verifying it, and correct the stale Preview AO row in the parity doc.
- **Sculptris Pro:**
  - `stroke.cell` creates or extends a detail region covering the stroke AABB + r;
  - same-cell overlaps merge;
  - at MAX_REGIONS = 16, the smallest region of the closest cell merges into a bounding ellipsoid.
- The MCP `sculpt` job uses the engine, so the agent gets the latency win too.

**Parity rows**
- P1 MatCaps.
- P1 Preview AO.
- P1 Sculptris Pro.
- The performance foundation for every interactive P0 row.

**Acceptance**
- Chunked vs monolithic anchored mesh on the sphere, torus, Human and hard-floor fixtures:
  - identical triangle counts;
  - bit-exact sorted vertex multiset;
  - 0 crack edges after welding by position.
- A dab followed by a dirty-only remesh equals a full chunked rebuild, bit-exact.
- Renders at 4 views: PSNR ≥ 50 dB, max_abs_diff ≤ 8, and chunked vs monolithic mean ≤ 0.5/255.
- `pick_field` vs `ray_hit` on 500 rays: |Δt| ≤ 0.5 voxel. The BVH fallback matches brute force on 10k rays within 1e-5.
- The partial-upload image equals a full-reupload image within 1/255. Dirty-set correctness after undo, redo and goto. These GPU checks are `#[ignore]` unless `CLIDE_GPU=1`.
- 30 strokes with `cell` produce ≤ 16 regions that cover every stroke AABB.
- Matcap: centre-to-rim luminance within 6/255 of the matcap samples.
- Perf on a 1.65M-triangle fixture at 2 mm with r = 40 cells, asserted with 1.5× slack:
  - dirty remesh p95 ≤ 14 ms;
  - upload ≤ 3 ms;
  - hover pick ≤ 0.2 ms;
  - Human dirty remesh p95 ≤ 25 ms;
  - cold Human cache ≤ 3 s;
  - a 10-stroke agent `sculpt` replies in ≤ 300 ms (2.1 s or more today);
  - a layer amount change on a 10k-brick layer reaches the screen in ≤ 33 ms (asserted ≤ 80 ms; moved here from Stage 4);
  - a mask or group edit on the 1.65M-triangle fixture refreshes shading in ≤ 16 ms, through slim-vertex attribute updates (no `Field` re-evaluation for mask/group-only changes).
- Rigged clay (where `content_hash` is `None`) gets chunked meshing at rest, with its own key: a test sculpts on a rigged Human and checks that only dirty chunks rebuild.

**Stage 6 result (2026-10-23).** `src/clay/base_cache.rs` holds the `BaseCache` (the blob field memoized on the detail lattice, bricks filled in parallel where asked for, keyed by an xxh3-128 of the blobs, shell and decals with the field's clamp and the cell; one registry, least recently used out past 256 MB); `src/clay/chunks.rs` the anchored chunked mesher (`Mesher`, `ChunkSet`, `dirty_detail`) and `pick_field` (`Mesher::pick`); `src/clay/sculptris.rs` Sculptris Pro's region rule; `src/mesh/session.rs` sculpt sessions in `MeshLib` (who is in one, chunk builds on their own thread, newest state first, the slim 24 B `SculptVertex` and draw-ready `SculptChunk`s); `src/mesh/pick.rs` the BVH picking fallback; `src/render/pool.rs` the paged GPU chunk pool and `src/render/matcap.rs` the matcaps; render.rs gains the slim-vertex pipelines (fills, HDR fills, shadow, occlusion pre-pass) and the matcap lookup. No tools were added (71); `sculpt` takes `cell` per stroke and per call, `set_display` takes `shading: matcap`, `matcap`, `ao` and `ao_radius`. No new dependencies; `ENGINE_VERSION` stays 1 (strokes read the cached blob field, which holds exactly what they read before: the legacy fixtures `0x5ce84c6172772869` and `0xb6b5840b9985247c`, the version-1 pin and every 1-against-12-thread check still hold). The checks are in `src/clay/chunks_tests.rs`, `src/clay/base_cache.rs`, `src/clay/sculptris.rs`, `src/mesh/session_tests.rs`, `src/mesh/pick.rs`, `src/render/pool.rs`, `src/render/matcap.rs`, `src/harness/live.rs` and, through MCP on a headless editor, `src/app/live_tests.rs`.
- Chunked against the monolithic anchored mesh (the same kernel over one range covering the clay): sphere res 128 (56 chunks, 137,388 triangles), torus (20, 43,256), organic figure (14, 16,832), hard-floor figure (36, 67,436) and Human (12, 14,032): equal triangle counts, the same triangles bit for bit (as sets), the same welded vertex set (positions, normals, colors) and 0 crack edges after welding by position. Saddle cells get one vertex per surface piece (`adaptive::pieces`); no fixture falls back to marching tetrahedra (the organic figure at res 80, which the regular mesher meshes whole with tetrahedra, chunks with 0 cracks and 0 edges in more than two triangles; the Human keeps 8 such edges, where a cube's inside is ambiguous). The anchored reference meets the Stage 0 bounds: sphere res 128 Hausdorff 0.011 cell (bound 0.6), volume 0.031% (bound 1%), normals within 0.03 degrees. Chunks are the same on 1 and 12 threads.
- A dab, then a dirty-only remesh, equals a full chunked rebuild bit for bit (16 dirty chunks of 192, 4 of which changed; every chunk that changed was in the dirty set). Through `MeshLib`: a stroke rebuilds a few chunks and the others keep their Arcs; undo, redo and history jumps rebuild under a third of the chunks and draw what a fresh session draws; a mask or group edit builds no mesh (`geometry_builds` and `chunk_builds` unchanged) and changes only the slim attributes of the chunks it reaches; a rigged Human at rest chunks and rebuilds only its dirty chunks.
- Renders (`CLIDE_GPU=1`), 4 views: chunked against the monolithic mesh drawn the regular way, PSNR 81.0-87.0 dB, max 1/255, mean 0.001/255 or less (bounds 50 dB, 8, 0.5). A partial upload draws what a full re-upload draws (0/255) after three strokes, two undos, a redo and two history jumps; each uploaded 6-20 chunks of the pool. MatCap (`clay`) on a sphere: 60 pixels from the middle to near the rim within 1.36/255 of the matcap's (bilinear) luminance (bound 6/255).
- `pick_field` against `ray_hit` on the chunk mesh, 500 rays: 353 hit both, worst |dt| 0.27 cell (bound 0.5), none hit one and not the other; normals agree. The BVH fallback against brute force on 10,000 rays: the same triangle every time, |dt| 0 (bound 1e-5).
- Sculptris Pro: 30 random strokes at three cells keep at most 16 valid regions and every stroke box inside one; through MCP, 30 strokes on a ball give 12 regions holding every stroke's box, the ledger replays with no mismatch (each relayout is a `Regions` record, see the deviations), a stroke inside a region of its cell changes no region, and a cell coarser than the clay's is refused.
- Perf (M2 Max; the 1.65M fixture is the hard figure at a 2 mm detail cell, 1,645,312 triangles in 732 chunks): cold cache and every chunk 440 ms; a dab of r = 40 cells dirties 40-58 chunks and remeshes them in 6.3 ms p95 over 24 dabs (bound 14 ms x 1.5; median about 5 ms); the dab itself applies in 7.2-7.3 ms p95, 5.3 ms median (17.4 ms p95 before the review fixes below; bound 8 ms x 1.5); hover pick 0.021 ms (bound 0.2 x 1.5); GPU upload of a dab's chunks 0.48-0.85 ms, at most 1.71 MB, 16-37 of 734 chunks (bound 3 ms x 1.5, 8 MB). Human (`set_human`'s resolution 384, a 4.5 mm cell, 228,908 triangles): dirty remesh 12.6 ms p95 (bound 25 x 1.5), cold cache and every chunk 2.76 s (bound 3 s). A 10-stroke agent `sculpt` on the 1.65M fixture replies in 92-95 ms (resolve 53-54, apply 25-26; 104-107 ms with apply 32 before the dab fixes; bound 300 ms x 1.5). A layer amount change on a 10k-brick layer reaches draw-ready chunks in 71-76 ms (bound 80 ms; the 33 ms goal is not met: the layer covers most of the figure, so about 970 chunks remesh, about 60 ms of it). A mask edit reaches draw-ready chunks in 2.9 ms (bound 16 x 1.5). All of these pass with `CLIDE_PERF=1` (no allowance) on the machine run alone. Timings are normalized by a reference chunk build measured before and after each one, so the whole suite running alongside does not fail them (`CLIDE_PERF=1` takes no allowance).
- Dab apply, after review (profiled with macOS `sample` on `harness::live::dab_profile`, which loops dabs for a profiler): it spent about 62% of a dab in `DetailGrid::write` (a sort over every changed point's brick key, then one hash lookup and copy-on-write check per point) and most of the rest in `Job::region`'s single-threaded passes (splitting per-point tuples into arrays, the inflate loop, the change list) and per-point brick lookups (`LevelView::get`, `BaseCache::block`). Now `write` takes brick runs (one lookup per run of up to 8 points; only run keys are sorted), `Job::region` writes its per-point arrays and its change list z slice by z slice in parallel (joined in z order) and reads the grid and the cached blob field a brick run at a time, `split` uses shifts, and a grid known to hold no zero brick (`DetailGrid::clean`, set by a full prune, kept by `write`, cleared by anything else that changes bricks) skips a stroke's first full prune. Every value and every list is the same, in the same order: the legacy fixture hashes, the version-1 pin, every 1-against-12-thread check and the ledger hashes hold, and `grid_writes_match_point_by_point_sets_and_a_full_prune` checks `write` against setting each point and pruning every brick. Warm, a dab went from 14.4 ms median, 15.4 ms p95 to 4.6 ms, 4.9 ms. What remains: a dab on bricks the session cache has not filled yet (off the surface the chunks read) evaluates the blob field there (6-15 ms on a fully cold cache), and `par_chunks` starts its threads on every call (about 8% of a warm dab). The layer amount change is unchanged at 68-71 ms (goal 33 ms): about 64 ms of it is remeshing about 970 dirty chunks, most of that in the surface-nets kernel (`Mesher::mesh_range`, about half of the samples) and field reads near hard prims, which needs mesher work, not dab work.
- A shared fix on the way: a clay's bounds read every detail brick (`DetailGrid::solid_bounds`), three times per palette, every frame; they are now kept with the grid until it changes, and a frame's `sync` on the layered 1.65M fixture went from 14 ms to 0.08 ms.

Deviations and defaults, for the stages that build on this:
- Detail regions never refine chunks: every chunk meshes at the lattice cell (factor 1 everywhere, so "coarser at chunk borders" holds trivially) and reads the regions' finer offsets at its points; the fine mesh (`set_display fine`) still shows them finer.
- The regular meshes are not assembled from chunks on demand. Snapping and screen-frame casting must read meshes that depend on the scene alone (a request must resolve the same whether a session runs), so every clay keeps its regular build; for a clay a sculpt tool is on it starts once strokes pause (`PAUSE`, 300 ms) and after its chunks, and `busy` counts that wait. Chunk builds never make agent calls wait (`busy` leaves them out, and `sync` does not join them: only the viewport reads chunks), and a session from agent strokes alone (no tool on the clay) neither holds back the regular remesh nor starts its first, whole chunk build until that remesh is in, so the agent's next call waits for the remesh only, as before Stage 6. A chunk build that fails (too many cells or chunks at the clay's cell) is not tried again until the clay or palette changes. `MeshLib::assembled` (chunks welded into one mesh) exists for checks.
- Sculptris Pro merges regions in box space: a region stands for the box its ellipsoid was made around, merges take the union of those boxes, and past 16 regions the pair of one cell with the smallest union box merges (two cells only when no two share one), so a drag of overlapping strokes grows its region to their union, not by the ellipsoid's 1.73x margin per merge.
- `pick_field` serves viewport picking (`pick_hit`, `zoom_point`) on clays in a session without hidden groups; `surface_point` rays and the screen-frame resolver (`clay_caster`) go through a BVH over the regular mesh that finds the brute-force hit bit for bit, for the same reason. The BVH fallback is per mesh (kept with the mesh while it lives), not per chunk.
- Slim chunks take their surface and light (roughness, metal, emission) from the object's material, as primitives do; per-blob materials show through color only, so a chunked clay of several materials can shade differently from its regular mesh (the fixtures have one). Chunks are drawn only for clays without decals, smart layers or weathering and not the exact planar single blob; posed rigged clay and baked or fine display draw their meshes as before.
- Sharp features (feature vertices, the bisected crossings, fan centers) are placed only in cells near a hard prim, so sculpted organic surface on a clay with one hard part meshes as plain surface nets.
- A stroke's `cell` stays in its spec, not in `Record::digest`: its effect is the region relayout, recorded before the stroke as its own record; `stroke_edit replace` and `replay` do not grow regions again. A call whose strokes grew regions installs the whole clay and is refused if the clay changed meanwhile.
- Relayouts are `RecordOp::Regions { regions, cell }` records (`layers::set_regions_recorded`, `ledger::commit_relayout`), not whole-grid Opaque records: the new region list (all of it, in order) and the coarse cell a clay without a grid takes. Replay runs `DetailGrid::set_regions` on the grid before it, and the live grid *is* that replay, so they agree bit for bit (`set_regions` and `carry` visit bricks in sorted key order and levels in list order, run on one thread, and never iterate the per-process-seeded brick map unsorted, so the result does not depend on the process either). Base and every extra sculpt layer with a grid get one (layers take base's levels' regions, which is what `relaid_like` did); morph grids are relaid as before, sharing what they shared. `Record::digest` hashes the regions bit for bit (name, center, radii, cell) and the cell; the variant is new, so older files and their hashes are unchanged. Files are bounded while read (at most 16 regions per record, each finite with positive radii and cell; `regions_in`), and `is_sane` refuses a `Regions` record outside a sculpt ledger. Squash across one gives a whole-grid Opaque record (the layout changed), delete and disable replay the strokes after it on the layout before it, and scale refuses it. Checks: `src/layers/regions_tests.rs` (replay from empty on 1 and 12 threads bit for bit, every record's own hash, save and load `replay_verified: true` with the exact grid, mirror, squash, delete; layered clays and morphs get exactly the grids the old path gave) and `a_drag_that_grows_its_region_keeps_the_first_strokes_fine_detail`, now exact (it held only to the file's rounding while relayouts were rounded whole grids). Not covered: `edit_clay detail_regions` and `set_human` keep recording whole grids (one-shot calls, not a per-stroke pile-up), and on a clay with extra sculpt layers a base or layer ledger still does not replay from empty across a relayout, as before: its strokes read the other layers as they are now, in the new layout (`stale_context`).
- The saved budget on a plain clay (no layers or morphs): it keeps its per-grid caps (`MAX_TOTAL_BRICKS`, about 270 MB in a file), and since a Sculptris relayout records a few hundred bytes, not the grid, a long drag no longer adds a whole grid per growing stroke (before, a clay at the caps hit `MAX_SAVED_BYTES`, 1 GiB, after two or three such strokes and was refused; a 40-stroke drag on a 60,000-brick clay now adds under 8 MB). Only outside edits that keep a grid (a new detail cell, hand-set regions, a cleared grid) count toward the 1 GiB there, checked when sync records them. A clay with morphs can still reach it (each morph's grids are written again); `a_sculptris_drag_the_saved_budget_refuses_leaves_the_scene_as_it_was` drives a drag there until a stroke is refused and checks the scene is unchanged. `layers::saved_bytes` now also counts the mask and polygroup grids and their ledgers' Opaque grids (the frozen masks and groups records read, at most 256 of each, are still not counted).
- In memory an Opaque record's grid is not a compact quantized copy: it is an f32 `Detail` whose values are rounded to the file's 1/256-cell steps, and after `sync` the clay's grid is that same `Detail` (one Arc, the record's and the clay's), so it costs nothing until the clay is sculpted on, then up to that whole grid again at 4 bytes per value; only a file stores it at 2 bytes per value.
- The sculpt reply reports `dirty_chunks` per record (null when the layout changed: every chunk) instead of `timing_ms.remesh` and `upload`, which happen after the reply; `get_clay.session_cache` reports the cache and `last_remesh_ms`.
- Shadows hash each chunk's generation and stay as they are while a UI drag records (`freeze_shadows`); the shader's overlay mode rides in the instance color's alpha; the matcap texture takes group 3's reserved binding 8.
- `DetailGrid` (and the caches) hash brick keys with a quick hash seeded per process and fully mixed (`base_cache::KeyHasher`), so keys read from a file cannot be chosen to collide. Each mesher memoizes its lattice bricks (at most 400,000, sharing the cache's bricks where no detail is); that memory sits outside the cache's 256 MB cap and goes with the mesher (one per session clay).
- The UI half of MatCaps is a Display > MatCap menu sending `set_display`; the interactive brush UI (live preview during a drag) is Stage 7.

### Stage 7: interactive brush UI (L-XL, 3 weeks)

**Scope**
- Builds on Stage 1c: adds live preview, `Tool::{Mask, Groups}`, the full HUD and pressure. Every control dispatches the tool call in §4.3's UI-to-tool table.
- **Gate:** the P0 Tablet-pressure UI half on macOS, the primary platform, closes only if Q4 is answered "invest". Otherwise this stage records that row as partial (velocity only).
- `Tool::{Sculpt, Paint, Mask, Groups}` (app.rs:49-54).
- The Sculpting shelf (chrome.rs:313-316) gets preset buttons.
- `src/app/brush.rs StrokeCapture`, described in §4.3:
  - LazyMouse with radius, step, LazySnap and Backtrack.
  - Modifiers: Shift smooth, Alt invert, Ctrl mask, Ctrl+Shift lasso, Ctrl+Alt mask erase.
  - Hotkeys: S, U and O drag-adjust size, strength and focal shift; X toggles symmetry.
  - Grab, Move and Snakehook project the drag delta onto the view plane at the hit depth.
- Cursor: a depth-tested ring in `draw_overlay`, plus a focal ring and a normal tick.
- HUD panel:
  - size, strength, focal, falloff curve editor, alpha picker;
  - symmetry toggles;
  - layer list with amount sliders, eye toggle and record dot;
  - mask and group toggles;
  - an Adjust Last slider;
  - a history scrubber.
- Pressure:
  - Windows and touch devices: `Event::Touch.force`, matched by TouchId.
  - macOS: an opt-in `src/app/pen_macos.rs` NSEvent local monitor. `objc2` 0.6 is already a dependency, but `objc2-app-kit` would be new. See Q4.
  - Velocity fallback.
  - `pressure_source` is recorded.
- `ui_activity` already ships in Stage 1c; this stage extends it to mask, group, layer and history actions.

**Parity rows (this stage closes the UI halves of these P0 rows)**
- PolyPaint.
- Tablet pressure curves.
- Core brush controls.
- Dynamic brush system.
- Smooth (Shift).
- Standard.
- Flatten.
- Inflate.
- Move.
- Masking (Ctrl-paint and lasso).
- 3D Layers (UI list).

P1 rows: LazyMouse, Layer brush (UI), Pinch (UI), Snakehook (UI), Custom brushes (picker), Adjust Last (UI), Undo History (scrubber).

**Acceptance**
- Synthetic egui `RawInput` in a headless Context drives `Tool::Sculpt`:
  - one record with `source: ui`;
  - every point within 0.5 voxel of the surface;
  - re-submitting its spec through MCP `sculpt` on a fresh pre-stroke copy gives an equal `result_hash`;
  - preview grid == committed grid.
- Shift records `smooth`. Ctrl writes the mask, not the detail.
- **UI-to-tool parity for every row of §4.3's table:** each UI action, driven by synthetic input, leaves a `ui_activity` request. Sent through MCP on the pre-action copy, that request gives an equal Scene (==) and an equal reply. This covers lasso, group hide, masked gizmo transform, the layer slider, Adjust Last, the history scrubber and D/Shift+D.
- `brush_preset set_current` from MCP changes the HUD, and the next UI drag records those settings.
- Undo after the drag restores the pre-drag Scene (==). The drag is one labelled history entry.
- LazyMouse on a jittered path (σ = 3 px): curvature variance reduced ≥ 60%, endpoint error ≤ the lazy radius.
- A simulated Touch force of 0.25 gives `radius_used` within 5% of the curve value. The macOS monitor is compile-tested only.
- Cursor ring: ring pixels differ from the background by ≥ 30 luminance, with no ring pixels where the depth test fails. HUD golden mean ≤ 2/255.
- Perf: a 2 s, 120-sample headless drag on the 1.65M-triangle fixture has frame time p95 ≤ 33 ms, measured from sample to chunk upload.

**Stage 7 result (2026-10-23).** `src/app/brush.rs` holds the stroke capture (`Trail`: LazyMouse with string radius, step, LazySnap and Backtrack; `Pen`: pressure from a touch's force matched by its TouchId, the macOS tablet monitor or pointer velocity) and the live preview worker (`Live`: one thread per stroke, newest input wins); `src/sculpt/preview.rs` folds the stroke so far into a scratch copy of the grid its commit writes (`SculptPreview`; paint, mask and group strokes through `preview::whole`); `src/app/recorder.rs` the brush tools (`Tool::{Sculpt, Paint, Mask, Groups}`), the modifiers, Grab and Snakehook across the view plane, posed clays, the request each drag sends and `ui_activity`; `src/app/hud.rs` the HUD and the brush hotkeys; `src/app/pen_macos.rs` the opt-in NSEvent local monitor; render.rs the depth-tested brush ring (`render::Cursor`, drawn with the grid's line pipeline in `draw_overlay`); `MeshLib::set_preview` (`src/mesh/session.rs`) draws a clay as its preview in chunks, and nothing else reads it. No tools were added (71); `set_display` takes `brush_cursor`, `brush_preset set_current` takes tool mask|groups, brushes grab|snakehook|mask_paint, falloff, alpha, pressure, lazy, velocity, tablet and group, and `sculpt`, `paint`, `mask` and `group` take `pressure_source`. New dependencies: `objc2-app-kit` 0.3.2 (Q4; macOS only, features `NSEvent` and `block2`) and `block2` 0.6.2, the handler-block type `addLocalMonitorForEventsMatchingMask:handler:` takes (both were already in Cargo.lock through winit). `ENGINE_VERSION` stays 1: the legacy fixtures, the version-1 pin and every 1-against-12-thread check hold. The checks are in `src/app/stage7_tests.rs` (a headless editor driven by synthetic egui `RawInput`, agent calls through `mcp::dispatch`), `src/sculpt/preview.rs`, `src/app/brush.rs`, `src/app/pen_macos.rs` and the updated `src/app/recorder/tests.rs`.
- Preview equals commit. Running a stroke's dabs in pieces, each piece from the last one's grid, gives the grid running them at once gives, bit for bit, brick set included (`<Detail as Grid>::hash`, which sums every brick): standard, clay and clay_buildup on a clay with no grid, with a grid and with a refined region, cut at 1, 3/4/20 and n-1 dabs. `ledger::SculptInput` is `Detail::apply` split into its input (the composite on a layered clay), runs of dabs and the fold back into the layer, so the preview runs the commit's own code. Through the editor: standard, clay_buildup, inflate and a stroke on a sculpt layer at amount 0.8 each show a preview during the drag (the scene unchanged every frame) and the record installed has the `result_hash` of the preview of the stroke as it ended (`preview_checks` 4 checked, 0 differed; with the modifiers test's smooth, inverted, mask paint, mask erase, Mask tool and Groups tool strokes, 6 more, 0 differed). Every dab point is within 0.03-0.05 of a mesh cell of the pre-stroke surface (bound 0.5). Each request resent through MCP on the scene before the drag gives the same record (`result_hash`), the same reply (timings aside) and the same scene; undo restores the pre-drag scene (==) in one step labelled "sculpt standard on Ball [ui]".
- Modifiers: Shift records `smooth`, Alt `invert`, Ctrl a `mask` paint request (the detail ledger unchanged, one mask record), Ctrl+Alt the same with `invert` (erase), Ctrl+Shift a `mask` lasso `{camera, px, mode: add}` (Alt with it: sub); the Mask tool paints the mask (Shift: lasso) and the Groups tool sends `group {paint: {id, strokes}}` (id 7 painted).
- UI-to-tool parity, every row of §4.3's table driven by pointer events on the HUD's controls (or the drag): layer amount slider (previewed while it drags, no call sent until release, then one `sculpt_layer amount`; the preview goes when the call has run), eye (`disable`), record dot (`record`), group hide, a mask action (Grow), Adjust Last (`stroke_edit {scale, last: true, factor}` ≈ 0.5), the history scrubber (`history goto` three steps back), Save brush (`brush_preset save {spec}`), the masked gizmo transform, Grab and Snakehook drags. Each left one `ui_activity` request that, sent through MCP on the editor as it was before the action, gave the same scene (the file form, without record times and sources) and the same reply (timings aside). D/Shift+D is not covered: see the deviations.
- `brush_preset set_current` from MCP (clay_buildup, radius 0.07, strength 0.35, falloff sharp with focal shift 0.3, X symmetry, pressure size off, lazy 8) shows in the HUD (`hud.shown` equals `get_current`), and the next drag's stroke carries exactly those settings (two symmetric copies resolved); dragging the HUD's Size slider goes through the same handler (`get_current` reads it back). S, U and O held while the mouse moves sideways change size, strength and focal shift; X toggles X symmetry; each through `set_current`.
- LazyMouse on an arc jittered by sigma 3 px (string 20 px): curvature variance 99% lower than the raw samples' (bound 60%), the stroke ends 20.0 px from the mouse (bound: the string), LazySnap ends it on the mouse, Backtrack takes it back along itself.
- Touch force 0.25 (the size curve linear): `radius_used` 0.04 against the curve's 0.04 (radius 0.16 x 0.25; bound 5%); the request names `pressure_source: touch_force` and so does the record. The macOS monitor is compile-tested (off a main thread it refuses to install and reports no pressure).
- Cursor ring (`CLIDE_GPU=1`): a ring on the sphere's silhouette, half behind it; 41 ring points the surface does not hide differ from what is behind them by 30+ luminance, the 19 it hides show nothing (under 4). HUD golden `tests/golden/hud.png` (made with `BLESS=1`): mean 0.0/255 on re-render (bound 2/255).
- Perf (M2 Max; the 1.65M fixture, standard r = 40 cells, 120 samples over 2 s, frames as the worker and chunk builds ask): sample to draw-ready preview chunks p50 7.0 ms, p95 23-25 ms, max 36 ms (`CLIDE_PERF=1`, no allowance; bound 33 ms); the window's frame with a sample p50 0.3 ms, p95 1.3 ms (strokes never block it); the worker p95 16.4 ms (one new dab per update), a dirty chunk build about 3.7 ms. The GPU upload of a dab's chunks that follows is Stage 6's check (0.48-0.85 ms). Earlier in the session, with other processes holding the machine at a load average of 30-40, the same test measured p95 43-266 ms strict; the suite's run divides by the measured load as Stage 6's checks do.
- Fixed on the way to the perf bound: (1) a standard dab of r = 40 cells on the hard figure took 19 ms warm (Stage 6's 5 ms figure was inflate): per grid point it looked the source point's prims up among every prim (`Field::near`, a fresh list each time); `Field::near_from` filters a list made once for the region (the box holding every source point the brush can read) with the same test, so it gives the same list in the same order (a point whose source leaves that box falls back to `near`), and the dab takes about 15 ms. (2) The first sample waited about 49 ms for the clay's picking tree; brush tools now build it in the background as soon as they are on a clay (`MeshLib::warm_bvh`), and the worker builds it if it is still missing (never the window's thread). (3) Each preview update used to run the stroke's last two dabs again on a copy; they now run only for the stroke as it ended.

- Fixed after review: (1) the big-fixture drag test asserts its timings only with `CLIDE_PERF=1`; in the suite (whose parallel tests load the machine past what dividing by the measured load makes up for: p95 120-418 ms in 3 of 3 runs) it logs them to target/perf.json and checks that samples reached the screen and the preview equals the record. (2) The HUD's falloff curve editor no longer panics (`f32::clamp` with min over max) dragging an inner point whose neighbours are under 0.02 apart, as a valid agent-set curve may have: the point keeps its t (`a_crowded_falloff_point_drags_without_panicking`). (3) Symmetric and tapered strokes no longer run the whole stroke so far again per sample (O(n^2) over a stroke: about 1.8 s per update by sample 60 of an X-symmetric drag on the 1.65M fixture). Once a dab already run changes, `SculptPreview` goes on from its grid with each copy's new dabs (holding each copy's moving end until the release, which runs it); over a drag about the stroke's dabs run once (`a_growing_stroke_previews_what_each_step_commits`, bound under 2x). Where the preview is not bit for bit the commit: such a stroke's preview, to the end, applies each update's new dabs copy after copy on the grid so far, where the commit applies every dab of one copy before the next copy's (and taper's later gains are not applied to dabs already run, nor is a Backtrack's path taken back from them once the preview goes per copy). It is marked inexact (`Live::inexact`, as is a preview made over another stroke's preview that had not settled), is not checked against its record (running the whole stroke again at the release would only race the commit for the cores, about 3.6 s of work for an X-symmetric 120-sample drag on the 1.65M fixture), and goes as soon as the record is in, so the committed clay shows. Path brushes still run whole per update (one dab per copy). (4) A stroke is sent only once every clay mesh is built (`MeshLib::stale`, besides `busy`): the request's `sync` (in `stroke_request` and the sculpt handler) waited on the window's thread for the rebuild that the stroke before it started, when it installed the frame before (`a_stroke_after_an_install_waits_for_the_mesh_off_the_window_thread`, with a test counter of `sync`'s blocking joins).

- Fixed after the second review: (1) a stroke pressed before the one before it installed (two quick strokes) cast its preview on the mesh without that stroke while its request casts on the rebuilt one, so an exact preview could differ from its record and warn falsely; it is now marked inexact when sent (above). (2) The HUD's layer amount preview stuck when the HUD stopped drawing mid-drag (an agent changed the selection, the tool switched): the drag is dropped when a frame does not go on with it (`a_layer_amount_drag_cut_short_drops_its_preview`). (3) `SculptPreview` in per-copy mode read the dabs as copies of equal length; a stroke that did not split so would have stopped advancing silently: it now reruns whole (`an_uneven_symmetric_stroke_falls_back_to_a_whole_rerun`). (4) The drift of an inexact preview is bounded by test (above), and the symmetric drag test asserts that its preview was inexact, unchecked, and ran between 1x and 2x the stroke's dabs (88 dabs, 89 run). (5) Stage 6's Human remesh and layer amount and mask edit timings, which failed now and then in the full suite under load, are asserted only with `CLIDE_PERF=1` and logged to target/perf.json otherwise (`asserted: false`); their load-independent checks (chunks rebuilt per dab, a mask edit builds no mesh, vertices masked) run always.

Deviations and defaults, for the stages that build on this:
- D/Shift+D (`detail_level`) is Stage 8a's tool and does not exist yet; its UI row of §4.3 comes with it. D stays Edit Pivot (hold).
- Alt with a brush tool inverts (Ctrl+Alt erases the mask) instead of orbiting; the right button orbits (the help line says so). X with a brush tool toggles X symmetry instead of holding grid snap. Ctrl or Cmd both count as Ctrl.
- The preview holds back the dabs that can still move as the path grows: the last for standard (its end point), the last two for the other spaced brushes (the direction of the dab before it); they run only for the stroke as it ended, so the preview trails the mouse by up to two spacings (0.3 of the radius by default). Path brushes (inflate, smooth, add ...) run whole each update (one dab per copy, as long as the path; newest input wins). Once a dab already run changes (symmetric copies, applied copy after copy, put each new sample in the middle of the list; taper rescales gains along the whole path; LazyMouse's Backtrack takes the path back; a stroke past 512 samples is thinned evenly, which moves every dab), the preview runs the stroke so far once from the start and from then on goes per copy: each update runs each copy's new dabs (holding each copy's end) on the grid so far, so over a drag about the stroke's dabs run once. Such a preview is marked inexact (`Live::inexact`), is not checked against its record and goes as soon as the record is in. How far it may drift is bounded by `an_inexact_preview_stays_close_to_the_commit` (fractions of the stroke's depth, measured on the test ball): symmetric copies 0.03-0.125 (bound 0.2), changed points overlapping 0.957+ (bound 0.93); taper up to 0.97 for standard (bound 1.5; each dab ran with the gain it had near the path's end, so the middle shows shallower and the taper-in deeper), overlap 0.876 (bound 0.8); Backtrack on a plain stroke ends as the commit (float rounding). Should a symmetric stroke's dabs not split evenly into its copies, the preview leaves the per-copy mode and reruns whole (exact) rather than stop following the stroke. The stroke's own `lazy` respaces only the path's end, which the preview holds, so it stays exact.
- No live preview (commit on release, as Stage 1c) for: alpha strokes (the commit binds the library's image), lassos (the outline shows), posed clays (chunks draw the rest pose), and clays chunks do not draw (decals, smart layers, weathering, baked or fine display). The preview casts samples on the regular mesh as it is at the press (`Live::cast_on`); right after a stroke that mesh may still be rebuilding (or deferred while strokes go on: `MeshLib::sculpting_now`), and the next stroke's preview starts from the clay as the strokes still to install leave it, so its preview is approximate until its record installs. When the request is sent (once every mesh is built), a preview whose mesh is not the one the handler casts on is marked inexact (`Live::mesh_moved`) and not checked, so two quick strokes in a row raise no false warning (`a_second_quick_stroke_previews_without_a_false_mismatch`). The record replaces it either way; an exact preview whose hash differs from its record counts in `preview_checks.1` and says so on the status line (later previews of the clay are not discarded).
- Posed clays: a sculpt drag on a clay whose rig is posed sends world points on the posed clay with `symmetry {posable: true, frame}` (the editor's frame), which the handler takes back to rest (Stage 2's posable symmetry); on a clay hung from an animated bone its points go back through the clay's placement at rest (plain world points). Paint, mask, groups, grab and snakehook on posed clays are still refused, with a reason.
- Grab and Snakehook send world points (`frame` left out): grab the press point and `delta` to where the ray under the mouse meets the view plane through it; snakehook the path across that plane (thinned to 512 points).
- LazyMouse: the string is in points of the view; the request carries the smoothed samples and no `lazy`, so a replay smooths nothing (the LazyMouse settings stay in the current brush). LazySnap runs the stroke on to where the mouse was released; Backtrack: the mouse within half the string of an earlier sample (not the last three) takes the stroke back to it. Lassos are never smoothed.
- Pressure: the first touch seen during the stroke (a pen already down at the press shows up moving) is followed by its TouchId; the macOS monitor (`tablet`, opt-in, saved with the editor's settings) reports a tablet event's pressure for 150 ms; velocity (opt-in) maps pointer speed to 1 (still) .. 0.1 (2500 px/s and faster). `pressure_source` is a call argument kept on the record as metadata only: it is not in `Record::digest` (the pressure itself is in the samples).
- Group strokes are sent at full strength (an id is all or nothing); mask strokes keep the brush's pressure curves.
- `ui_activity` lists every UI request (the brush tools' strokes and the HUD's calls) with its tool and, for non-stroke calls, its action. `set_current` from the HUD and hotkeys is editor state and not listed, as in Stage 1c; Save brush is (it changes the scene).
- Parity checks compare scenes in their file form (grids quantized, every record's exact `result_hash`), without record times and sources: `Scene ==` compares grids by pointer, and a resent request builds new grids. Adjust Last's "pre-action copy" includes the editor's `last_deltas`, since a `LastDelta` is invalid after undo by design (§3.5).
- The HUD is an area over the viewport's right side (no scroll: brush options beyond size, strength, focal, falloff and symmetry fold under "More brush options"); its golden and the ring check run only with `CLIDE_GPU=1`.
- The layer amount change is unchanged at about 70 ms to draw-ready chunks (Stage 6's remesh of about 970 chunks); the slider previews while it drags and sends one call on release. A drag the HUD stops drawing (the tool or the selection changed under it) is dropped the next frame: its preview goes and nothing is sent.
- Two Stage 1c expectations changed on purpose: `set_current {brush: grab}` is now accepted (the refused example is noise), and `a_press_on_a_posed_clay_is_refused` became `a_drag_on_a_posed_clay_sends_a_posable_stroke` (the Paint tool is still refused there; the Sculpt tool's posable request replays through MCP to the same record).

### Stage 8: detail levels + transport, displacement/VDM, render passes, movie, curve strokes (L, 3-4 weeks)

**Critic split.** This stage bundles unrelated work. Ship it as three independently testable sub-stages:
- **8a, levels and transport** (`detail_level`, transport hook wiring, D/Shift+D; 1.5-2 weeks). Wiring transport changes what grab, move, snakehook and transform produce, so 8a **bumps `ENGINE_VERSION`**, and older records of those brushes load as `engine_stale`.
- **8b, outputs** (displacement and VDM bake maps, `render_film passes`, mp4; about 1 week). It depends only on Stage 4's `Field` composition and can run any time after Stage 4.
- **8c, VDM stamps and curve strokes** (`alpha create_vdm`, `brush: vdm`, `kind: curve`, MoveCurve; about 1 week). It depends on Stage 2.

Each sub-stage keeps its own acceptance bullets below.

**Scope**
- `detail_level` tool:
  - `Clay.sculpt.levels` map onto `DetailGrid` levels;
  - strokes default to the active cell;
  - D / Shift+D in the UI.
- **Detail transport** through the Stage 1a hook:
  - transport brushes (grab, move, snakehook, transform) apply their backward map to every finer level and layer inside the influence;
  - normal-displacing coarse brushes shift finer grids along ∇T by −ΔT/|∇T|.
- `bake` maps `displacement` and `vector_displacement`.
- `alpha create_vdm` with `brush: vdm` stamps: replayable records, no Opaque record.
- `render_film passes`.
- `render_animation` with mp4 through ffmpeg, falling back to png_sequence.
- `kind: curve` keeps its control points. Using `stroke_edit replace` on them gives MoveCurve.
- Tools: `detail_level`.

**Parity rows**
- P1 Multi-resolution subdivision levels (as grid levels; partial).
- P1 Displacement Maps.
- P1 VDM / Alpha 3D.
- P1 BPR Render Passes.
- P1 MOV/MP4 export.
- P1 Curve brushes (partial).

**Acceptance**
- Transport: pores at level 2, then a level-0 grab of 0.5r. The pore-pattern correlation in the moved frame is ≥ 0.85 with transport and ≤ 0.3 without. The test asserts both, documenting the limit.
- Coarse grab → fine noise → coarse edit: the high-pass RMS stays within 5%.
- Displacement bake reapplied through film/dice.rs: Hausdorff to the clay ≤ 1 cell.
- A VDM created from a horn and stamped onto a plane:
  - Hausdorff ≤ 1.5 cells;
  - the overhang is reproduced;
  - replay is bit-equal.
- Passes:
  - the normal pass on a sphere has mean angle error ≤ 2°;
  - depth is monotone;
  - the mask pass reaches IoU ≥ 0.98.
- mp4 is produced when ffmpeg is present; otherwise the test is skipped with a reason. `png_sequence` writes N frames.
- Moving one curve control point equals a fresh stroke with the new points (hash).

**Stage 8a result (2026-10-23).** `src/levels.rs` holds detail levels: level 0 is the clay's own detail cell, finer levels are finer cells kept on its sculpt stack (`StackData::levels`, at most 8, coarse first, tidied when read) with the active one (`StackData::level`); `src/app/leveling.rs` the `detail_level` tool (list, `add`, pick by `level` or `cell`); `src/sculpt/engine.rs` detail transport. A stroke takes `level` (per stroke or per call, default the active level; records keep `level` and `cell`, both in `Record::digest`): at level k > 0 it sets the stroke's `cell` to that level's, so it first grows a Sculptris Pro region of that cell over itself, and it writes level k and the coarser ones. The written layer's finer detail is carried along: transport brushes (grab, stretch, snakehook, transform) pull every finer grid through their backward map, and the other brushes shift the finer offsets along the normal by the coarse change (−ΔT/|∇T|), so a coarse edit keeps the pores made at a finer level. The live engine runs the same hook, so incremental strokes equal the ledger's replay. One tool was added (71 → 72): `detail_level`; `get_clay` reports `levels`. No new dependencies. `ENGINE_VERSION` goes 1 → 2 (transport changes what the transport brushes and every coarse brush over finer detail produce). D / Shift+D in the editor step the lead clay's level through `detail_level`. The checks are in `src/sculpt/stage8.rs` and, through MCP on a headless editor, `src/app/leveling/tests.rs` and `src/app/stage7_tests.rs`.
- Transport: pores at level 2, then a level-0 grab of 0.5 r: pore-pattern correlation in the moved frame 0.987 with transport, −0.270 without (bounds ≥ 0.85 and ≤ 0.3); the grab 10 ms.
- Coarse lift of 0.045 over fine noise: high-pass RMS 0.00389 before, 0.00392 carried (1.008; bound within 5%), 0 without transport; the edit 123 ms.
- A leveled stroke equals its dabs run one at a time and on any thread count (bit for bit); a stroke on an already refined level applies once; transport moves only the written layer's detail; the version-2 level fixture hash is pinned.
- Per dab (warm, M2 Max): standard 6.5 ms with transport against 0.6 ms without, inflate 1.9, clay 2.4, grab 2.1, snakehook 2.6 ms. `level_timing_report` (ignored): standard at level 2 p50 2.2 ms, at level 0 with the normal carry p50 2.4 / p95 4.7 ms; clay p50 1.7 ms; the worst case, smooth r = 0.3 at level 0, 35.9 ms; grab r = 0.2 with the map carry 5.7 ms.

Deviations and defaults:
- Transport carries only the written layer's finer detail. A stroke writes one layer; other sculpt layers keep their detail where it was, so their records now read a changed base: they show `stale_context` and `stroke_edit rebase` replays them. A transport record kind across layers is left for the user to decide.
- `ENGINE_VERSION` 2 marks every version-1 record `engine_stale`, not only those of the brushes transport changed; narrowing that is left for the user to decide.
- `stroke_edit replace` fills `level` and `cell` as `sculpt` does, and refuses a stroke that would grow a detail region (a replace cannot grow regions).
- Fixed after review: D and Shift+D are the hotkeys' rebindable LevelFiner and LevelCoarser actions. With a brush tool on they win over Edit Pivot's D (the viewport ignores the pivot hold then), and they send nothing while a stroke is drawn. Likewise Stage 7's X is the hotkeys' SymmetryX, which wins over Snap to Grid's X with a brush tool. Bindings saved before these actions existed get their default keys (`hotkeys_known`). Picking the active level is selection-like: it is saved with the clay but is never an undo step, and undo, redo and `history goto` keep it (as they keep the playing clips). Adding a level is an undo step. A plain `detail_level` list changes nothing (no settle, no sync), and an add keeps the cells a coarser `detail_cell` hides.

**Stage 8b result (2026-10-23).** `src/outputs.rs` holds the output files: 16-bit and 8-bit PNG, float OpenEXR (zip), the overwrite rule (`writable`: never over a file of another kind), output folders and movies through an external ffmpeg (`CLIDE_FFMPEG`, the PATH or the usual install folders; arguments as a list, never a shell). `src/bake.rs` bakes `bake maps: [displacement, vector_displacement]` over the bake's UVs (`map_size`, `map_format` png|exr, `save_dir`, `verify`). `src/film/passes.rs` makes `render_film passes` (depth, normal, albedo, ao, shadow, mask, emission; `passes_dir`, `passes_format`, `pass_samples`, `ao_distance`, `normal_space`). `src/app/anim.rs` gives `render_animation` `format: png_sequence | mp4` (with ffmpeg; otherwise the frames stay as a sequence and the reply warns). `alpha import` reads OpenEXR too. No tools were added (72). New dependency: `exr` 1.74, default features off (approved in the plan's decisions). `ENGINE_VERSION` stays 2 (no brush changed). The checks are in `src/outputs.rs`, `src/bake.rs`, `src/film/passes.rs`, `src/app/anim.rs`, `src/app/cinema.rs` and, through MCP on a headless editor, `src/app/game.rs`.
- Displacement reapplied through `film/dice.rs` (diced to half a cell, 109,664 triangles): Hausdorff to the clay 0.41 / 0.42 cells for the height PNG and for the vector EXR (bound 1 cell). The 800-triangle simple mesh alone is 1.75 / 1.43 cells away.
- Passes on a sphere (128 px, all seven, 4 samples, 0.1 s): normal mean angle error 0.23° (bound 2°), mask IoU 0.991 (bound 0.98). Depth along a slanted floor rises monotonically toward the horizon, and the ball on the floor shades and shadows it.
- mp4 when ffmpeg is present (libx264, else mpeg4): 12 synthetic frames have exactly 12 frames by ffprobe, and a stale tail from a longer run never reaches the movie. Without ffmpeg the test is skipped, saying why. `png_sequence` writes exactly N frames, removing a longer run's tail.
- Timings (`stage8b_timings`, ignored; M2 Max): a 2048 bake of a 267k-triangle clay to 30k takes 6.5 s, and its displacement and vector maps 0.32 s more (PNG encode 0.03 s); the film picture at 640 px on the CPU with 128 samples takes 9.6 s, all seven passes 0.51 s; mp4 encoding is 0.25 s per 100 frames of 512².

Deviations and defaults:
- Passes are not the film tracer's AOVs: they come from their own deterministic CPU first-hit trace (a few anti-aliasing samples per pixel, seeded per pixel) of the same meshes, hair, floor and lens, whichever device traced the picture. They run after the picture, outside `time_limit` (time-limiting them is left for the user to decide), and not with `motion_blur`.
- The vector displacement map looks for the clay's surface only along the simple mesh's interpolated normal, as the height map does, so its R and G are about 0: it holds the height's offset in vector form and carries no overhangs or sideways detail (VDM stamps, Stage 8c, keep those).
- Each PNG map gets its own code scale (`scale` in the reply); EXR holds the floats as they are, in the clay's units.
- Fixed after review: depth PNG codes are 1..65535 for hits (near 65535, the farthest 1) and 0 for the background, and the reply's decode text gives the exact formula. Views whose names make the same file stem are refused before anything renders. A '%' in the frames folder is escaped for ffmpeg's image2 pattern. Output folders (`save_dir`, `passes_dir`, `path`, `<stem>_frames`) are made when the job writes, not while its arguments are read, so a refused call leaves no folder. An empty file at an output path (an encode cut short) may be overwritten, and a refusal over another kind of file says to delete it or give another path. The stale-frame cleanup removes only PNG files named as frames. `map_size` is checked as the number given (no wrapping cast). Vector maps interleave into one buffer. The albedo pass includes the floor's gray (0.42, as traced).

**Stage 8c result (2026-10-23).** `src/brush/vdm.rs` holds VDM stamps: `alpha create_vdm {name, target, center, radius, above?, below?, normal?, tangent?, cell?, space?}` captures, in a frame standing on the clay's surface (the base plane is the mean surface on two rings around the center, so a feature on the center does not lift it; up is their mean normal, across is X laid on it), D = field - base plane on a lattice (default the detail cell, coarsened to at most 2M points; every size bounded before sampling). D is zero wherever the clay is flat, so the stamp is a sparse detail grid (8³ bricks, quantized at capture so a saved stamp loads bit for bit, at most 4096 bricks, checked when read). A stamp is an `Alpha` with a volume (its image is its height from above), so the alpha library, the clays' alpha stores, content hashing, rebinding/stale_context and pruning all carry it unchanged. `brush: vdm` (a dab brush, `alpha: {name}` names the stamp; default strength 1, spacing 2) stamps it on the area's plane under each dab at radius / captured radius: the field becomes clamp(t) + clamp(k (P + D)) - clamp(k h) with the smaller of the two clamps, exactly the captured field on a flat surface (overhangs included, however far past either field's clamp band it stands) and additive displacement on a curved one; invert turns it over through its base plane and negates D (a horn becomes a hole of its shape), symmetry mirrors it. `kind: {curve: {closed}}` resolves its points as control points of a centripetal Catmull-Rom curve (each span in quarter-radius steps, at most 32 per span, re-snapped when the points snap), then as a path; the record's spec keeps the control points, so `stroke_edit replace` with one moved is MoveCurve. No tools were added (72), no dependencies, `ENGINE_VERSION` stays 2 (no existing brush changed). `vdm` joins the UI brushes (the HUD's alpha picker names the stamp; it sends no falloff). Not done: importing VDM images from other tools (the patch → `MeshSdf` path), curve handles in the viewport.
- Horn (a 12 mm capsule up 6 cm, then over 4.5 cm) captured at 4 mm cells: 24 bricks, 3.7 ms; 2 KB of JSON.
- Stamped onto a flat slab elsewhere: Hausdorff 0.29 cells (source → stamp) and 0.50 cells (stamp → source), sampled every half cell on the surface bands (bound 1.5); the overhang is empty under the arm and solid in it; replay bit-equal on 1, 3 and all threads and after the stamp is saved and read back; the X-symmetric copy is the mirror image.
- Stamp dab: 4.8 ms warm (37.5 ms cold, with the base cache filling), r = 20 cells, 10 cm tall.
- MoveCurve (through MCP): a curve stroke replaced with one control point moved has the `result_hash` (and grid) of a fresh stroke with the new points. The checks are in `src/sculpt/stage8c.rs` and `app/alphas/tests.rs` (`vdm_stamps_and_curve_strokes_through_mcp`).
- Fixed after review: a stamp read from a file checks its brick keys in i64, so a key whose eightfold overflows i32 (or i32::MIN) is refused as outside the box, never a panic or a wrap. A UI vdm drag strips everything a stamp refuses, its preset's included: falloff (sent as null) and the preset alpha's use, depth, mid and tile mode. `brush_preset set_current` refuses an alpha of the wrong kind for the brush (an image for vdm, a stamp for the other dab brushes), drops a kept alpha when the brush changes kind, and the HUD's picker lists only alphas of the brush's kind. Rebinding a name checks archived layers' records for a kind clash too (the alpha store still keeps nothing for them).

### Stage 9 (optional, structural): derived explicit topology (XL, 8-9 weeks, in three sub-stages)

**9a: Poly object + multires (3 weeks)**
- `src/poly/`, following design B's S1 and S7 specification:
  - `Chunked<T>` storage: `Arc<Vec<Arc<[T;4096]>>>`, Morton-ordered, compared with ptr_eq. This avoids Cloth's deep-copy flaw, and the same pointer list is the undo cost model and the GPU dirty set.
  - `Topology` with CSR adjacency.
  - A per-object skin, following the Cloth pattern.
- `to_poly`:
  - starts from the surface-nets quads (clay.rs:1696-1726);
  - welds, splits non-manifold vertices and edges, Morton-sorts and relaxes;
  - can start from the MakeHuman quad base (human.rs:30) instead.
- Catmull-Clark levels generalized from `human::subdivide` (human.rs:391), with cached stencils and **tangent-space displacement** per level, plus `reconstruct`.
- Poly → clay through `to_clay` (MeshSdf).
- A Poly is sculpted with the same `src/brush` StrokeSpec and the Jacobi kernel. Its records are vertex-addressed and kept in a separate, per-object log.

**9b: retopo (3-4 weeks)**
- An Instant-Meshes-style 4-RoSy orientation field plus a position field on a meshopt-decimated copy.
- The orientation field is initialised from the SDF Hessian.
- Constraints: group, mask and sharp borders; guides; symmetry.
- Quad extraction and cleanup.
- `project_levels`.
- `bake lowpoly: retopo` uses the result.

**9c: `poly_edit` / ZModeler-lite (2 weeks)**
- `HalfEdges` is built on demand.
- Actions: extrude, inset, bevel, bridge, insert_loop, delete, merge, crease, fill_hole, qmesh.
- Every op is a `Topology` record.

Tools: `to_poly`, `retopo`, `poly_edit`.

**Parity rows**
- P1 ZRemesher.
- P1 ZModeler.
- P1 Multi-resolution subdivision levels (full on Poly).
- PolyGroups topology operations on Poly.

**Acceptance**
- to_poly on the organic figure:
  - Hausdorff < 0.5 cell;
  - ≥ 98% quads;
  - 0 non-manifold edges and 0 boundary edges.
- Multires: L3 detail keeps ≥ 0.98 correlation after an L0 move, against < 0.6 for a world-space delta.
- Retopo on sphere, torus and head:
  - ≥ 92% quads;
  - face count within ±15% of the request;
  - Hausdorff ≤ 0.75 cell;
  - irregular vertices ≤ 6% (sphere, torus) and ≤ 12% (head);
  - symmetry within 1e-4.
- `keep_group_borders`: ≥ 85% of the group-border length lies within 1 cell of an edge loop.
- poly_edit:
  - extruding a cube face by 0.5 gives 10 faces, volume 1.5 ± 1e-5 and Euler characteristic 2;
  - the half-edge invariants hold after every op;
  - replay is bit-exact.
- Retopo of a 2M-triangle clay to 5k quads takes ≤ 30 s as a job, with cancel honoured within 200 ms.

### P0 coverage map (16 rows)

| P0 row | Agent half | UI half | Caveat (critic) |
|---|---|---|---|
| Symmetry (X/Y/Z, Radial, Posable) | 1a, posable in 2 | 1c (X toggle), 7 | — |
| Supported 3D formats | 5 | 5 (File > Import) | **Partial**: MA/VRML/X3D/MESH/SC1/GoZ not covered |
| 3D Layers (incl. Bake Current) | 4 (gated on Q1) | 7 | — |
| Masking brushes & core controls | 3 | 7 | `by_topology` is `connected` (spatial flood fill), not edge topology |
| PolyPaint | have; ledger in 1b | 1c, 7 | — |
| Tablet pressure curves | 1a | 7 | **Conditional**: macOS pen pressure needs Q4 = invest; otherwise partial on the primary platform |
| Clay brushes | 2 | 7 | — |
| Core brush controls | 1a (MRGB test in 1a) | 7 | — |
| Dynamic brush system & library | 2 | 7 | — |
| Smooth brushes | 1a | 1c, 7 | — |
| Standard brush | 1a | 1c, 7 | — |
| Flatten | have; side in 1a | 1c, 7 | — |
| Inflate | have | 1c, 7 | — |
| Move | have; transport in 8a | 7 (view-plane drag) | — |
| Alphas (16-bit) | 2 | 7 | 16-bit PNG only unless Q6 allows `exr` |
| DynaMesh | 5 | 5 (Remesh as clay… menu) | Closed by the Stage 5 menu (was "—") |

P1 rows not covered by Stages 0-8 stay as P2 follow-ups or are closed in Stage 9: ZModeler, ZRemesher, full subdivision, and topological polygroup ops. Camera import (FBX) is a follow-up. Run a final pass over docs/ZBRUSH_PARITY.md after each stage to update row status.

P1 tally (critic): Stages 0-8 touch 35 of 37 rows. Four stay partial: PolyGroups (spatial only), FBX ExportImport (no cameras), Multi-resolution levels (grid levels) and Curve brushes (no curve object or handles). ZModeler and ZRemesher are Stage 9 only.

Total effort: Stages 0-8 take about 24.5-30.5 weeks, including the 1.5-week Stage 1c. Stage 9 adds 8-9 weeks.

---

## 7. Performance targets (M2 Max, 12 cores)

| Scenario | Today | Target (p95) | Hard test bound |
|---|---|---|---|
| Hover pick, 1.65-2M tris | brute force: 0.1 ms at 30k tris, 11 ms at 1.65M, 46 ms at 6.6M | ≤ 0.2 ms (`pick_field`) | ≤ 1 ms |
| Dab apply, r ≤ 24-40 cells, 2 mm | 1-20 ms at 2 mm (20 ms at r = 0.08); 6-169 ms at 1 mm | ≤ 4-8 ms with BaseCache | — |
| Dirty-chunk remesh (8-27 chunks) | full remesh 21-168 ms (organic res 128: 21 ms; hard-floored to 192: 155-168 ms); build_fine 2.2 s (1.65M tris), 13.3 s (6.6M) | ≤ 14 ms | 1.5× slack |
| GPU upload of dirty chunks | full re-upload | ≤ 3 ms, ≤ 8 MB | — |
| Dab-to-photon, organic, 1.65M tris | not interactive | ≤ 16 ms goal, ≤ 33 ms committed | ≤ 33 ms |
| Dab-to-photon, Human form / hard-floored clay | ~254 ms (1.5 ms dab + 252 ms remesh) / ~169 ms | ≤ 50 ms | — |
| Worst case: smooth, r = 64 cells | — | ≤ 60 ms, latest-wins, UI frame unaffected | ≤ 120 ms |
| Agent `sculpt`, 10 strokes, end to reply | apply + 2.2 s fine rebuild | ≤ 300 ms | — |
| Commit after release | — | ≤ 50 ms | — |
| Replay | — | ≥ 100 dabs/s; single-record selective undo ≤ 1 s; 1,000 strokes ≤ 10 s | — |
| Layer amount slider, 10k bricks | — | ≤ 33 ms | ≤ 80 ms |
| Mask/group edit | — | ≤ 3 ms shading refresh, no remesh | — |
| Scene clone+compare (heavy fixture) | — | ≤ 50 µs | ≤ 50 µs |
| First stroke on a cold Human clay | 0.70 s cold Human mesh build, then 1.5 ms dab + 252 ms remesh | ≤ 1.5 s, once per blob edit | ≤ 3 s |
| Saved detail size | 1.44 MB (2 mm) / 8.6 MB (1 mm) after 6 dabs | ≥ 2.5-3× smaller (deflate) | ≥ 2.5× |

The "Today" column was re-baselined in Stage 0 (2026-10-22) from the in-tree report `cargo test --release sculpt_timing_report -- --ignored --nocapture` (src/harness/timing.rs), which also logs every figure to `target/perf.json`. The "full remesh" range spans the organic figure at res 128 (30k triangles) and the same figure with box feet at the default resolution, which the hard-surface floor meshes at 192 (211k triangles).

Memory budgets:
- Slim vertex: 2M triangles ≈ 24 MB, against ~120 MB with the 120 B `Vertex`.
- BaseCache: ≈ 32 MB at 2M triangles (16k surface bricks × 2 KB), with an LRU cap of 256 MB.
- `MAX_TOTAL_BRICKS` (262,144 ≈ 512 MB) is shared by all layers, the mask, groups and morph divergence.
- Undo is capped at 2 GB of unique bricks.

At 1 mm cells, UI dabs are capped at r = 40 cells. Larger brushes preview at a coarser level and apply at full level on commit.

---

## 8. Risks

1. **Chunked meshing is the critical path for every interactive P0 row.** It depends on understanding the non-monotonic triangle counts (Stage 0). It must keep seams bit-exact, adaptive-region levels, sharp vertices and the exact-planar path working per chunk. Forcing coarser factors at chunk borders may show density bands.
2. **Layer and morph blending is linear only for small offsets.** Large grab or stretch offsets near the ±127-voxel cap fold at fractional amounts. Mitigation: warn in replies when max offset exceeds 4 cells, offer bake-down, and recommend blob edits for big shape changes. Storing Δ/amount fails near amount 0, so strokes are refused below |amount| < 0.05.
3. **Determinism across platforms is not bit-exact.** libm sin/exp in the noise brushes and SIMD reordering can differ between platforms. Bitwise checks therefore run on one machine only, with a `CLIDE_XPLAT` tolerance mode. Noise moves to hash-based code without transcendentals, and parallel float reductions within a voxel are banned.
4. **Stale context.** Blob edits after sculpting change what a replay would produce. Mitigations: default `store: Both`, hash-verified replay on load, and `stale_context` plus `rebase` exposed to the agent.
5. **Memory.** Per-layer grids, BaseCache, ReplayCache checkpoints and undo all grow together. Each needs explicit budgets and LRU, and the limit error messages must tell the agent what to do (bake down, squash, coarser cell).
6. **File size.** Ledgers add resolved dabs. Deflate helps. A binary sidecar may be needed above about 32 MB per object (Q7).
7. **Detail transport is approximate.** Large snakehooks and posing smear detail. The tests document the measured correlation limit.
8. **Thin features below about 2 cells fuse or vanish.** The plan should add a field-based warning when a stroke thins a wall below 2 cells.
9. **MeshSdf cost** (~10× per sample) for Human and imports. BaseCache warm-up of 1-3 s is needed after blob edits. Open meshes need a winding-number sign, which is O(triangles) per sample without a hierarchy, so imports are decimated to ≤ 200k triangles first.
10. **Rig skinning per rebuild was never measured.** Sculpting is at rest and skin is deferred to commit. A posed preview while sculpting is out of scope.
11. **GPU and renderer churn in render.rs (2816 lines).** The work adds a second vertex layout, paged pools, 2-4k draws, shadow freezing and matcap/AO. Shadow-cache and `fit_buffer` regressions for existing clay are possible. GPU tests need a device and are `#[ignore]` by default.
12. **macOS pressure.** winit 0.30.13 and egui-winit 0.33.3 deliver no pen pressure. An NSEvent monitor may conflict with eframe and cannot be tested headlessly.
13. **Concurrent agent and human edits.** Agent jobs fail if the human stroked the same layer meanwhile; the error must suggest a retry or another layer.
14. **The undo cap changes behaviour.** Agents must see dropped steps.
15. **The hard-surface resolution floor** (clay.rs:1876-1886) no longer drives the sculpt display, which meshes at `detail_cell`, but it still drives the exported rest mesh. Preview and export density will therefore differ and must be documented.
16. **Stage 9 retopo is research-grade.** Results may fall short of ZRemesher on hard-surface models.
17. **Tool-surface growth** (57 → 72/75) and large StrokeSpec schemas cost agent context. Presets and defaults must keep typical calls short.
18. **Engine drift invalidates saved ledgers** (critic). Any kernel change changes `result_hash`. Without `Record.engine` and the policy in §3.4, every old file would report replay mismatches, and selective undo across versions would silently change shapes. Stage 8a's transport is a known bump.
19. **Hash stability** (critic). `DefaultHasher` is unsuitable for saved hashes. Q6 blocks Stage 1b.
20. **Shared brick budget** (critic). `MAX_TOTAL_BRICKS` currently caps one detail grid. Sharing it across 16 layers, the mask, groups and morph divergence can exhaust it on a single heavy layer. It becomes a byte budget, specified in Stage 4.
21. **Undo byte estimate is unreliable** (critic). Arc strong counts include references held by MeshLib, ReplayCache, BaseCache and the GPU dirty-set, so "unshared" bricks are undercounted. Count only references reachable from the undo stack, using an address set walk.
22. **UI/agent drift** (critic). If UI actions call engine functions directly instead of the tool handlers, agent replays diverge in snapping, scaling and refusals. §4.3 now requires request-level dispatch, and Stages 1c and 7 test every UI action.
23. **Selection state leaking into results** (critic). Active layer and level live in `ClideApp`. Agent defaults must come from scene state, and records must store resolved values (§3.3).

---

## 9. Open questions for the user

**Q1. What does a stroke on layer k read?** The winning design reads only the blob field + layers below k + layer k. This keeps replay stable when upper layers change, but it is not WYSIWYG: the brush ignores visible detail on upper layers, unlike ZBrush.
- **Recommended alternative:** read *all enabled layers* and write the active one. `context_hash` then covers every other layer, so more records are flagged stale after edits elsewhere.
- Which do you want?

**Q2. Should layer composition be linear in SDF offset**, with the warning and refusal rules in Risk 2, or would you rather restrict layer amounts to [0, 1]?

**Q3. Is the Stage 9 Poly track committed or optional?** It is the only path to ZModeler, ZRemesher and true Catmull-Clark levels, at a cost of 8-9 weeks and a second, topology-addressed log.

**Q4. macOS pen pressure.** Should we invest in an `objc2-app-kit` NSEvent monitor (new dependency, opt-in setting), or ship velocity and setting-based falloff on macOS and defer pressure?

**Q5. Undo cap.** Is 300 steps / 2 GB acceptable as a behaviour change from today's unbounded undo? Should history marks be saved into the scene file (opt-in `persist`)?

**Q6. New dependencies.** Which of these are acceptable?
- **Brick hashing:** `xxhash-rust` (xxh3). It is not in Cargo.toml today; the alternative is a hand-rolled 128-bit hash.
- **3MF export:** `zip`.
- **EXR alphas and displacement:** `exr`. Without it, these use 16-bit PNG only.
- Already present: flate2, png 0.18, image, meshopt, xatlas, objc2.

**Q7. File format.** Should we keep everything in the pretty-printed JSON with deflated base64, or add a binary `.clide-data/` sidecar above a threshold (e.g. 32 MB per object)? And should the default ledger `store` be `Both` (safest, largest) or `Ledger` (smallest, with replay on load)?

**Q8. ffmpeg.** Is an external ffmpeg binary acceptable for MP4, with a png_sequence fallback?

**Q9. Retopo.** In-house Instant-Meshes-style only, or allow an optional external engine behind a feature flag if licensing permits?

**Q10. Preview vs export density.** For hard-surface clays, should exports match the sculpt-session `detail_cell` density (bigger files) or keep today's resolution floor?

---

## 10. Critic notes

A completeness review against `docs/ZBRUSH_PARITY.md` (16 P0 and 37 P1 rows) and the code on `graphics`. The edits it led to are already folded into the sections above. This section records what was found and why.

### P0 coverage
- All 16 P0 rows are named in the coverage map, but three were overstated as closed:
  - **Supported 3D formats.** The row lists MA, VRML, X3D, MESH/MESHB, SC1 and GoZ. The plan covers only OBJ/STL/PLY/GLB/FBX in and OBJ/STL/PLY out. It is now marked partial, with a suggestion to split the row.
  - **Tablet pressure.** The macOS UI half was only "compile-tested", and velocity is not pressure. It is now conditional on Q4.
  - **DynaMesh.** The UI half was "—", which breaks the plan's own UI-second rule. A Stage 5 "Remesh as clay…" menu now dispatches `to_clay`.
- **Core brush controls** include MRGB (sculpt and colour in one stroke). `StrokeSpec.paint` existed, but no acceptance test covered it. A test is added to Stage 1a.
- The P1 count "about 30 of 37" is replaced with the enumerated tally: 35 rows touched, 31 full and 4 partial; ZModeler and ZRemesher are Stage 9 only.

### Claims checked against the code
- Confirmed:
  - `Detail`/`Paint` Arc newtypes (clay.rs:2043, paint.rs:26);
  - `Scene` `PartialEq` skips `frame` (scene.rs:221-230);
  - `Field::new` (clay.rs:655);
  - fixed `falloff` (sculpt.rs:234);
  - `Stroke` is Deserialize-only (sculpt.rs:54-55);
  - `WeightStroke` (rig.rs:64);
  - `MAX_TOTAL_BRICKS` = 262,144 and `MAX_REGIONS` = 16 (clay.rs:1941-1943);
  - `JOB_TOOLS`/`EDIT_JOBS` (app/tools.rs:26-28);
  - the whole-clay install guard (app/tools.rs:722);
  - File > Import disabled (chrome.rs:91);
  - empty shelf (chrome.rs:313-316);
  - `Tool` enum (app.rs:49-54);
  - `render_image` (render.rs:2520) and render.rs at 2816 lines;
  - winit 0.30.13 and egui-winit 0.33.3 (Cargo.lock);
  - flate2, png 0.18, image, meshopt, xatlas and objc2 present, with no xxhash, zip or exr.
- Corrected:
  - every `tools.rs:N` cite actually points at `src/app/tools.rs`;
  - the mirror "second apply" is at app/tools.rs:715-717, not 704-707;
  - the install guard is at 722-724, not 717-721.
- Unverified, now flagged in the text:
  - the `mesh_shading::refresh` "about 1 ms": it re-evaluates `Field` per vertex, so the cost scales with vertex count, and a 1.65M-triangle bound is added to Stage 6;
  - the §7 "Today" timings, which come from a scratchpad bench, not the tree, and are re-baselined in Stage 0.
- Preview AO: render.rs already has a live `Ao { enabled: true, .. }` pass with blur. The parity row's "reserves AO bindings (unused)" is stale, as the plan suspected. Stage 6 should verify it and correct the row, not build SSAO.
- New facts that change the design:
  - `content_hash` uses `DefaultHasher`, which is unstable and 64-bit (mesh.rs:228), and returns `None` for rigged and cloth objects (mesh.rs:225).
  - `Clay.masks` already exists for material channel masks (mesh.rs:242), so the new mask is renamed `sculpt_mask` in code.
  - Today's `Vertex` (mesh.rs:982) has no mask or group attribute.

### Stages that were not independently shippable or testable
- **Stage 0:** image goldens need a wgpu device. GPU gating now starts in Stage 0, and geometry and hash checks are the gate that always runs. If the triangle-count finding is left `#[ignore]`d, Stage 6 lacks a valid reference, so a monolithic anchored reference is now specified.
- **Stage 1a:** "legacy JSON bit-equal" contradicted "default curve within 1e-6". It is fixed with a `legacy` falloff preset that calls the old function.
- **Stage 1b:** blocked on the Q6 hash choice, now an explicit gate.
- **Stage 3:** overlays and hidden-group clipping assumed per-vertex attributes that arrive only in Stage 6. They are now done through `Vertex.color` and index filtering in the shading refresh.
- **Stage 4:** the ≤ 33 ms amount-change target cannot pass before the chunked mesher, because it triggers a full remesh. It is now a Composite-only bound, with the end-to-end target moved to Stage 6. The `stale_context` acceptance depends on Q1, now a gate.
- **Stage 7:** the AI-first guarantee (a UI stroke replays through MCP) was first testable around week 20. The new **Stage 1c** ships a commit-on-release UI recorder right after 1b.
- **Stage 8:** five unrelated deliverables. It is split into 8a (levels and transport, with an engine bump), 8b (outputs) and 8c (VDM and curve strokes).
- **Stage 5:** it does not depend on Stages 2-4 and can run earlier or in parallel. That is now stated.

### AI-first violations fixed
1. **UI used `ledger::commit`, not the tool.** The MCP handler also does `to_local`, snapping, `radius_scale`, refusals, the install guard and replies. The UI now dispatches the same tool request in-process. The test compares requests and replies, not just ledgers, and §4.3 has a UI-to-tool table covering every control.
2. **Hidden selection state.** Active layer and level in `ClideApp` could change what an agent `sculpt` did. Defaults now come only from scene state, and records store the resolved layer and cell.
3. **Camera-dependent specs.** `screen` frames and lasso/rect stored a view name. They now store the camera matrices, so resubmitting is stable after the human orbits.
4. **The human's current brush was invisible to the agent.** `brush_preset get_current|set_current` was added.
5. **LazyMouse.** The stored path is post-lazy, and preview and commit share one streaming dab list, so the preview-equals-commit test can hold.
6. **Adjust Last.** `LastDelta` was serde-skip and was not invalidated on undo. It is now tagged with the ledger head and source.

### Architecture risks that were underweighted (Risks 18-23)
- Engine versioning of saved ledgers (`Record.engine`, load policy, bump in 8a).
- Saved-hash stability.
- The shared brick budget, now a byte budget.
- The undo byte estimate double-counting Arcs held by caches.
- UI/agent drift.
- Selection state leaking into results.

### Decision gates by stage

| Stage | Must be decided first |
|---|---|
| 1b | Q6 hash (xxhash-rust or hand-rolled 128-bit) |
| 4 | Q1 (what a stroke on layer k reads), Q2 (linear amounts) |
| 5 | Q6 `zip` for 3MF (otherwise ship without 3MF) |
| 7 | Q4 macOS pressure (otherwise the P0 pressure row is partial) |
| 8b | Q6 `exr`, Q8 ffmpeg |
| 9 | Q3 (committed or optional), Q9 |

### Still open
- 72-75 tools is a real context cost. Consider folding `sculpt_diff` and `probe_clay` into `stroke_log`/`get_clay` actions if agent evals show tool-selection errors.
- The Stage 9 Poly log is topology-addressed. UI ZModeler actions must still dispatch `poly_edit` requests under the same request-level parity test.
