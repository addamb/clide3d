# ZBrush parity roadmap for Clide3D

Every ZBrush feature found in Maxon's product pages, documentation, release notes (2021-2027) and ZBrush for iPad (389 features), checked against Clide3D's code and MCP tools. Built 2026-10-21 by a multi-agent research workflow; each feature links its source.

**Status:** 79 have · 129 partial · 146 missing · 35 not applicable (recounted from the table rows after sculpt Stage 8; the research pass found 49 · 148 · 157 · 35).  
**Priority:** 16 P0 (core sculpting) · 37 P1 · 96 P2 · 240 P3 (niche). Effort: S/M/L/XL.

Clide3D is AI-first: every gap is described as an MCP tool first, with mouse/tablet UI built on the same data.

## How Clide3D differs

I checked all 389 catalogue features (368 base plus 21 critic additions) against the 56 MCP tool schemas in src/mcp.rs and the modules behind them. Sources read: sculpt.rs Brush enum and Mask, clay.rs/hard_surface.rs blob forms and ops, paint.rs, decal, adaptive.rs, bake.rs (meshopt + xatlas), hair.rs, cloth.rs (XPBD), rig/pose, film/, render.rs, app.rs (Tool enum: Select/Move/Rotate/Scale, undo: Vec<Scene>) and app/viewport.rs (transform_gizmo). Nothing was edited and no cargo or git commands were run.

Clide3D's model is very different from ZBrush's. ZBrush edits polygon meshes. Clide3D's clay is a signed-distance field made of blobs, with a voxel detail layer on top, meshed again on demand. In practice:
- Things it already does in its own way: DynaMesh-style uniform remeshing, Live Boolean (op add/carve within one clay), Sculptris-Pro/HD-style local density (detail_cell and detail_regions), shell hollowing, curve sweeps (sweep/extrude/lathe/chain), and arrays.
- Things it cannot do yet: anything that needs explicit topology. That covers ZModeler, ZRemesher quad retopology, true subdivision levels, edge loops and UVs on arbitrary meshes. (PolyGroups, morph targets and detail levels came later in clay-native form: sculpt Stages 3, 4 and 8a.)

Strengths at parity or better:
- The film path tracer (DOF, physical camera, HDRI, SSS, glass, fog, bloom, grade, denoise, GPU/CPU)
- PBR and smart layered materials with procedural masks
- Paint (UV-free polypaint) and decals
- The bake pipeline (decimate, auto-UV, color/normal/AO/ORM, LODs)
- Rigging, posing and keyframes, human base, cloth draping, strand hair
- Kitbash/scatter, which cover InsertMesh, NanoMesh and ArrayMesh
- Agent automation through MCP, which exceeds ZScript/Python

Largest P0 gaps:
1. Interactive sculpting (sculpt Stage 7): the Sculpt, Paint, Mask and Groups tools preview the stroke while it records (a worker folds the stroke so far into a copy of the clay, drawn in chunks) and send the same request an agent would on release; pen pressure (touch force, the opt-in macOS tablet monitor, or pointer velocity), LazyMouse, the brush HUD and its hotkeys are in. (Agent strokes have falloff curves, pressure, spacing, stroke kinds, symmetry and LazyMouse since sculpt Stage 1a.)
2. Alphas shape agent strokes (16-bit PNG library, stamp/drag/tile, height or mask; sculpt Stage 2), and import reads OpenEXR since Stage 8b, but there is no alpha editing yet, and alpha strokes commit on release without a live preview (Stage 7).
3. Persistent sculpt masks and polygroups (sculpt Stage 3) were agent-only at first (the mouse painted the mask with the MaskPen preset); Ctrl-paint, Ctrl+Shift lasso, the Mask tool and the Groups tool in the viewport came with Stage 7.
4. Mesh import covers OBJ/STL/PLY/GLB/FBX (meshes only) and print export OBJ/STL/PLY/3MF (sculpt Stage 5); MA, VRML, X3D, MESH/MESHB, SC1 and GoZ are not read.
5. Sculpt and paint layers, morphs (exported as blend shapes) and undo-history recall are in (sculpt Stages 1b and 4).
6. No quad retopology.
7. No UV editor (3D-print output came with sculpt Stage 5; MatCaps with sculpt Stage 6; movies, render passes and displacement / vector-displacement maps with sculpt Stage 8b).

Suggested order:
1. Persistent mask channels and a mask MCP tool.
2. Brush falloff and alpha parameters on sculpt strokes.
3. Sculpt layers with per-layer amounts (which also give morphs, contrast and adjust-last).
4. OBJ/STL/GLB import, followed by a polygon-mesh sculpt path.
5. Interactive viewport brush UI that emits the same Stroke structs.
6. Quad remesher.
7. Deformer tool and render AOVs/movie.

iPad-only items are marked not_applicable (P3). They map to touch, Pencil and plan features Clide3D doesn't target.

## P0

### Characters & posing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Symmetry (X/Y/Z, Radial, Posable)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/symmetry/symmetry.html) | 🟡 partial | S | sculpt/paint stroke symmetry {axes: [x,y,z], radial {axis, count}, about pivot\|origin, posable} (src/brush/symmetry.rs; mirror:true = axes [x]); posable (sculpt Stage 2): points on the posed clay go back to rest through inverse LBS, are mirrored there and sculpt the rest pose; blob/rig mirror; mirror_object any plane | Add radial and posable UI toggles (the Sculpting shelf has X since sculpt Stage 1c and the brush HUD X/Y/Z since Stage 7; a Sculpt drag on a posed clay sends posable strokes since Stage 7). |

### Import/export & interop

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Supported 3D formats (OBJ, MA, FBX, STL, VRML, X3D, PLY, MESH/MESHB, SC1, GoZ)](https://www.maxon.net/en/zbrush/features/supported-formats) | 🟡 partial | L | MCP import_mesh {path: obj\|stl\|ply\|glb\|fbx, as: clay\|mesh} (src/mesh_import/, sculpt Stage 5; File > Import… sends it); export GLB/FBX, export_print OBJ/STL/PLY/3MF; scene JSON | The P0 bar is this subset. MA, VRML, X3D, MESH/MESHB, SC1 and GoZ (proprietary) are not covered: move them to P2/P3. |

### Layers & morphs

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [3D Layers (incl. Bake Current)](https://www.maxon.net/en/zbrush/new) | ✅ have | L | sculpt and paint layers (src/layers.rs, sculpt Stage 4): MCP sculpt_layer {add, list, select, amount, enable, disable, rename, move, bake (Bake Current), merge_down, delete, split_by_mask, record}; each layer has an amount (-2..2) and its own stroke ledger; strokes read every layer and write theirs (sculpt/paint/stroke_log/stroke_edit/sculpt_diff take layer); meshes, bakes, films and exports read the composite; the brush tools' HUD lists the layers with an amount slider (previewed while it drags, one sculpt_layer amount on release), eye, record dot and select, each a sculpt_layer call (sculpt Stage 7) | Done. |

### Masking & selection

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Masking brushes and core controls](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/masking/masking.html) | ✅ have | L | persistent sculpt mask (src/mask.rs, sculpt Stage 3): MCP mask {paint (mask_paint strokes, MaskPen), lasso/rect through a stored camera, invert, clear, all, grow, shrink, blur, sharpen, by_cavity, by_curvature, by_normal, by_height, by_blobs, by_groups, connected, from/to_channel}, each a mask-ledger record; every sculpt and paint stroke moves by 1 - mask; viewport overlay; per-stroke mask {blobs, above, below, channel, invert_channel}; Ctrl-paint, Ctrl+Alt erase, Ctrl+Shift lasso, the Mask tool and HUD buttons (invert, clear, grow, shrink, blur) send mask calls (sculpt Stage 7) | UI half done (sculpt Stage 7). by_topology is connected (a spatial flood fill), not edge topology. |

### Polypaint & texturing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [PolyPaint](https://www.maxon.net/en/zbrush/features/polypaint) | ✅ have | M | MCP paint (UV-free color on clay detail grid; src/paint.rs); Paint tool records a mouse drag and sends the same paint request on release (sculpt Stage 1c); live preview while the drag records, pen pressure (sculpt Stage 7) | Done. |

### Sculpting brushes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Tablet pressure curves](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/tablet-pressure/tablet-pressure.html) | ✅ have | M | agent strokes take per-point pressure ([x,y,z,pressure] or {p, pressure}) mapped through pressure {size, strength} curves (src/brush/pressure.rs); the brush tools capture pen pressure per sample: a touch's force (matched by TouchId), the macOS tablet monitor (opt-in objc2-app-kit NSEvent local monitor, src/app/pen_macos.rs) or pointer velocity (opt-in), mapped through the brush's pressure curves; records keep pressure_source (sculpt Stage 7) | macOS monitor is compile-tested only (no headless tablet). |
| [Clay brushes (Clay, ClayBuildup, ClayTubes)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ✅ have | M | sculpt brushes clay (fills toward the area plane, square footprint), clay_buildup (fills, then builds a layer per stroke), clay_tubes (even square strip along the path), all taking alphas as masks (src/sculpt/brushes.rs, sculpt Stage 2); offered by the Sculpt tool and as presets; live preview and pressure (sculpt Stage 7) | Done. |
| [Core brush controls (Draw Size, Focal Shift, ZAdd/ZSub, Z Intensity, MRGB)](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-user-guide.html) | ✅ have | M | sculpt/paint stroke radius, strength, invert, falloff presets/curve with focal_shift, MRGB (sculpt stroke paint: {color}) (src/brush/); HUD: size, strength, focal shift (S, U, O drag hotkeys), falloff presets and a curve editor, alpha, symmetry X/Y/Z; a depth-tested ring on the surface with a focal ring and normal tick (sculpt Stage 7) | Done. |
| [Dynamic brush system & brush library](https://www.maxon.net/en/zbrush/features/brush-system) | ✅ have | XL | MCP sculpt (src/brush/spec.rs Brush enum: 21 sculpt kinds incl. clay, clay_buildup, clay_tubes, trim_dynamic, polish, snakehook, transform; mask_paint and group_paint for the mask and group tools); brush presets (src/brush/preset.rs): 21 built in (MaskPen since sculpt Stage 3), scene presets via brush_preset list/get/save/delete, sculpt preset per call or stroke (sculpt Stage 2); the Sculpting shelf picks presets through brush_preset set_current; live preview, preset buttons on the shelf, Save brush in the HUD (brush_preset save) (sculpt Stage 7) | A thumbnail brush picker (the shelf's preset list and buttons stand in). |
| [Smooth brushes (Smooth, Smooth Peaks/Valleys, SmoothAlt)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/smooth/smooth.html) | ✅ have | S | sculpt brush smooth with smooth_mode gentle\|normal\|peaks\|valleys; Shift held while sculpting sends smooth (sculpt Stage 7) | Done. |
| [Standard brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ✅ have | S | sculpt brush standard: area-normal displacement dab with falloff curve, invert carves, spacing/pressure/symmetry/every stroke kind, alpha height stamps (src/sculpt/); live preview (sculpt Stage 7) | Done. |
| [Flatten brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ✅ have | S | sculpt brush flatten (plane normal optional); Sculpt tool on release (sculpt Stage 1c); live preview (sculpt Stage 7) | Done. |
| [Inflate brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ✅ have | S | sculpt brush inflate / deflate; Sculpt tool on release (sculpt Stage 1c); live preview (sculpt Stage 7) | Done. |
| [Move brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ✅ have | S | sculpt brush grab with delta (src/sculpt.rs Brush::Grab); Grab (Move preset) drags across the view plane at the press's depth and sends the press point and delta (sculpt Stage 7) | Optionally a falloff curve parameter. |

### Strokes & alphas

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Alphas (16-bit)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/alphas/alphas.html) | 🟡 partial | M | MCP alpha {import (16/8-bit PNG, JPEG), from_view (height capture), list, delete, preview}; stroke alpha {name\|hash, depth, mid, rotate, scale, mode stamp\|drag\|tile, use height\|mask} on dab brushes, bilinear in the dab frame; records bind by content hash and clays keep their images (src/brush/alpha.rs, sculpt Stage 2) | Paint and mask_paint strokes take alphas too (sculpt Stage 3); alpha import reads OpenEXR (Y channel, sculpt Stage 8b). Add an alpha picker UI. |

### Topology & remeshing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [DynaMesh](https://www.maxon.net/en/zbrush/features/base-mesh-creation) | ✅ have | M | MCP to_clay {target, mode: volume, resolution, blur, polish} remeshes any object (imports included) into one closed clay surface (src/to_clay.rs, sculpt Stage 5); Modify > Remesh as Clay… sends it | None. |

## P1

### Deformers & gizmo

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Gizmo 3D](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/gizmo-3d.html) | ✅ have | M | transform_gizmo in src/app/viewport.rs (W/E/R, X snap, D pivot); MCP update_object/set_pivot; masked sub-object transform (sculpt Stage 3): sculpt brush transform {translate, rotate, scale, pivot \| matrix} moves each point by 1 - mask, and the gizmo on a masked clay sends it on release | Live preview of the masked drag (Stage 6/7). |

### Hard surface & modeling

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [ZModeler brush](https://www.maxon.net/en/zbrush/features/polygon-modeling) | ❌ missing | XL | parametric SDF forms extrude/lathe/sweep/array (src/hard_surface.rs) | There is no polygon box-modeling. Add a Poly object type and MCP poly_edit {action, target: faces\|edges\|points\|loop\|island}, a half-edge mesh with extrude, inset, bevel, bridge and loop. Then add a hover-target UI brush. |
| [Live Boolean](https://www.maxon.net/en/zbrush/features/flexible-workflows) | ✅ have | M | blob op add/carve/intersect, live and non-destructive in one clay; MCP to_clay {targets, op: add\|carve\|intersect, into} across objects (sculpt Stage 5) | None. |

### Import/export & interop

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [3D Print Hub](https://www.maxon.net/en/zbrush-for-ipad-key-features) | ✅ have | S | MCP export_print {path .stl\|.obj\|.ply\|.3mf, size_mm, hollow, fine}: welded, millimeters, Z up on the plate, watertight and non-manifold report (sculpt Stage 5); File > Export for Print… | None. |
| [FBX ExportImport (mesh, camera, images)](https://www.maxon.net/en/zbrush/features/interoperability) | 🟡 partial | L | export_fbx (mesh, skin, clips; src/fbx.rs); import_mesh reads binary FBX geometry (sculpt Stage 5) | Add camera import/export and FBX model transforms on import. |

### Insert / array / nano meshes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [InsertMesh brushes](https://www.maxon.net/en/zbrush/features/brush-system) | ✅ have | M | MCP kitbash place (parts snapped onto surfaces), scatter; kitbash part {object, op: add\|carve} inserts any object into clay (sculpt Stage 5) | None. |

### Layers & morphs

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Morph Targets](https://www.maxon.net/en/zbrush-plan-options) | ✅ have | M | MCP morph {store, switch, blend, delete, list, diff} (src/layers.rs, sculpt Stage 4): shared snapshots of the sculpt layers (grids, ledgers, amounts); export_glb / export_fbx blend_shapes [morph:NAME, layer:NAME] as glTF morph targets and FBX BlendShape channels, fitted to the exported vertices (refused with a residual when they cannot follow); check_export morph_weights | Add the Morph brush (P2) and a UI morph list (Stage 7). |

### Masking & selection

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [BlurMask / SharpenMask / GrowMask / ShrinkMask](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/masking/masking.html) | ✅ have | S | mask {grow, shrink, blur, sharpen} on the persistent mask, walking the surface band (blur keeps the mask's sum; sculpt Stage 3); HUD buttons (sculpt Stage 7) | Done. |
| [Brush Auto Masking](https://www.maxon.net/en/zbrush-plan-options) | ✅ have | M | stroke mask {backface, cavity -1..1, normal_angle, connected, groups, blobs} (sculpt Stage 3), read from the surface around each dab | UI toggles (Stage 7). |
| [Mask By Cavity](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/masking/masking.html) | ✅ have | S | mask {by_cavity {radius}} and {by_curvature} with the smart materials' cavity estimate (sculpt Stage 3) | A cavity curve (a falloff-style profile) instead of the fixed ramp. |

### Polygroups

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [PolyGroups](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/polygroups/polygroups.html) | 🟡 partial | M | spatial polygroups on the surface band (src/groups.rs, sculpt Stage 3): MCP group {from_blobs, from_mask, from_normals, paint, rename, show, hide, isolate, show_all, clear, list}; brushes skip hidden groups, strokes keep to mask.groups, mask by_groups; set_display overlay groups | Spatial labels only: no group edges or loop operations (Stage 9). |

### Polypaint & texturing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Displacement Maps](https://www.maxon.net/en/zbrush/features/map-generation) | ✅ have | M | bake maps: [displacement] (sculpt Stage 8b): height of the clay (blobs + every layer) along the simple mesh's normal over its UVs, 16-bit PNG (mid grey = 0, scale in the reply) or 32-bit EXR, written to save_dir; verify reapplies it through film/dice.rs and reports the error in cells | Scalar maps need the clay to be a height field over the simple mesh: steep detail a coarse budget cuts through shows as a larger verify error (raise triangles). |

### Rendering

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [BPR Render Passes](https://www.maxon.net/en/zbrush/features/best-preview-render-bpr) | ✅ have | M | render_film passes: [depth, normal, albedo, ao, shadow, mask, emission] (src/film/passes.rs, sculpt Stage 8b): first hits of the film scene on the CPU, anti-aliased, 16-bit PNG / 8-bit sRGB or EXR in passes_dir, decode notes in the reply | No SSS or floor pass; still pictures only (not with motion_blur). |
| [MOV / MP4 movie export](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ✅ have | M | render_animation format: png_sequence \| mp4 (sculpt Stage 8b): every frame streamed to disk (up to 1200, 1024 px); mp4 through an external ffmpeg (PATH, Homebrew folders or CLIDE_FFMPEG; arguments as a list, cancellable), falling back to the PNG sequence with a warning | Viewport quality only (film-quality movies would be one render_film per frame). No MOV. |
| [MatCaps & MatCap Maker](https://www.maxon.net/en/zbrush/features/custom-materials) | 🟡 partial | S | set_display {shading: matcap, matcap: clay\|red_wax\|skin\|metal\|image path} shades every fill with a matcap, creases darker on sculpted clay (sculpt Stage 6, src/render/matcap.rs); Display > MatCap sends it | MatCap Maker: capture a matcap from a lit sphere render (render_film of a sphere to a matcap image). |
| [Preview AO & Raytraced AO](https://www.maxon.net/en/zbrush-plan-options) | ✅ have | M | viewport SSAO (pre-pass, 12-tap occlusion, depth-aware blur; render.rs), set_display ambient_occlusion / ao / ao_radius (sculpt Stage 6), checked by ao_darkens_creases_but_not_open_floor; render_film traces its own AO and GI | None. |

### Sculpting brushes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Custom brushes (Save As, Load, LightBox Brushes, cross-device)](https://help.maxon.net/zbr/en-us/Content/html/zbrushcore/reference-guide/brush/brush.html) | ✅ have | S | brush_preset list/get/save {spec \| from_record}/delete: JSON stroke settings (alpha refs included) saved in the scene; the Sculpting shelf's preset dropdown (sculpt Stage 2); shelf preset buttons and the HUD's Save brush (sculpt Stage 7) | A thumbnail picker. |
| [VDM / Alpha 3D brushes](https://www.maxon.net/en/zbrush/features/brush-system) | ✅ have | L | alpha create_vdm {target, center, radius, above, below, normal, tangent, cell} captures a clay's surface around a point as a sparse volume (what stands out of or sinks into the base plane, overhangs included; src/brush/vdm.rs), kept in the alpha library and the clays' alpha stores by content hash; sculpt brush vdm stamps it per dab (size from the radius, rotate, scale, stamp/drag modes, invert carves it, symmetry mirrors it), as a replayable stroke record; the HUD offers vdm with the alpha picker (sculpt Stage 8c) | Importing VDM images from other tools (a patch through MeshSdf) is not in; stamps are additive displacement on curved surfaces (exact on flat ones). |
| [Brush Edit Curve / Curve sub-palette (AccuCurve, WrapMode, CurveByPen, ZeroCurve, PenCurve)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/curve/curve.html) | 🟡 partial | S | stroke falloff: presets or up to 16 [t, value] points (monotone cubic) plus focal_shift, shared by sculpt and paint; pressure curves (src/brush/curve.rs); the HUD's falloff curve editor (drag the points) (sculpt Stage 7) | Add WrapMode. |
| [Curve brushes (Curve, CurveMesh, CurveTube, CurveSurface, MoveCurve, Curve Flat/Flat Snap)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/curve-brushes/curve-brushes.html) | 🟡 partial | M | clay blob sweep {profile, points, taper, twist}, chain, capsule; hair cards; any sculpt, paint or mask stroke takes kind {curve: {closed}}: a smooth (centripetal Catmull-Rom) curve through its control points, snapped onto the surface when its points snap, and the record keeps the control points, so stroke_edit replace with one moved is MoveCurve (sculpt Stage 8c) | Missing: curve-flat ribbons/cards on arbitrary objects, and an editable curve object with UI handles. |
| [Move Topological brush](https://novedge.com/blogs/design-news/zbrush-tip-move-topological-precise-island-editing) | ✅ have | M | sculpt grab (any brush) with mask.connected: a flood over the surface band from the dab's center within 1.5 radii, so a neighbouring finger stays still (sculpt Stage 3); live preview (sculpt Stage 7) | Done. |
| [Planar, Trim and Polish brushes (incl. Planar Cut)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/planar-trim-polish/planar-trim-polish.html) | ✅ have | S | sculpt flatten with normal and side above\|below\|both (Planar preset); trim_dynamic and polish on the area plane (sculpt Stage 2); live preview (sculpt Stage 7) | Done. |
| [TrimDynamic and hPolish brushes](https://novedge.com/blogs/design-news/zbrush-tip-trimdynamic-planar-blocking-and-hard-surface-refinement) | ✅ have | S | sculpt trim_dynamic (cuts only above the area's plane, flat sharp-edged falloff) and polish (presses both ways onto it), TrimDynamic and hPolish presets (sculpt Stage 2); clay sharp:true feature-preserving meshing | Live preview (Stage 7). |
| [Dam Standard brush](https://www.versluis.com/2021/02/zbrush-basics-cheat-sheet) | ✅ have | S | sculpt brush crease (sharp V line; invert gives ridge) with taper [start, end]; DamStandard preset on the Sculpt tool, live preview (sculpt Stage 7) | Done. |
| [Layer brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ✅ have | S | sculpt brush layer (fixed plateau, no buildup); Sculpt tool, live preview (sculpt Stage 7) | Done. |
| [Pinch brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ✅ have | S | sculpt brush pinch; Sculpt tool, live preview (sculpt Stage 7) | Done. |
| [Snakehook brush](https://www.maxon.net/en/zbrush-plan-options) | ✅ have | S | sculpt brush snakehook: pulls a horn along a path up to 16 radii, curl_deg curls it (sculpt Stage 2); stretch / extrude (tapered tip pull up to 8 radii); the Sculpt tool drags it across the view plane and sends the path (sculpt Stage 7) | Done. |

### Strokes & alphas

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Adjust Last](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/strokes/adjust-last/adjust-last.html) | ✅ have | S | stroke_edit {action: scale, last: true, factor 0..2} rescales the newest stroke from its stored before/after (LastDelta, stale after undo); stroke_edit scale {ids, factor} for any stroke (sculpt Stage 1b); the HUD's Adjust Last slider sends stroke_edit scale last on release (sculpt Stage 7) | Done. |
| [LazyMouse 2.0 (LazySnap, Backtrack)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/stroke/lazy-mouse/lazy-mouse.html) | ✅ have | S | LazyMouse in the brush tools' stroke capture (src/app/brush.rs): string radius, step, LazySnap and Backtrack; the request holds the smoothed samples (no lazy, so a replay smooths nothing); agents' strokes take lazy since sculpt Stage 1a (sculpt Stage 7) | Done. |
| [Stroke types (Dots, FreeHand, DragRect, DragDot, Spray, Colorized Spray)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/stroke/stroke.html) | 🟡 partial | S | stroke kind path\|dots\|spray {count, jitter, seed}\|drag_rect {from, to}\|drag_dot, spacing and lazy for sculpt and paint (src/brush/) | Add colorized spray. Record and replay are done: every stroke is a ledger record (stroke_log, stroke_edit replace/replay, sculpt Stage 1b). |

### Subdivision & detail

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Multi-resolution subdivision levels](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | 🟡 partial | L | clay resolution + detail_cell + coarse/fine detail grids (src/clay.rs) | Detail lives on coarse and fine grids, but there are no user-navigable levels. Add MCP detail_level {target, level} so coarse edits propagate to fine (sculpt at a chosen cell). Add UI to step levels. |

### Topology & remeshing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [ZRemesher](https://www.maxon.net/en/zbrush/features/remeshing-options) | ❌ missing | XL | bake simplifies to a triangle budget (meshopt, src/bake.rs) | Add MCP retopo {target, quads target count, adaptive, keep_blob_borders, symmetry} that produces a quad polygon mesh (e.g. Instant Meshes/QuadWild-style field-aligned). Bake should use it as the low poly. |
| [Remesh All / boolean-type remeshing](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/remeshing/remesh-subtools/remesh-subtools.html) | ✅ have | S | blob op add/carve/intersect; MCP to_clay {targets, op} merges objects into one clay, mode volume remeshes it (sculpt Stage 5) | None. |
| [Sculptris Pro](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | ✅ have | M | sculpt stroke cell grows a detail region over the stroke (same-cell overlaps merge, at most 16; src/clay/sculptris.rs, sculpt Stage 6); edit_clay detail_regions by hand | None (the live view meshes regions at the clay's detail cell; the fine mesh shows them finer). |

### UI/workflow & automation

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [QuickSave](https://pixologic.com/zbrushlive/?p=9373) | ✅ have | S | QuickSave every 5 min while unsaved, 10 rotating backups in ~/Library/Application Support/Clide3D/backups; history {action: quicksave, enabled, now}; File menu toggle (sculpt Stage 1b) | None. |
| [Undo History (markers, saved in ZPR)](https://www.maxon.net/en/zbrush-for-ipad-key-features) | 🟡 partial | M | history {list, goto, mark, label}: one labelled step per agent call or UI edit ("sculpt standard x3 on Head [agent]"), capped at 300 steps / 2 GB (sculpt Stage 1b); the HUD's history scrubber sends history goto on release, Mark sends history mark (sculpt Stage 7) | Optional saving of marks in the scene. |

### ZSpheres & base meshes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Make PolyMesh3D (primitive to polymesh conversion)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/polymesh.html) | ✅ have | S | MCP to_clay {target}: primitives become analytic blobs in place, other meshes mesh blobs (sculpt Stage 5) | None. |

## P2

### Characters & posing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [TransPose (action line)](https://www.maxon.net/en/zbrush/features/flexible-workflows) | 🟡 partial | M | pose_bone (rest-pose move/turn carrying props); gizmo | Add maskless posing via anchor_deform, plus a UI action-line manipulator. |

### Deformers & gizmo

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Gizmo deformer: Bend Arc](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | L | — | Add MCP deform {target, type: bend_arc\|bend_curve\|twist\|taper\|skew\|stretch\|flatten\|inflate\|offset\|rotate\|scale\|smooth, axis, amount, region}, implemented as a space warp applied to blobs and detail, then baked. Add UI handles later. |
| [Gizmo deformer: Bend Curve](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | — | deform type bend_curve with control points along an axis. |
| [Gizmo deformer: Deformer / Deformer Hard / Deformer Soft (FFD)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | M | — | Add deform type lattice {divisions, points}, an FFD cage over the object's bounds. |
| [Gizmo deformer: Taper](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | sweep taper (blob only) | deform type taper. |
| [Gizmo deformer: Twist](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | sweep/chain twist (blob only) | deform type twist. |
| [Anchors brush (incl. Bend mode)](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | 🟡 partial | M | pose_bone on rigs; rig weights | Add MCP anchor_deform {target, anchors:[points], move/rotate/bend at one anchor}, using a temporary rig with automatic weights, without creating a permanent rig. |
| [Gizmo 3D extra operations (extrude, duplicate, equidistant copies, negative mesh, soft deformation)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/extra-operations/extra-operations.html) | 🟡 partial | M | duplicate_object; blob array; kitbash array | Add MCP soft_transform {target, mask/region, falloff, translate/rotate/scale} on the clay detail and blobs. Add ctrl-drag duplicate in the UI. |

### Dynamics & cloth

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Collision volumes](https://www.cgchannel.com/?p=118420) | 🟡 partial | M | cloth collides with its parent clay | Add cloth colliders: [objects]. |
| [Dynamics palette (cloth / soft-body simulation)](https://www.maxon.net/en/zbrush/features/flexible-workflows) | 🟡 partial | L | MCP cloth (XPBD sewn patterns, fabrics, pins/belts, src/cloth.rs) | Add simulation on any object or region (cloth {target: any mesh, pins: mask}), plus inflate, contract and liquify modes. |

### Fibers / hair

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Groom brushes](https://www.maxon.net/en/zbrush/features/flexible-workflows) | ❌ missing | M | hair grow comb/part settings | Add MCP groom {target, brush: comb\|lengthen\|clump\|twist\|blow\|cut, points, radius, direction} that edits guides. Add a UI drag. |
| [FiberMesh](https://www.maxon.net/en/zbrush/features/flexible-workflows) | 🟡 partial | M | Hair object (guides, cards, film strands, clump, frizz; src/hair.rs); face_hair | Add fiber growth on any surface region (grow on any clay with a density map), and export strands as curves. |

### Hard surface & modeling

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Knife brushes (Curve, Lasso, Rect, Circle) & Split to Parts](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/clip-brushes/knife-brushes/knife-brushes.html) | ❌ missing | S | — | Add MCP split_clay {target, plane or view curve}, which makes two clays by carving each side. |
| [ZModeler Edge Actions](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/zmodeler-actions/edge-actions/edge-actions.html) | ❌ missing | L | — | poly_edit edge actions: bevel, bridge, close hole, collapse, crease, insert, slide. |
| [ZModeler Polygon Actions](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/zmodeler-actions/polygon-actions/polygon-actions.html) | ❌ missing | L | — | poly_edit face actions: extrude, inset, bevel, bridge, split, insert loops, QMesh. |
| [Bevel brushes (BevelArc, BevelFlat)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/bevel-brushes/bevel-brushes.html) | 🟡 partial | S | blob round, extrude bevel, join chamfer/round | Add a sculpt brush bevel {flat\|arc} along a hard-edge path in the detail layer. |
| [BevelPro](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/bevelpro/bevelpro.html) | 🟡 partial | M | blob round/bevel/join chamfer; kitbash parts are chamfered | Add an auto-bevel of all hard edges at a radius across a clay (edit_clay bevel_all), as a global SDF rounding with crease detection. |
| [ClayPolish](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | S | edit_clay sharp:true (feature-preserving field placement) | Add edit_clay polish {amount, angle}, a curvature-aware smoothing of the detail layer that keeps sharp edges. |
| [Clip brushes (ClipCurve, ClipCircle, ClipRect)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/clip-brushes/clip-brushes.html) | 🟡 partial | S | carve box/extrude blobs with blend 0 | Add MCP clip {target, view polyline or plane}, which adds a carve extrude from a screen-space curve. |
| [Panel Loops](https://www.maxon.net/en/zbrush-automotive-design) | 🟡 partial | M | blob op plate (wall, rim, bevel); detail_pass panel | Add panels from a painted mask or group region (plate limited by mask) once masks exist. |
| [Trim brushes (Curve, Lasso, Rectangle, Circle)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/clip-brushes/trim-curve/trim-curve.html) | 🟡 partial | S | carve blobs (always closed) | Same as clip, using a lasso polygon extruded through the view. |

### Import/export & interop

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [GoZ](https://www.maxon.net/en/zbrush/features/interoperability) | ❌ missing | L | export_glb / export_fbx (one-way) | Add mesh import (see formats), then a file-watch round-trip bridge. MCP already lets other agents drive it. |
| [Maya Blend Shapes](https://www.maxon.net/en/zbrush/features/interoperability) | ❌ missing | M | — | Export sculpt layers and morphs as glTF morph targets / FBX BlendShapes. |
| [USD / USDZ](https://www.maxon.net/en/zbrush/features/supported-formats) | ❌ missing | L | — | Add USD(A/Z) export. |
| [Image formats (BMP, PSD, JPG, TIF, PNG, GIF, HDR, EXR)](https://www.maxon.net/en/zbrush/features/supported-formats) | 🟡 partial | S | reads PNG/JPEG/HDR; writes PNG | Add EXR and 16-bit TIFF read/write for alphas and displacement. |

### Insert / array / nano meshes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Create InsertMesh / IMM brush from Tool](https://www.maxon.net/en/zbrush-for-ipad-key-features) | ❌ missing | S | scatter sources accept any object | Add kitbash register_part {target, name} so any scene object becomes a catalog part. |
| [ArrayMesh](https://www.maxon.net/en/zbrush/features/flexible-workflows) | 🟡 partial | M | blob array (line/circle/path, nested up to 4096); kitbash array; scatter instances | Add an object-level live array (create_object Array {source, stages:[{count, offset, rotate, scale}]}) with instancing and export. |
| [InsertMultiMesh (IMM) brushes](https://www.maxon.net/en/zbrush/features/brush-system) | 🟡 partial | S | kitbash catalog (bolts, vents, hatches...) | Add user IMM sets: kitbash library {name, objects}. Add a UI picker. |

### Layers & morphs

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Morph brush (incl. Undo History targets)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ❌ missing | S | — | Add a sculpt brush 'morph' toward the stored morph or history snapshot. |

### Masking & selection

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Grow All / Shrink All mask](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | ❌ missing | S | — | Add mask grow_all/shrink_all, a flood to the connected component boundary. |
| [Mask By Color (Intensity, Hue, Saturation)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/masking/masking.html) | ❌ missing | S | — | Add mask {by:'color', hue/sat/intensity range} from paint. |
| [Gizmo / Transpose topological masking](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | 🟡 partial | M | rig split parts; pose_bone carries assigned blobs | Add mask {by:'connected', from point, falloff}. Let the gizmo or a deform tool act on the masked region. |
| [Mask Adjust](https://www.maxon.net/en/zbrush-for-ipad-key-features) | 🟡 partial | S | material layer threshold/contrast/invert | Add mask {adjust: curve, blur} on the sculpt mask channel. |
| [Mask Ambient Occlusion](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/masking/masking.html) | 🟡 partial | S | material mask term ao; bake AO | Add mask {by:'ao'} on the sculpt mask channel. |
| [Mask By Smoothness / MaskPeaksAndValleys](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/masking/masking.html) | 🟡 partial | S | material mask terms edges/cavity | Add mask {by:'peaks'\|'valleys'\|'smoothness'}. |
| [Mask by Normals / Mask by Depth](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2022/zbrush-2022.html) | 🟡 partial | S | material mask terms up/down/direction/height | Add mask {by:'normal', direction, angle} and {by:'depth', view, range} for the sculpt mask. |
| [Mirror / Flip Mask by Posable Symmetry](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/masking/masking-additions/masking-additions.html) | 🟡 partial | S | stroke mirror:true (X=0 object space) | Add mask {mirror\|flip} across X. Topological symmetry would need mesh correspondence (not applicable to SDF). |
| [Selection brushes / Mesh Visibility](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/mesh-visibility/mesh-visibility.html) | 🟡 partial | M | object hide/show (Ctrl+H, update_object visible) | Add partial visibility within clay: hide a region or blobs (MCP hide_region {blobs \| view polygon}). Sculpt would affect visible parts only. |

### Polygroups

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [PolyGroup generators](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/polygroups/polygroups.html) | ❌ missing | M | — | Add group generators: by_normals, from_paint, from_mask, by_blob. |

### Polypaint & texturing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Adjust Colors / Adjust by Color](https://www.maxon.net/en/zbrush/features/advanced-painting-texturing) | ❌ missing | S | — | Add paint {adjust: hue, saturation, intensity, contrast, gamma, tint, color_mask}. |
| [Vector Displacement Maps](https://www.maxon.net/en/zbrush/features/map-generation) | ✅ have | M | bake maps: [vector_displacement] (sculpt Stage 8b): the offset to the clay in the simple mesh's tangent frame (R +u tangent, G bitangent, B normal), RGB 16-bit PNG or EXR | Offsets are found only along the simple mesh's interpolated normal (from the fine surface or the clay field), so R and G are about 0 and bakes carry no overhangs or sideways detail; VDM stamps from clay (alpha create_vdm, sculpt Stage 8c) keep them. |
| [Multi Map Exporter (incl. UDIM selection)](https://www.maxon.net/en/zbrush/features/interoperability) | 🟡 partial | M | bake (color, normal, AO/rough/metal) + export_glb atlas | Add bake maps:[...] with save_dir, naming and UDIM tiles. |
| [SpotLight](https://www.maxon.net/en/zbrush/features/spotlight) | 🟡 partial | M | MCP project_reference (view projection of reference image color) | Add brush-local image projection with an offset/scale/rotate image placement (also as relief), and multiple images. |
| [Texture Maps](https://www.maxon.net/en/zbrush/features/map-generation) | 🟡 partial | M | bake color texture (src/bake.rs) | Add texture to paint import (paint {from_texture} using UVs) once mesh import exists. |
| [Normal Maps](https://www.maxon.net/en/zbrush/features/map-generation) | ✅ have | S | bake tangent-space normal map | Add standalone PNG map export (bake save_maps path). |

### Rendering

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Redshift AOV passes & OIDN denoising](https://help.maxon.net/zbr/en-us/Content/html/user-guide/materials-lights-rendering/rendering/redshift/redshift-aov-passes.html) | 🟡 partial | M | render_film denoise (src/film/denoise.rs) | Add AOVs (diffuse, spec, GI, depth, normal, position) as with BPR passes. |
| [Redshift real-time viewport lighting](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | 🟡 partial | M | viewport HDR post, key-light shadows, object light pools (set_display) | Add viewport HDRI environment lighting matching render_film environment. |
| [Timeline](https://www.maxon.net/en/zbrush/features/flexible-workflows) | 🟡 partial | M | animate clips (bone keys), playback, time slider (src/app/timeline.rs) | Add keyframes for object transforms, camera, visibility and material values (animate target object/camera channel). |
| [Turntable](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | S | render_views multiple yaws; render_animation | Add render_turntable {frames, axis, path} to a contact sheet or movie, with film quality optional. |

### Sculpting brushes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Brush Depth (Imbed, Inner/Outer Depth, Gravity)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/depth/depth.html) | ❌ missing | S | — | Add stroke.imbed (offset of the brush center along the normal) and gravity {direction, strength} on sculpt strokes. |
| [Contrast slider and Contrast brushes (ContrastTarget/ContrastDelta)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/contrast/contrast.html) | ❌ missing | S | — | Add edit_clay detail_contrast (scale the high-frequency detail globally) and a 'contrast' sculpt brush (local amplitude scale). |
| [Elastic brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ❌ missing | S | — | Add an elastic variant of inflate that keeps shape: displacement along the smoothed normal with volume-preserving falloff. |
| [History / HistoryRecall brush](https://www.maxon.net/en/zbrush/features/brush-system) | ❌ missing | M | undo stack (app.rs undo: Vec<Scene>) | Add a sculpt brush 'recall' with history_index that blends the detail field toward a stored undo snapshot within radius. Add an MCP list_history tool. |
| [Per-brush and dynamic size memory](https://3dvf.com/actualite-23003-pixologic-lance-zbrush-2018-html/) | ❌ missing | S | — | Matters only for the interactive UI. Store size and strength per brush preset, and add a screen-relative dynamic size mode. |
| [Smooth Brush Modifiers](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/smooth-brush-modifiers/smooth-brush-modifiers.html) | ❌ missing | S | — | Add smooth stroke options: polish strength, weighted/tangent smoothing, separate alt radius, and converge color. |
| [XTractor brushes](https://www.maxon.net/en/zbrush/features/brush-system) | ❌ missing | M | — | Add MCP capture_alpha {target, center, normal, radius}, which samples the detail height field into a 16-bit alpha for reuse. Requires the alpha system. |
| [Brush Noise (Local Projection, Dynamic Scale)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/brush-noise/brush-noise.html) | 🟡 partial | S | sculpt noise brush (pores, bumps, wrinkles, scales, plates, folds) | Add a noise modulation term on any brush (noise:{pattern, scale, strength}) instead of a separate noise brush. |
| [DecoBrush / DecoCurve brushes](https://www.maxon.net/en/zbrush/features/advanced-painting-texturing) | 🟡 partial | S | MCP decal (image/rune/emblem stamps with depth relief) | Add decal path + spacing that repeats an alpha or image along a curve, for both relief and paint. |
| [Displace brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | 🟡 partial | S | sculpt inflate on detail layer | Add a 'displace' brush that pushes along the low-frequency normal so existing fine detail is carried along instead of smoothed. |
| [MoveInfiniteDepth brush](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/move-infinite-depth/move-infinite-depth.html) | 🟡 partial | S | sculpt grab (volumetric SDF, radius sphere) | Add a depth:'infinite' option to grab that uses a cylinder along the view or a given axis, so front and back surfaces move together. |
| [Scribe brushes (ScribeStandard, ScribeChisel) and SubSteps](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/curve-brushes/scribe-brushes/scribe-brushes.html) | 🟡 partial | S | sculpt crease along path; clay band engrave | Add width taper at the path ends and a substeps dash pattern on the crease/carve path. |
| [SnakeCurve brushes](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/snake-curve/snake-curve.html) | 🟡 partial | S | clay capsule/sweep/chain blobs along points; sculpt stretch | Add a sculpt 'snake_curve' that pulls a tapered extrusion along a 3D curve off the surface (a sweep blob with smooth join, from the surface point). |
| [ZProject brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | 🟡 partial | M | MCP project_reference (projects reference image color onto clay) | Add a brush-local projection: project_reference with center/radius, and also project depth from an image or other objects. |
| [CurveAlpha / CurveAlphas / ExtrudeProfile brushes](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/curve-alpha/curve-alpha.html) | ✅ have | S | clay blob sweep {profile, points} and extrude {profile, depth} (src/hard_surface.rs) | Add a profile from an alpha image or a mesh cross-section. Add UI for drawing curves. |

### Strokes & alphas

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Alpha Create (incl. from NoiseMaker)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/alpha/create/create.html) | ❌ missing | S | sculpt noise patterns; material grunge maps | Add MCP alpha_create {noise pattern, size, seed} or from a library map. |
| [Alpha Modify](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/alpha/modify/modify.html) | ❌ missing | S | — | Add MCP alpha_edit {blur, noise, tiles, mid_value, contrast, curve} that outputs a new alpha. |
| [Curve Modifiers (incl. Repel, Bend Start/End)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/stroke/curve-modifiers/curve-modifiers.html) | ❌ missing | S | sweep taper/twist | Add per-point size and intensity along stroke paths (size_curve, strength_curve). |
| [DragStamp stroke](https://www.maxon.net/en/zbrush/features/advanced-painting-texturing) | ❌ missing | S | decal point/size/rotate is a static stamp | Add a stamp mode for sculpt (alpha at point with intensity). Add an interactive drag-to-scale UI. |
| [Frame Mesh](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/strokes/frame-mesh/frame-mesh.html) | ❌ missing | M | kitbash along:'edge' finds hard edges | Add MCP extract_curves {target, from: blob_borders\|creases\|hard_edges} that returns polylines for reuse in strokes and sweeps. |
| [Stencils (Coin Controller, Wrap Mode)](https://www.maxon.net/en/zbrush/features/flexible-workflows) | ❌ missing | M | project_reference (view-aligned image) | Add MCP set_stencil {image, view, position, scale, rotate, tile, wrap} that masks sculpt and paint strokes in screen space. Add a UI overlay. |
| [Stroke Modifiers](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/stroke/modifiers/modifiers.html) | ❌ missing | S | — | Add spacing, directional alpha, roll and replay_last (MCP repeat_last_stroke with offset) to the stroke schema. |
| [Curve Mode and Curve Functions](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/stroke/curve-functions/curve-functions.html) | 🟡 partial | M | stroke points paths; sweep blobs | Add persistent editable curve objects (MCP create_curve/edit_curve) that any brush or kitbash part can be applied along, then re-applied after edits. |

### Subdivision & detail

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Dynamic Subdivision (Smooth/Flat, QGrid, Dynamic Thickness)](https://www.maxon.net/en/zbrush/features/polygon-modeling) | ❌ missing | M | box round / extrude bevel blobs | For polygon objects (primitives and imports) add a Catmull-Clark preview: update_object subdiv {levels, qgrid, thickness, apply}. |
| [Mesh Projection (Project, Project All, ProjectionShell, Reproject Higher Subdiv)](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | M | face_detail / project_reference (color only) | Add MCP project_detail {source object, target, distance} that projects one object's surface into another clay's detail layer (scan to clean model). Requires mesh import. |
| [HD Geometry](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | 🟡 partial | M | detail_regions (fine mesh up to 24M triangles, adaptive) | Add on-demand fine-mesh viewing of one region in the viewport (set_display fine, region-limited). |
| [Surface Noise & NoiseMaker (incl. NoiseMaker 2.0)](https://www.maxon.net/en/zbrush/features/flexible-workflows) | 🟡 partial | M | sculpt noise brush; set_material detail library + detail_displace (film only) | Add non-destructive object-level noise (update_object surface_noise {pattern, scale, strength, mask}) shown in the viewport, with apply to sculpt. |

### Topology & remeshing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Retopo brush (manual retopology)](https://www.maxon.net/en/zbrush/features/brush-system) | ❌ missing | L | — | Add MCP poly_edit on a new polygon object (add_vertex snapped to surface, add_face, extrude_edge). Add UI click tools. |
| [ZRemesher Guides, partial & symmetric remesh](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/topology/zremesher/introduction/introduction.html) | ❌ missing | M | — | Add retopo guides: [polylines], region and mirror. |
| [Mesh Fusion](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/topology/mesh-fusion/mesh-fusion.html) | 🟡 partial | S | blob blend / join round\|chamfer (smooth SDF union) | Covered for clay. Fusing separate objects needs MCP merge_clay {targets} into one clay. |
| [Decimation Master](https://www.maxon.net/en/zbrush-video-games) | ✅ have | S | MCP bake triangles budget (meshoptimizer) + lods (src/bake.rs) | Add standalone decimate {target, ratio} for non-clay meshes, keeping paint as vertex color without UV baking. |

### UI/workflow & automation

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Custom Interface & QuickMenu](https://help.maxon.net/zbr/en-us/Content/html/user-guide/customizing-zbrush/interface-layout/custom-interface/custom-interface.html) | ❌ missing | M | fixed Maya-style layout with shelf | Add a spacebar quick menu for brushes once interactive sculpting exists. |
| [LightBox (Maxon & User)](https://help.maxon.net/zbr/en-us/Content/html/getting-started/basic-concepts/lightbox/lightbox.html) | ❌ missing | M | set_material list; kitbash catalog | Add MCP library {list, open: scenes, brushes, alphas, materials} with a thumbnail browser panel. |
| [Macros](https://help.maxon.net/zbr/en-us/Content/html/user-guide/customizing-zbrush/macros/macros.html) | ❌ missing | S | — | Add MCP macro {record, stop, play, name}, which records MCP calls into a replayable list exposed as shelf buttons. |
| [Preferences: Mem / Undo limits & Config (Store Config, Init ZBrush)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/preferences/mem/mem.html) | 🟡 partial | S | undo capped at 300 steps and 2 GB of grid data held only by the history; dropped steps reported by history list (sculpt Stage 1b) | Make the limits user settings. |
| [Sticky Hotkeys](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2021-6/zbrush-2021-6.html) | ❌ missing | S | — | Add hold-and-drag to adjust brush size and strength (needed with interactive sculpting). |
| [SubTool Split / Merge](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | S | — | Add merge_clay {targets} and split_clay {by blobs \| plane}. |
| [Variable Cursor Size](https://zbrushcentral.com/t/zbrush-2021-now-available/377852) | ❌ missing | S | — | Comes with the interactive brush cursor. |
| [High polygon capacity](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | L | fine mesh up to 24M triangles with detail regions; resolution up to 512 | Add interactive viewport display of multi-million meshes with LOD. |
| [Reference images (floor grids)](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | S | set_reference overlay on a viewport pane; compare_reference | Add multiple reference images per view (front/side/top) with placement. |

### UV

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [UV Editor](https://www.maxon.net/en/zbrush/features/uv-editor) | ❌ missing | L | — | Add a UV view panel plus MCP uv_edit (pack, relax, move islands, texel density). Clay is UV-free, so this matters for baked or imported meshes. |
| [UV Master](https://www.maxon.net/en/zbrush/features/uv-master) | 🟡 partial | M | bake auto UV unwrap via xatlas (src/bake.rs) | Add MCP unwrap {target, seams_hint: painted channel, density_hint}, available without a full bake. |

### ZSpheres & base meshes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Mannequins](https://www.maxon.net/en/zbrush/features/base-mesh-creation) | 🟡 partial | L | MCP create_human (MakeHuman base, 600+ controls, rig) | Add animal or creature base templates (quadruped, bird) with rigs. |
| [Mesh Extract](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | S | blob op cover (offset shell clipped to a blob) / plate | Add an extract from a painted mask into a new clay object with thickness. |
| [Snapshot3D](https://www.maxon.net/en/zbrush/features/base-mesh-creation) | 🟡 partial | S | extrude blob from polygon profile (add/carve) | Add an extrude profile from an image alpha (traced contour), via profile_image: path. |
| [Text 3D & Vector Shapes](https://help.maxon.net/zbr/en-us/Content/html/user-guide/zbrush-plugins/text-3d-vector-shapes/text-3d-vector-shapes.html) | 🟡 partial | M | kitbash stencil text (A-Z, 0-9) | Add a text blob form {text, font, depth, bevel} and SVG import as extrude profiles. |
| [ZSpheres](https://www.maxon.net/en/zbrush/features/base-mesh-creation) | 🟡 partial | S | capsule (tapered a/b radii) and chain blobs; rig add points chain | Add MCP armature {spheres:[{center, radius, parent}]} that generates capsule blobs and a matching rig in one call. |

## P3

### Characters & posing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Stager](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | S | — | Add MCP stage {store_home, store_target, switch, interpolate} per object transform. |
| [Contact](https://www.maxon.net/en/zbrush-plan-options) | ✅ have | S | update_object bone/frame/grip; kitbash parts ride bones | None. |
| [Local / Dynamic Symmetry (Gizmo-based)](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | ✅ have | S | mirror is in object space, so it follows moved or rotated objects | Add a custom symmetry plane per object. |
| [Proxy Pose](https://www.maxon.net/en/zbrush/features/flexible-workflows) | ✅ have | S | rig skinning; sculpt works in rest pose while clips play | None. |
| [Transpose Master](https://www.maxon.net/en/zbrush/features/flexible-workflows) | ✅ have | S | pose_bone carries every object/prop/kitbash part on the bones | None. |
| [ZSphere Rigging](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/posing-your-model/rigging/rigging.html) | ✅ have | M | MCP rig (bones, auto weights, weight strokes), animate | Add a UI bone placement tool. |

### Deformers & gizmo

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Gizmo deformer: Extender](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | — | Add deform type extend (shift points beyond a plane). |
| [Gizmo deformer: Multi Slice](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | — | Polygroup slicing. Depends on surface groups. |
| [Gizmo deformer: Remesh by ZRemesher](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | — | Depends on the retopo MCP tool. |
| [Gizmo deformer: Skew](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | — | deform type skew. |
| [Gizmo deformer: Slice](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | — | Add an edge-loop slice. Depends on polygon or surface groups. |
| [Gizmo deformer: Stretch](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | — | deform type stretch. |
| [Gizmo deformer: Subdivide](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ❌ missing | S | — | Depends on polygon subdivision. |
| [Gizmo deformer: Bevel](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | blob round/bevel/join chamfer | Already covered by blob parameters. Nothing extra needed for clay. |
| [Gizmo deformer: Crease](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | clay sharp:true; join sharp | Applies to subdivided polygon meshes only. |
| [Gizmo deformer: Flatten](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | sculpt flatten brush | Add deform type flatten across the whole object. |
| [Gizmo deformer: Inflate](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | sculpt inflate brush | Add deform type inflate (whole object offset of the SDF). |
| [Gizmo deformer: Offset](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | update_object translate | Make deform offset with a gradient. |
| [Gizmo deformer: Project Primitive](https://www.maxon.net/en/zbrush/features/polygon-modeling) | 🟡 partial | S | blob op add/carve with primitives | Carve and add already project primitives into clay. Add an intersect op. |
| [Gizmo deformer: Remesh by Decimation](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | bake triangles | Add standalone decimate. |
| [Gizmo deformer: Remesh by Union](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | blob union | Add merge_clay across objects. |
| [Gizmo deformer: Rotate](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | update_object rotate | Add rotate with falloff via deform. |
| [Gizmo deformer: Scale](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | update_object scale | Add scale with falloff via deform. |
| [Gizmo deformer: Smooth / Smooth All](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | 🟡 partial | S | sculpt smooth | Add deform type smooth (whole object, iterations). |
| [Gizmo/Transpose snap to mask center & Transpose Classic](https://cgpress.org/archives/zbrush-2024-released.html) | 🟡 partial | S | set_pivot center:true | Add set_pivot to mask center once masks exist. |
| [Repeat To Active / Repeat To Other](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/deformation/deformation.html) | 🟡 partial | S | damage/wear/kitbash accept targets arrays; agent can loop | Accept targets[] on deform, sculpt and paint tools. |
| [Tool > Deformation sliders](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/deformation/deformation.html) | 🟡 partial | S | mirror_object; update_object; sculpt smooth/inflate | Add whole-object presets via the deform tool (relax, spherize, gravity, noise, unify). |
| [Gizmo deformer: Remesh by DynaMesh](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/deformers/list-of-deformers/list-of-deformers.html) | ✅ have | S | clay remesh via resolution | None for clay. |
| [Numeric Position / Size (Tool > Geometry)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/geometry/geometry.html) | ✅ have | S | update_object translate/scale; Channel Box (src/app/panels.rs) | Add a size-in-meters option (fit bounds to [x,y,z]). |

### Dynamics & cloth

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Cloth brushes (ClothMove, ClothPull, ClothSlide, ClothDimple, ClothTwister, ClothHook, ClothWind, ClothPinchTrails)](https://www.maxon.net/en/zbrush/features/brush-system) | ❌ missing | M | — | Add MCP cloth_stroke {target, brush: pull\|twist\|pinch\|wind, points, radius, strength} that runs XPBD with a stroke constraint. Add a UI drag. |
| [Dynamics recording & MDD export](https://www.cgchannel.com/?p=118420) | 🟡 partial | M | cloth frames/clip cache; exported via vertex joints in GLB/FBX | Add MDD or Alembic point-cache export. |
| [TransposeCloth / Gizmo cloth](https://www.cgchannel.com/?p=118420) | 🟡 partial | M | cloth re-drape after pose/animation clip | Add simulation during a transform (compress/move). |
| [Clothing pattern alphas](https://www.cgchannel.com/?p=118420) | ✅ have | S | cloth patterns tube/skirt/shirt/tunic/cape/panel | None (parametric patterns). |

### Hard surface & modeling

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Crease / Uncrease Unmasked](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | ❌ missing | S | — | Depends on poly creases and masks. |
| [Edge Extrude and Inset Equidistant](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/single-sided-meshes/single-sided-meshes.html) | ❌ missing | S | membrane blob (thin sheets) | poly_edit equidistant inset and extrude for single-sided meshes. |
| [ZModeler Bevel inner/outer edge modes](https://support.maxon.net/hc/en-us/articles/15780513877788-ZBrush-2025-0-1-Release-Notes) | ❌ missing | S | — | poly_edit bevel {edges: inner\|outer\|both}. |
| [ZModeler Crease Shortest Path](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/polygroup-fill/polygroup-fill.html) | ❌ missing | S | — | poly_edit crease_path {from, to}. |
| [ZModeler Edge Selector Widget](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/edge-selector-widget/edge-selector-widget.html) | ❌ missing | S | — | UI only, after poly_edit. |
| [ZModeler Insert Edge Loop snapping](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ❌ missing | S | — | poly_edit insert_loop {at: 0..1}. |
| [ZModeler Point Actions](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/zmodeler-actions/point-actions/point-actions.html) | ❌ missing | M | — | poly_edit point actions. |
| [ZModeler Presets / My Presets](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ❌ missing | S | — | Saved poly_edit action presets. |
| [ZModeler Slice Mesh](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/slice-mesh/slice-mesh.html) | ❌ missing | S | band engrave | poly_edit slice by plane or curve. |
| [ZModeler Targets](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/targets/polygon-targets/polygon-targets.html) | ❌ missing | M | — | poly_edit target selectors (loop, island, group). |
| [Crease Bevel (Geometry > Crease > Bevel, PropWidth, Bevel Width) & CTolerance](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/geometry/geometry.html) | 🟡 partial | S | extrude bevel, box round, join_size | Add an auto-crease angle option for sharp meshing. |
| [Crease brush](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/clip-brushes/crease-brush/crease-brush.html) | 🟡 partial | S | sculpt crease (V groove), band engrave | Subdivision crease marks need polygon support. The sculpt crease covers the visual result. |
| [Crease controls (Crease, Crease All, CreaseLvl, Crease PG, Uncrease)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/geometry/geometry.html) | 🟡 partial | S | edit_clay sharp:true, blob join sharp | Add Poly-object crease edges for subdivision (poly_edit crease). |
| [Polish By Features / By Groups / Crisp Edges](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/polish-features/polish-features.html) | 🟡 partial | S | sharp meshing; blob-aware smoothing | Add a polish option that keeps blob borders. |
| [QMesh / Quick Mesh primitives](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/quick-mesh/quick-mesh.html) | 🟡 partial | S | create_object Cube/Sphere/Cylinder primitives | Add all-quad primitives with segment counts for poly_edit. |
| [Delete Loops / Align Loops](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/delete-align-loops/delete-align-loops.html) | ➖ n/a | S | — | Only meaningful for polygon topology. |
| [Edge Loop (Masked Border, GroupsLoops, GPolish)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/geometry/geometry.html) | ➖ n/a | S | — | Only meaningful for polygon topology. |
| [UnClip](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/hard-surface/clip-brushes/unclip/unclip.html) | ➖ n/a | S | — | Carves are already non-destructive blobs. Removing the blob 'unclips' and keeps the sculpt detail. |

### Import/export & interop

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Alembic, Collada & 3MF](https://www.cgchannel.com/?p=118420) | ❌ missing | M | — | Add 3MF export (for printing) and Alembic cache export. |
| [Draft Angle Analysis (Mask/PolyPaint from Draft)](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | S | — | Add MCP analyze {target, kind:'draft', pull direction}, which paints or masks the result. |
| [Faster OBJ export](https://www.cgchannel.com/2024/09/maxon-releases-zbrush-2025/) | ❌ missing | S | — | OBJ export does not exist yet (see formats). |
| [Maya file format export](https://help.maxon.net/zbr/en-us/Content/html/user-guide/zbrush-other-programs/maya-file-format/maya-file-format.html) | ❌ missing | M | export_fbx (Maya reads it) | FBX covers this. Optional .ma writer. |
| [Mesh Thickness Analysis (PolyPaint from Thickness)](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | S | — | Add analyze kind 'thickness' (SDF makes this cheap), which paints a heat map. |
| [Photogrammetry camera import & ZCameras (256)](https://www.cgchannel.com/2021/11/pixologic-releases-zbrush-2022) | ❌ missing | S | — | Add stored cameras (set_camera save/recall name) first. Camera import is lower priority. |
| [Transpose Units / Transpose-line measuring](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/preferences/transpose-units/transpose-units.html) | ❌ missing | S | surface_point returns world points | Add MCP measure {from, to \| target bounds} in units. Add a UI ruler. |
| [ZBrush to Photoshop CC](https://www.maxon.net/en/zbrush/features/interoperability) | ❌ missing | S | render_film save_path PNG | Covered by render passes plus a layered EXR/PSD output. |
| [Model hollowing for 3D printing](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | S | edit_clay shell (hollow inward) | Add drain holes (carve capsules) and keys/joints helpers in export_print. |
| [Scale Master](https://www.maxon.net/en/zbrush-for-ipad-key-features) | 🟡 partial | S | scene unit = 1 meter | Add get_scene dimensions reporting in mm/cm/in and a fit-to-size option. |
| [Substance Bridge](https://www.maxon.net/en/zbrush/features/substance-bridge) | 🟡 partial | S | bake low poly + export_fbx/glb | Add export of low poly and high poly (fine mesh) together for external baking (export_fbx high_poly:true). |
| [ZTL / ZPR native files](https://www.cgchannel.com/2024/09/maxon-releases-zbrush-for-ipad/) | ✅ have | S | save_scene/open_scene JSON (blobs, detail, rigs, materials) | Add per-object save/load (an object library) and binary compression for large detail grids. |
| [Cineware](https://www.maxon.net/en/zbrush) | ➖ n/a | S | — | Specific to the Maxon ecosystem. |
| [GoZ updates for Maya / 3ds Max 2025-2027](https://www.maxon.net/en/zbrush/new) | ➖ n/a | S | — | Depends on a GoZ-like bridge. |
| [XMD ToolBox integration](https://www.cgchannel.com/2021/11/pixologic-releases-zbrush-2022) | ➖ n/a | S | — | Third-party integration. |
| [iMage3D](https://www.maxon.net/en/zbrush-plan-options) | ➖ n/a | S | — | Niche. |

### Insert / array / nano meshes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Gizmo3D IMM Replacement workflow](https://www.cgchannel.com/2025/12/maxon-releases-zbrush-2026-1-and-zbrush-for-ipad-2026-1/) | ❌ missing | S | — | Add kitbash action replace {group, part}, which keeps transforms. |
| [MicroMesh](https://www.maxon.net/en/zbrush/features/flexible-workflows) | ❌ missing | M | — | Add render-time instancing of a unit mesh per triangle in film (chainmail, weave). |
| [IMM Viewer (tabs and folders)](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | 🟡 partial | S | kitbash action catalog | Add a UI part browser with thumbnails. |
| [NanoMesh](https://www.maxon.net/en/zbrush/features/flexible-workflows) | 🟡 partial | S | scatter (on surfaces, density, scale/yaw random), detail_pass greeble | Add per-face or per-region placement fit to the face size. Scatter could align to the normal (align 1) with regular grid placement. |
| [TriParts brushes](https://www.maxon.net/en/zbrush-jewelry) | 🟡 partial | S | blob chain; kitbash track_link/chain/cable along path | Add kitbash triparts {start, middle, end objects, path}. |
| [All stroke types for Insert brushes](https://www.maxon.net/en/zbrush/features/brush-system) | ✅ have | S | kitbash at/points/path+spacing/array/along edge | Add a UI drag stroke. |
| [MeshInsert Dot brush](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/insert-mesh/meshinsert-dot/meshinsert-dot.html) | ✅ have | S | kitbash place at point | None. |
| [NanoMesh Edit Mesh](https://www.cgchannel.com/?p=118420) | ✅ have | S | scatter sources are live scene objects (editing the source updates every instance) | None. |

### Masking & selection

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Boost / Dilute Mask, Mask Changed Points](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/masking/masking-additions/masking-additions.html) | ❌ missing | S | — | Add mask boost/dilute, and mask {by:'changed_since', history_index} that diffs the detail field against an undo snapshot. |
| [Intersection Masker](https://help.maxon.net/zbr/en-us/Content/html/user-guide/zbrush-plugins/intersection-masker/intersection-masker.html) | ❌ missing | S | — | Add mask {by:'intersection', with: objects or blobs} using SDF overlap. |
| [Mask By Features (Border / Groups / Crease) & Go to Unmasked Center](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/masking/masking.html) | ❌ missing | S | — | Add mask {by:'blob_borders'\|'hard_edges'} and set_pivot {mask_center:true}. |
| [Mask By Fibers](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/masking/masking.html) | ❌ missing | S | — | Add a hair mask by strand length (root to tip) for later groom brushes. |
| [Mask Region (Auto Region, Analyze, Fill)](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | ❌ missing | M | — | Add mask {fill_from: point} that floods inside painted outline masks. |
| [Selection Sets](https://www.maxon.net/en/zbrush/new) | ❌ missing | S | — | Add MCP store_set/recall_set {target, kinds: mask, paint, blob groups} with named snapshots on the object. |
| [Mask By Alpha / Mask Txtr](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/masking/masking.html) | 🟡 partial | S | material mask grunge {map} | Add mask {by:'alpha'\|'texture', projection} into the sculpt mask. |
| [Lasso Smoothness and LazyMouse for Lasso](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2021-6/zbrush-2021-6.html) | ➖ n/a | S | — | Only needed once there is an interactive lasso. Agents pass polygons. |

### Polygroups

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [PolyGroup Fill (ZModeler)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zmodeler/polygroup-fill/polygroup-fill.html) | ❌ missing | S | — | Depends on polygon modeling. |
| [PolyGroup Select Growing](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2021-6/zbrush-2021-6.html) | ❌ missing | S | — | Add group-based selection growth once surface groups exist. |
| [PolyGroupIt](https://help.maxon.net/zbr/en-us/Content/html/user-guide/zbrush-plugins/polygroupit/polygroupit.html) | ❌ missing | S | — | Add group painting with symmetry (stroke-based), the same as group {paint}. |
| [QuickPolygroup brush](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/polygroups/quick-polygroup-brush/quick-polygroup-brush.html) | ❌ missing | M | — | Requires polygon loops. Only feasible on an imported or remeshed polygon mesh. |
| [ReGroup Visible / Groups Changed Points](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/polygroups/polygrouping-additions/polygrouping-additions.html) | ❌ missing | S | — | Add group {from:'changed_since'} after surface groups exist. |
| [Slice brushes (SliceCurve, SliceCircle, etc.)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/polygroups/slice-curve/slice-curve.html) | ❌ missing | S | — | Add group {slice: view polyline} that splits group ids along a projected curve. |
| [ZModeler Polygon Selection mode](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ❌ missing | M | — | Depends on polygon modeling. |

### Polypaint & texturing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Fade Opacity](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/fade-opacity/fade-opacity.html) | ❌ missing | S | — | Add set_display paint_opacity (show form without color). |
| [Redshift Baker 360 (bake render to polypaint)](https://www.maxon.net/en/zbrush/features/redshift-in-zbrush) | ❌ missing | M | — | Add bake {lighting:true}, which records film irradiance into paint or texture. |
| [SpotLight Dial (incl. Edge Detection)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/texture/spotlight/spotlight.html) | ❌ missing | S | — | Image transform params on projection. Clone/smudge are UI-only. |
| [SpotLight MidValue & Alpha Blur](https://renderu.com/en/news/post/24489) | ❌ missing | S | — | Projection-as-displacement params. |
| [ZAppLink](https://www.maxon.net/en/zbrush-advertising) | ❌ missing | S | — | Add render_views with save_path, then paint {project_image, view}, a round trip of an edited screenshot back onto the surface. |
| [ZColor](https://www.maxon.net/en/zbrush/features/advanced-painting-texturing) | ❌ missing | S | set_reference returns palette | Add saved color palettes in the UI. Agents use sRGB values directly. |
| [Color palette: FillObject, Secondary color, Gradient, SysPalette](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/color/color.html) | 🟡 partial | S | paint fill; object color; set_material | Add UI main/secondary color and gradient fill (paint gradient). |
| [Flip & Mirror Polypaint by Posable Symmetry](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/painting-your-model/polypaint/mirror-by-posable-symmetry/mirror-by-posable-symmetry.html) | 🟡 partial | S | paint stroke mirror:true; mirror_object | Add paint {mirror_all: 'x+to-x'}. |
| [Polypaint with sculpting brushes (M/RGB/MRGB)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/painting-your-model/polypaint/using-sculpting-brushes/using-sculpting-brushes.html) | 🟡 partial | S | separate sculpt and paint tools | Add an optional color/material on sculpt strokes (paint in the same stroke). |
| [Texture Grab flat/shaded](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2022/zbrush-2022.html) | 🟡 partial | S | screenshot / render_views | Add flat (unlit) grab mode. |
| [Tool > Polypaint utilities](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/polypaint/polypaint.html) | 🟡 partial | S | paint fill/clear | Add paint {gradient, from_texture, from_blob_colors}. |
| [Ambient Occlusion plugin](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2021-6/zbrush-2021-6.html) | ✅ have | S | bake AO map; material ao mask term | Add AO baked to paint (vertex color) as an option. |
| [Delete Spotlight](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ✅ have | S | set_reference clear:true | None. |
| [Layer palette (2.5D canvas layers)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/layer/layer.html) | ➖ n/a | S | — | No 2.5D canvas. |
| [PaintStop](https://www.maxon.net/en/zbrush-movies-vfx) | ➖ n/a | S | — | A 2D painting app is out of scope. |
| [Picker palette](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/picker/picker.html) | ➖ n/a | S | — | Canvas-picking semantics do not apply. |
| [Pixol 2.5D canvas & Gyro](https://www.maxon.net/en/zbrush-scientific) | ➖ n/a | S | — | Not part of a 3D editor's scope. |
| [Projection Master](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/projection-master/projection-master.html) | ➖ n/a | S | — | No 2.5D canvas. project_reference covers the use case. |
| [QuickSketch & Pen brushes](https://www.maxon.net/en/zbrush-movies-vfx) | ➖ n/a | S | — | 2D sketching is out of scope. |

### Rendering

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [3D Posterization & Material Mixer](https://help.maxon.net/zbr/en-us/Content/html/user-guide/materials-lights-rendering/posterization/3d-posterization/3d-posterization.html) | ❌ missing | S | — | Add a posterize post option. |
| [LightCap / LightCap Designer](https://www.maxon.net/en/zbrush/features/lightcap-system) | ❌ missing | M | — | Viewport light rig editing (set_display lights) to go with matcaps. |
| [LightCap with Environments](https://www.maxon.net/en/zbrush/features/lightcap-system) | ❌ missing | S | render_film environment hdr | Viewport HDRI (see real-time viewport lighting). |
| [Movie palette (Record, TimeLapse, Snapshot, overlays)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/movie/movie.html) | ❌ missing | M | — | Add MCP record_session {start, stop, path}, which captures viewport frames on each edit (timelapse). |
| [NPR (Non-Photorealistic Rendering)](https://www.maxon.net/en/zbrush/features/best-preview-render-bpr) | ❌ missing | M | — | Add render_film style {toon, outline, halftone}, or an NPR viewport pass. |
| [Render Recall](https://www.maxon.net/en/zbrush/features/redshift-in-zbrush) | ❌ missing | S | — | Add MCP render_history {list, restore settings}, which keeps the last N render_film settings and images. |
| [Tool > Display Properties (Double, Flip, DSmooth, DRes, BPR Transparent/Invisible/Smooth Normals)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/display-properties/display-properties.html) | ❌ missing | S | update_object visible | Add per-object display flags (double-sided, render-invisible, transparent) on update_object. |
| [Undo History Movies](https://www.maxon.net/en/zbrush-scientific) | ❌ missing | S | — | Render the undo stack snapshots as a sequence. |
| [Wax Preview](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | S | — | Add a set_display wax (viewport wrap-lighting translucency). |
| [BPR Filters & Modulators](https://www.maxon.net/en/zbrush/features/best-preview-render-bpr) | 🟡 partial | S | render_film DOF, bloom, vignette, sharpen, grain, chromatic aberration, grade | Add depth- and mask-modulated filters (contour, depth fog in post). |
| [Depth Cue & Fog](https://help.maxon.net/zbr/en-us/Content/html/user-guide/materials-lights-rendering/rendering/depth-cue/depth-cue.html) | 🟡 partial | S | render_film fog (volumetric) | Add viewport depth cue (set_display fog). |
| [Lights & Floor Grid save/load](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | S | object lights saved with scene; film lights per call | Add saved render presets (render_preset save/load). |
| [Monster Clay MatCaps & materials](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ✅ have | S | built-in matcaps clay, red_wax, skin and metal (set_display matcap; sculpt Stage 6) | None. |
| [Radial Overlay BPR filter](https://www.cgchannel.com/2020/12/pixologic-unveils-zbrush-2021-5/) | 🟡 partial | S | film fog god rays / bloom streaks | Real god rays already exist. A 2D overlay filter is not needed. |
| [Redshift Shadow Catcher](https://www.cgchannel.com/2023/01/maxon-ships-zbrush-2023/) | 🟡 partial | S | render_film floor catches shadows | Add transparent background with shadow-catcher alpha output. |
| [Redshift emissive lighting, glass & caustics, metals](https://www.maxon.net/en/zbrush/features/redshift-in-zbrush) | 🟡 partial | L | emission lights scene; glass transmission; metal presets | Shadow rays pass glass, so there are no caustics. Add caustic paths if needed. |
| [Render Properties](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/render/render-properties/render-properties.html) | 🟡 partial | S | render_film floor, fine, object_lights, bounces, denoise | Add explicit toggles for shadows, SSS and hair on/off. |
| [Render modes (Best / Preview / Fast / Flat) & Preview Shadows](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/render/render.html) | 🟡 partial | S | viewport shaded/wireframe (set_display), render_views, render_film | Add flat (unlit color) and fast shading modes to set_display. |
| [Best Preview Render (BPR)](https://www.maxon.net/en/zbrush/features/best-preview-render-bpr) | ✅ have | S | MCP render_film (GPU/CPU path tracer, src/film/) | Add BPR-style quick toggles. Shadows, AO, SSS and hair are already present. |
| [Bucket or Progressive rendering](https://www.maxon.net/en/zbrush/features/redshift-in-zbrush) | ✅ have | S | progressive passes with time_limit (src/film/mod.rs) | None. |
| [Environment Maps / HDR lighting](https://www.maxon.net/en/zbrush-plan-options) | ✅ have | S | render_film environment {hdr, preset, rotate, intensity} | Viewport display of the environment. |
| [Light palette (8 standard lights, shadows, color, intensity, ambient)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/light/light.html) | ✅ have | S | render_film lights/extra_lights (sun, area, point, sphere, up to 64); object lights | Add more viewport lights beyond the key-light shadow. |
| [Redshift Materials (C4D parity, Polypaint & Preset Color)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/materials-lights-rendering/rendering/redshift/redshift-materials.html) | ✅ have | S | set_material PBR (rough, metal, clearcoat, sheen, SSS, transmission, emission) + smart presets | None. |
| [Redshift in ZBrush](https://www.maxon.net/en/zbrush/features/redshift-in-zbrush) | ✅ have | S | render_film GPU path tracer with CPU fallback; no UVs needed | None. |
| [Render > Adjustments (color correction)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/render/adjustments/adjustments.html) | ✅ have | S | render_film grade {lift, gamma, gain, saturation, contrast}, exposure, white_balance; viewport post | Add RGB curves. |
| [Standard Materials & Shader Mixer](https://www.maxon.net/en/zbrush/features/custom-materials) | ✅ have | M | set_material presets + layers (smart materials, src/smart.rs) | Add a UI material editor. |
| [SubSurface Scattering](https://www.maxon.net/en/zbrush-plan-options) | ✅ have | S | set_material subsurface/scatter (film) | Add a viewport approximation. |
| [ZBrush to KeyShot bridge](https://www.maxon.net/en/zbrush/features/universal-camera) | ➖ n/a | S | render_film is the built-in renderer | Not needed. |

### Sculpting brushes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Brush Modifiers](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/modifiers/modifiers.html) | ❌ missing | S | — | Add a strength multiplier, tilt brush and variations to the stroke schema. Mesh-insert modifiers belong with kitbash. |
| [Chisel 3D brushes and VDM in Sculptris Pro](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2021-6/zbrush-2021-6.html) | ❌ missing | S | detail_regions provide local density | Once VDM stamps exist, have them add a local detail region automatically so stamps resolve. |
| [Curve Bridge brush](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/curve-bridge/curve-bridge.html) | ❌ missing | M | membrane blob (polygon sheet) | Add a 'loft' blob form that lofts between two point curves with thickness and is welded via smooth join. |
| [Drop 3D (from IMM / from Alpha)](https://www.maxon.net/en/zbrush/features/flexible-workflows) | ❌ missing | M | kitbash place; decal image relief | Add stamps that convert an alpha or insert mesh into clay blobs or detail, with a local detail region and movable placement until committed. |
| [Hatch / HatchBackTrack brushes](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/hatch-brushes/hatch-brushes.html) | ❌ missing | S | — | Matters only for interactive strokes. Add stroke.forward_only, which ignores path segments that reverse direction. |
| [Magnify brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ❌ missing | S | sculpt pinch (inverse not exposed) | Allow negative pinch strength or invert:true on pinch, which gives magnify. |
| [Nudge brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | ❌ missing | S | — | Add a 'nudge' brush: a tangential slide of the surface along the stroke direction with no normal displacement. |
| [Blob brush](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/brush-types/brush-types.html) | 🟡 partial | S | sculpt noise pattern bumps; clay sphere blobs | Add a 'blob' brush: irregular noise-modulated add per dab, with a push/pull slider. |
| [Brush Samples](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/samples/samples.html) | 🟡 partial | S | sculpt stroke snap:true (on surface) | Add on-surface sampling modes (buildup vs constant samples, preserve edge, stabilize orientation) as stroke options. |

### Strokes & alphas

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Alpha Make 3D](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/alpha/make-3d/make-3d.html) | ❌ missing | S | extrude blob from polygon profile | Add a heightfield blob form {alpha, size, depth} that turns an alpha into clay. |
| [Alpha Transfer (MakeTx, MakeSt, GrabDoc, CropAndFill)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/alpha/transfer/transfer.html) | ❌ missing | S | — | Add MCP grab_depth {view} that renders the depth buffer as an alpha. Stencil conversion depends on stencils existing. |
| [Alpha from 3D Mesh (Multi Alpha brushes)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/alpha-from-3d-mesh/alpha-from-3d-mesh.html) | ❌ missing | S | — | Add MCP alpha_from_object {target, view} that renders object depth to an alpha. Brushes would take a list of alphas. |
| [Alpha/Stencil From IMM](https://www.cgchannel.com/2023/03/maxon-ships-zbrush-2023-1) | ❌ missing | S | — | Add alpha_from_object over a kitbash catalog part. |
| [Bas Relief (Geometry and Alpha)](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | M | decal image depth relief | Add MCP bas_relief {source objects, target, view, contrast, blur} that projects object depth into the target's detail layer or an alpha. |
| [Brush Orientation (SpinCenter / SpinAngle / SpinRate)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/brush/orientation/orientation.html) | ❌ missing | S | — | Add stroke.alpha_spin {rate, angle} once alphas exist. |
| [Dual Alphas & Textures (Two by Two)](https://www.maxon.net/en/zbrush/features/advanced-painting-texturing) | ❌ missing | S | — | Add stroke.alpha_b with a blend factor or per-point pressure. |
| [From Brush](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/from-brush/from-brush.html) | ❌ missing | S | — | Covered by capture_alpha (see XTractor), which also captures paint color into a texture. |
| [No Back & Forth mode](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/no-back-forth/no-back-forth.html) | ❌ missing | S | — | Same mechanism as Hatch: add stroke.forward_only. Mostly relevant to the interactive UI. |
| [Stroke Interpolate / Strokes Count](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/strokes/stroke-interpolate/stroke-interpolate.html) | ❌ missing | S | — | Add MCP interpolate_strokes between two stored strokes (count, blending size, strength and color). |
| [Stroke Jitter and Imperfection](https://zbrushcentral.com/t/zbrush-2021-now-available/377852) | ❌ missing | S | — | Add stroke.jitter {position, size, strength, seed} for deterministic imperfection. |
| [Curves Helper](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/sculpting-brushes/insert-mesh/curve-strokes/curves-helper/curves-helper.html) | ➖ n/a | S | — | Agents pass curve points directly, so no ZSphere-to-curve helper is needed. |

### Subdivision & detail

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Cinema 4D (Maxon) noises](https://www.maxon.net/en/zbrush/features/flexible-workflows) | ❌ missing | S | — | Add more noise types (voronoi, cell, turbulence) to the noise patterns and mask terms. |
| [Convert BPR To Geo](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/geometry/geometry.html) | ❌ missing | S | film-only hair strands and detail_displace | Add MCP convert_render_geo {target}: hair strands to a mesh, detail_displace to sculpt detail. |
| [MicroPoly](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | L | scatter instances | Add tiled geometry units over quad faces after polygon support exists. |
| [Project UndoHistory](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/sculpting/project-undohistory/project-undohistory.html) | ❌ missing | S | undo snapshots | Add recall {history_index, amount} for a whole object (see HistoryRecall). |
| [Slime Bridge](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | ❌ missing | S | capsule/sweep blobs | Add MCP slime_bridge {target, from blobs/points, to, count, tension, branches} that generates sagging capsule/sweep blobs. |
| [Thick Skin](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | S | — | Add edit_clay detail_limit (max displacement depth of the detail layer from the blob surface). |

### Topology & remeshing

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Additional Modify Topology utilities (Mesh From Brush, Unweld Group Border, Optimize Points Order, InsertMesh)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/geometry/geometry.html) | ❌ missing | S | — | Add kitbash part to standalone object (detach), and merge object into clay. |
| [Manual retopo ZModeler tools & auto-crease options](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ❌ missing | M | — | Depends on polygon modeling (insert loop, inset). |
| [Topology Patches library](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/topology/Retopo/05-using-topology-patches/05-using-topology-patches.html) | ❌ missing | M | — | Patch library placement onto a surface, after polygon editing exists. |
| [Topology brush](https://www.maxon.net/en/zbrush/features/remeshing-options) | ❌ missing | M | — | Add MCP topology_from_curves {curves, thickness} that builds a quad grid from intersecting curves. |
| [ZModeler Extrude Edge with SnapToSurface](https://zbrushcentral.com/t/zbrush-2021-now-available/377852) | ❌ missing | S | — | Add poly_edit extrude_edge {snap:true}. |
| [ZRemesher Retry, Keep Polypaint & caching](https://www.maxon.net/en/zbrush/features/remeshing-options) | ❌ missing | S | — | Add retopo seed variants and paint transfer to vertex colors. |
| [ZSphere Topology (Edit Topology)](https://www.maxon.net/en/zbrush/features/remeshing-options) | ❌ missing | M | — | Covered by retopo and poly_edit. Not needed separately. |
| [DynaMesh Partial Mesh & PolyGroup preservation](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/dynamesh/partial-mesh/partial-mesh.html) | 🟡 partial | M | blob names survive rebuilds | Add region-limited rebuild (only visible or masked region), and carry surface groups through remesh. |
| [DynaMesh Resolution Picker & auto-resolution dot](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2022/zbrush-2022.html) | 🟡 partial | S | edit_clay resolution / detail_cell; sculpt reports radius_used | Add resolution:'auto' that picks a cell from the smallest blob or feature size. |
| [Mesh Integrity (Check / Fix Mesh)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/geometry/geometry.html) | 🟡 partial | S | MCP check_export (validates GLB); clay is watertight | Add mesh_check for imported meshes (non-manifold, holes, flipped normals). |
| [Sculptris Pro SubDivide Size Picker](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | 🟡 partial | S | detail_regions cell per region | Add MCP sample_cell {point} that returns the effective cell, and use it in region auto-creation. |
| [Tessimate](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | M | adaptive refinement (src/adaptive.rs) | Add density by curvature, exposed via a detail_regions 'auto' mode. |
| [Auto Welding / Unweld All](https://zbrushcentral.com/t/zbrush-2021-now-available/377852) | ➖ n/a | S | — | Applies only to polygon meshes. |
| [Modify Topology (Close Holes, Weld, Merge Tris, Mirror And Weld, Delete By Symmetry, Equalize, DelHidden)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/geometry/geometry.html) | ➖ n/a | M | clay meshes are always closed (SDF) | Clay is always closed and needs none of this. Needed only for imported polygon meshes; add a mesh_cleanup MCP when import exists. |
| [Sculptris Pro on hidden geometry](https://www.cgchannel.com/2020/12/pixologic-unveils-zbrush-2021-5/) | ➖ n/a | S | — | There is no partial hiding inside clay yet. See Mesh Visibility. |

### UI/workflow & automation

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [3Dconnexion support](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/navigation/3dconnexion-devices/3dconnexion-devices.html) | ❌ missing | M | — | Add SpaceMouse input for viewport navigation. |
| [Copy/Paste/Clear for PolyGroups, Masking, Polypaint](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ❌ missing | S | paint clear | Add paint/mask copy_from {source object}. |
| [Language localization](https://www.maxon.net/en/zbrush-for-ipad-key-features) | ❌ missing | M | — | Add UI string localization. |
| [Pro Projects](https://www.maxon.net/en/zbrush-plan-options) | ❌ missing | S | — | Add sample project downloads. |
| [Save / Save Next](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2023/zbrush-2023.html) | ❌ missing | S | save_scene path | Add save_scene increment:true (name_v002.json). |
| [SubTool Visibility Sets](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2022/zbrush-2022.html) | ❌ missing | S | — | Add MCP visibility_set {save, recall, name}. |
| [Align & Distribute](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/align-distribute/align-distribute.html) | 🟡 partial | S | update_object align/frame; kit snap grid | Add MCP align_objects {targets, axis, mode: min\|center\|max\|distribute}. |
| [Apply Last Action to All SubTools / Folder](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | 🟡 partial | S | MCP agents repeat calls; damage/wear targets[] | Add targets[] on more tools. |
| [Axis Rotation sub-palette](https://www.cgchannel.com/2020/12/pixologic-unveils-zbrush-2021-5/) | 🟡 partial | S | set_camera yaw/pitch | Add a UI turntable lock or step rotation. |
| [CamView](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | S | four-view layout; frame_view persp/top/front/side | Add a clickable view cube in the UI. |
| [Document palette (canvas size, background gradient, image export/import)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/document/document.html) | 🟡 partial | S | render_film size/aspect/background; screenshot | Add a viewport background color or gradient setting. |
| [Image Plane (incl. Model Opacity toggle)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/zbrush-plugins/image-plane/image-plane.html) | 🟡 partial | S | set_reference view/opacity | Add several image planes and model ghosting (set_display model_opacity). |
| [Real-time Silhouette / Thumbnail view](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | S | compare_reference silhouettes; render.rs unlit silhouette fills | Add a set_display silhouette inset panel. |
| [Repeat to Similar Parts](https://www.maxon.net/en/zbrush/features/sculptural-freedom) | 🟡 partial | S | mirror_object linked:true | Add linked duplicates (instances that follow source edits) beyond mirroring. |
| [SubTool Master (incl. batch rename)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/zbrush-plugins/subtool-master/subtool-master.html) | 🟡 partial | S | agent loops over update_object | Add rename with pattern on multiple targets. |
| [Transform palette (Solo, Ghost/Transparency, PolyF, Local, 3D Copy, Explode)](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/transform/transform.html) | 🟡 partial | S | hide/show, wireframe_on_shaded | Add set_display solo/ghost/explode. |
| [Unified user folder](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | 🟡 partial | S | hotkeys/preferences persisted | Define a user dir for brushes, alphas and presets once they exist. |
| [Universal Camera](https://www.maxon.net/en/zbrush/features/universal-camera) | 🟡 partial | S | render_film camera focal_length_mm, sensor, DOF, ISO/shutter | Add stored named views (set_camera save/recall) and viewport FOV. |
| [ZPlugins architecture](https://www.cgchannel.com/2024/09/maxon-releases-zbrush-for-ipad/) | 🟡 partial | L | external MCP clients act as plugins | Add a plugin API for new tools or brushes, possibly WASM. |
| [CPU-based; native Apple Silicon & Windows on Arm](https://www.maxon.net/en/zbrush) | ✅ have | S | native Rust binary; film CPU fallback; wgpu | The viewport still needs a GPU. |
| [Custom Hotkeys](https://help.maxon.net/zbr/en-us/Content/html/user-guide/customizing-zbrush/hotkeys/hotkeys.html) | ✅ have | S | Hotkey Editor (src/app/hotkeys.rs) | None. |
| [Gizmo vertex camera centering](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2023/zbrush-2023.html) | ✅ have | S | double-click a model to orbit around that spot; set_camera look_at | None. |
| [Optimal multithreading](https://zbrushcentral.com/t/zbrush-2021-now-available/377852) | ✅ have | S | par_chunks meshing, background jobs (src/app/jobs.rs) | None. |
| [Python scripting](https://www.maxon.net/en/zbrush/features/python-scripting-support) | ✅ have | S | MCP JSON-RPC server, drivable from any language | Ship a thin Python client. |
| [SubTools & SubTool Folders](https://www.maxon.net/en/zbrush-plan-options) | ✅ have | S | scene objects, groups, Outliner (src/app/panels.rs) | None. |
| [Undo/Redo for Lighting & Materials](https://www.cgchannel.com/2023/03/maxon-ships-zbrush-2023-1) | ✅ have | S | undo snapshots the whole Scene including materials and lights | None. |
| [ZScript](https://www.oreilly.com/library/view/zbrush-character-creation/9780470249963/ch11.html) | ✅ have | S | 72 MCP tools (src/mcp.rs) + MEL subset command line (run_mel) | Expand the MEL subset or add a script runner that executes saved MCP call lists. |
| [See-through Mode](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/reference-images/see-through-mode/see-through-mode.html) | ➖ n/a | S | set_reference opacity overlay | The overlay covers this. |
| [ZBrushCore / ZBrushCoreMini product tiers](https://maxon.net/en/zbrushcore-or-zbrush) | ➖ n/a | S | — | Product packaging, not a feature. |

### UV

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Crease Edge UV Unwrap](https://www.maxon.net/en/zbrush/features/advanced-painting-texturing) | ❌ missing | S | — | Seams from hard edges and blob borders, as an unwrap option. |
| [Morph UV](https://www.maxon.net/en/zbrush/features/advanced-painting-texturing) | ❌ missing | M | — | Not useful for UV-free clay. Skip unless a poly path exists. |
| [Unwrap by PolyGroup](https://help.maxon.net/zbr/en-us/Content/html/features/whats-new-in-zbrush/zbrush-whats-new.html) | ❌ missing | S | — | Add unwrap {by:'blobs'}, with islands per named blob. |
| [Tool > UV Map sub-palette](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/tool/polymesh/uv-map/uv-map.html) | 🟡 partial | S | bake texture size | Add basic unwrap and map-size options to the unwrap tool. |
| [UV Editor unwrap / edit / packing tools (texel density, UDIM, trim sheets)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/uv-editor/unwrapping/unwrapping.html) | 🟡 partial | M | export_glb atlas texel_density; bake region_density | Add UDIM and trim-sheet packing in uv_edit. |

### ZSpheres & base meshes

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Alpha Flat To Mesh](https://help.maxon.net/zbr/en-us/Content/html/reference-guide/release-notes/zbrush-2021-6/zbrush-2021-6.html) | ❌ missing | S | — | Covered by Snapshot3D profile_image. |
| [ShadowBox](https://www.maxon.net/en/zbrush/features/base-mesh-creation) | ❌ missing | S | compare_reference silhouettes | Add MCP shadowbox {front, side, top silhouette images} that intersects three extrusions into clay. |
| [Gizmo 3D parametric meshes](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/gizmo-3d/parametric-meshes/parametric-meshes.html) | 🟡 partial | S | create_object primitives | Same as above. Add a topology parameter on insertion. |
| [MeshFromMask brushes (MeshExtrude, MeshBalloon, MeshSplat, MeshExtrudePropDepth)](https://www.maxon.net/en/zbrush/features/base-mesh-creation) | 🟡 partial | S | extrude blob with bevel | Add a balloon option on extrude (inflate profile) and a view-drawn lasso profile. |
| [ZSketch](https://www.maxon.net/en/zbrush-movies-vfx) | 🟡 partial | S | capsule/sweep blobs along points | Add UI freehand 3D strokes that create sweep blobs on a surface or armature. |
| [ZeeZoo & startup model library](https://www.maxon.net/en/zbrush-plan-options) | 🟡 partial | M | create_human; kit pieces; kitbash catalog | Add a starter-scene library (MCP list_samples/open_sample) of bundled JSON scenes. |
| [Adaptive Skin](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zspheres/adaptive-skin/adaptive-skin.html) | ✅ have | S | clay meshing of blobs (smooth union) | None. The SDF skin is automatic. |
| [Parametric primitives (Tool > Initialize)](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/primitives/primitives.html) | ✅ have | S | create_object Cube/Sphere/Cylinder/Cone/Plane/Torus; clay forms | Add segment and size parameters on primitives. |
| [Unified Skin](https://help.maxon.net/zbr/en-us/Content/html/user-guide/3d-modeling/modeling-basics/creating-meshes/zsketch/unified-skin/unified-skin.html) | ✅ have | S | clay SDF meshing | None. |

### iPad

| Feature | Clide3D | Effort | Equivalent today | What parity needs |
|---|---|---|---|---|
| [Global Quick Search](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-whats-new.html) | ❌ missing | S | — | Add a command palette over menu items, tools and MCP tools. |
| [Multiple brush/alpha/texture import](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-whats-new.html) | ❌ missing | S | — | Batch import into the library once alphas and brushes exist. |
| [Render export (4K, PNG alpha, resolution gate)](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-whats-new.html) | 🟡 partial | S | render_film size up to 1600, aspect, save_path PNG | Add a 4K size limit and an alpha background. |
| [Touch gestures](https://help.maxon.net/zbp/en-us/Content/html/3_interface-and-gestures_touch-gestures.html) | 🟡 partial | S | trackpad pinch/two-finger pan in viewport | Add trackpad undo gestures if wanted. |
| [3D Layers & SpotLight on iPad](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-whats-new.html) | ➖ n/a | S | — | See the desktop 3D Layers and SpotLight rows. |
| [3D Print Hub & Scale Master on iPad](https://www.cgchannel.com/?p=170024) | ➖ n/a | S | — | See the desktop 3D Print Hub row. |
| [Apple Pencil controls](https://help.maxon.net/zbp/en-us/Content/html/3_interface-and-gestures_apple-pencil-controls.html) | ➖ n/a | S | — | Desktop tablet pressure is covered by the tablet pressure row. |
| [Custom hotkeys on iPad](https://www.maxon.net/en/zbrush/new) | ➖ n/a | S | — | The desktop hotkey editor exists. |
| [Desktop-iPad roundtrip (ZTL/ZPR, GoZ)](https://www.maxon.net/en/zbrush-for-ipad) | ➖ n/a | S | — | No mobile build. |
| [Free vs subscription plan](https://www.cgchannel.com/2024/09/maxon-releases-zbrush-for-ipad/) | ➖ n/a | S | — | Licensing, not a feature. |
| [GoZ iPad to Cinema 4D](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-whats-new.html) | ➖ n/a | S | — | No mobile build. |
| [Home Screen](https://help.maxon.net/zbp/en-us/Content/html/3_interface-and-gestures_home-interface.html) | ➖ n/a | S | — | Could become a recent-files start page. |
| [Map creation, Surface Noise & Mask Adjust on iPad](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-whats-new.html) | ➖ n/a | S | — | See the desktop rows. |
| [Modifier Wheel](https://help.maxon.net/zbp/en-us/Content/html/3_interface-and-gestures_canvas-interface-modifier-wheel.html) | ➖ n/a | S | — | Touch only. |
| [Photogrammetry capture](https://www.maxon.net/en/zbrush-for-ipad-key-features) | ➖ n/a | XL | — | Device capture is out of scope. Importing scans depends on mesh import. |
| [ZBrush for iPad](https://www.maxon.net/en/zbrush) | ➖ n/a | XL | — | Desktop only. |
| [ZModeler, UV Master, NanoMesh & VDM From/To Mesh on iPad](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-whats-new.html) | ➖ n/a | S | — | See the desktop rows. |
| [iPad touch UI & custom UI presets](https://help.maxon.net/zbp/en-us/Content/html/0_ZBrush-ipad-user-guide.html) | ➖ n/a | L | — | Desktop only. |
