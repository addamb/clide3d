# Clide3D user guide

The full feature reference. For an overview, screenshots and setup, see the
[README](../README.md).

A small Maya-style 3D scene editor in Rust (egui + wgpu). The screen follows
Maya 2017+: menu bar with menu sets, status line, shelf, toolbox, Outliner,
viewport with panel menus, Channel Box / Attribute Editor, time and range
sliders, a MEL command line, and the help line. Items not built yet are shown
disabled.

```
cargo run
```

Controls: RMB or Alt+LMB orbit · MMB, Shift+RMB or Shift+scroll (two fingers
on a trackpad) pan · scroll / pinch / Alt+RMB zoom toward the mouse ·
double-click a model to orbit around that spot · click select ·
Q W E R = select / move / rotate / scale · hold X = grid snap · hold D = edit
pivot · 4 / 5 = wireframe / shaded · Space = single / four view · F frame
selected · A frame all · Ctrl+G group · P parent (last selected is the parent)
· Shift+P unparent · Ctrl+H / Shift+H hide / show · Alt+V play · ⌘Z / ⇧⌘Z
undo / redo · ⌘D duplicate · ⌫ delete · ⌘S save · ⌘O open. With a brush
tool on (Sculpt, Paint, Mask, Groups), D / Shift+D step the detail level
and X toggles X symmetry instead (see Sculpt, Paint, Mask and Groups tools
below).

Selection: click, Shift-click to toggle, Ctrl-click to remove, or drag a box.
The last selected object is the lead (green); the others are white.
Outliner: drag rows onto another row to parent them, onto empty space to
unparent; double-click to rename; Shift-click selects a range.

Change any of these hotkeys in Windows > Settings/Preferences > Hotkey Editor.
They are saved between sessions.

The command line runs a MEL subset: `polyCube -n box; move 0 2 0`, `select -cl`,
`select -add box`, `rotate -r 0 45 0`, `group -n grp a b`, `parent a b`,
`parent -w a`, `xform -cp`, `setAttr box.ty 3`, `hide`, `showHidden -all`,
`ls -sl`, `undo`.

## Built-in MCP server (for AI agents)

While the editor is open it serves MCP (Streamable HTTP) at
`http://127.0.0.1:7878/mcp`. Change the port with `CLIDE3D_MCP_PORT`.
It only listens on localhost and rejects non-local browser origins.

Connect Claude Code from any folder:

```
claude mcp add --transport http clide3d http://127.0.0.1:7878/mcp
```

Inside this repo, `.mcp.json` already registers it.

It serves 79 tools; the README lists them all by area, and each section
below covers its own. The basics: `get_scene`, `create_object`,
`update_object`, `delete_object`, `duplicate_object`, `select_object`
(multi), `group_objects`, `parent_objects`, `set_pivot`, `set_camera`,
`frame_view`, `screenshot` (PNG of the viewport or whole window),
`set_material`, `run_mel`, `set_display`, `undo`, `redo`, `history`,
`new_scene`, `open_scene` and `save_scene`. Open/save only accept absolute
`.json` paths and never overwrite a file that is not a scene.

### Sculpting from an image

Agents sculpt with **clay**: soft blobs (spheres, ellipsoids, tapered
capsules, rounded boxes, tori) that melt together into one smooth, colored
surface. Each blob can `add` or `carve`, has a `blend` radius, and can be
`mirror`ed across X for symmetric characters.

- `create_object` with `shape: "Clay"` and `blobs`, then `edit_clay`
  (replace / remove / update / add) and `get_clay`.
- `sculpt`: brush strokes for detail on top of the blobs: `add` / `carve`
  (tubes along a path: brows, lips, grooves), `inflate` / `deflate`,
  `smooth`, `flatten`, and `grab` (pull the surface). Strokes can `mirror`.
  They are stored as a fine grid of offsets saved with the scene.
- `paint`: color over the blob colors: `fill` the whole surface, soft brush
  `strokes` (with `hardness`, `erase`, `mirror`), or `clear`. Paint is a fine
  grid of colors saved with the scene. With `channel: "name"` the same steps
  paint a mask channel (up to 4 per clay) that smart material layers read.
- `project_reference`: shine the reference image onto the model from a view,
  lined up like `compare_reference`. Only visible surfaces that face the view
  get color.
- `set_reference`: load an image to copy (absolute `.png`/`.jpg`) into a
  view slot (`view`: front, back, left, right, top, three_quarter; default
  front), one reference per view. It finds the subject, returns its main
  colors (palette), and overlays it on a viewport pane (front, side for
  left and right, top; `overlay` picks another or none). Without `path` it
  changes that slot's overlay; `clear: true` removes the slot (every slot
  without `view`, and a sheet's image plane of it). The reply's `slots`
  lists the loaded views. Old calls still work: a path alone fills front,
  `side` is the right slot, and `persp` the front slot shown on the
  perspective pane.
- **Turnaround sheets** (`set_reference {sheet: {path, views, boxes,
  height, threshold}, planes, plane_opacity}`; File > Load Turnaround
  Sheet… sends the same call): one image holding several views of the
  subject. It is split into one slot per view (`views`, in reading order,
  default front, right, back; front, back, left and right only) by the
  subjects' boxes: 8-connected parts of the subject mask, parts under 15%
  of the tallest one's height (captions, arrows) ignored, parts whose
  columns overlap merged, left to right. A sheet that does not split into
  as many views as named is refused with the boxes found; pass `boxes`
  (one `[x0, y0, x1, y1]` per view, sheet pixels, inclusive) and each
  view's subject is found inside its box. Every view gets **one scale**,
  the tallest subject's height in pixels over `height` (default the
  model's height at rest; needed in an empty scene), and stands on its own
  ground line (its subject's lowest edge) at the model's lowest point.
  `compare_reference` and `quality_report` then line the model up once at
  that scale for every view (`aligned_by: "sheet"`) instead of fitting each
  view on its own, so a model 10% too big, or too deep in profile, fails
  where a per-view fit would pass; single images keep the per-view fit.
  Each view also stands in the scene as an **image plane** behind the
  model at the same scale (`planes`, default true; `plane_opacity` 0-1,
  default 0.6), seen only from its own side, unlit and casting no shadow.
  Planes are references, not geometry: exports, print, film (unless
  `render_film show_references`), silhouettes, close-ups, framing and
  picking leave them out; `render_views` draws them (`include_reference:
  false` hides them). A saved scene keeps them, and opening it fills the
  slots again. Sheets are at most 8192 pixels a side (checked before
  decoding) and are split at 4096 at most; each view's image is at most
  1024 a side, every view scaled by the same factor. The reply lists each
  view's subject box, cut, ground line and pixels per unit, with an image
  of the split. Caption-like parts big enough to pass the 15% filter
  (wider than tall, under 40% of a view's tallest part, clear of it by a
  gap of rows) are left out of the views' boxes and listed in
  `captions_left_out` with a warning; pass `boxes` if one belongs to the
  subject. Undo, redo and `history goto` bring the slots back with the
  planes (or take them away), deleting a plane removes its slot, rays
  (`surface_point`, picking) pass through planes, and objects parented
  under a plane stay (under its parent) when the plane is replaced.
- `render_views`: one contact sheet of the model from several cameras,
  rendered off screen.
- `compare_reference` (in the rest pose, as `quality_report`): lines the
  model up with the image from one view and returns how well the outlines overlap (IoU), which regions are missing or
  extra, the contour distance, a pixel-to-world mapping, and
  reference | model | overlap images. Each view compares against its own
  slot's image (`front` against the front image), the slot a view object
  names (`{view, slot}`, `{dir: [x, y, z], slot}`), or the only image
  loaded. `views: [...]` (up to 12; default every loaded slot) compares
  several at once and replies per view with `mean_iou` and `worst_view`,
  one row of panels per view; one view replies flat, as before.
  The contour distance (`contour`, default on) goes outline to outline
  both ways: `mean` and the larger 95th percentile `p95`, in world units
  and as `mean_pct` / `p95_pct` of the subject's height. IoU hides a thin
  missing ear; the contour does not (a 60-pixel antenna the model lacks
  moves IoU by 0.8% and the p95 by 48 pixels).
  Close-ups also compare **feature lines** (`features`, default on with
  `center`): Canny edges of the reference (its drawn lines) against the
  model's lines (edges of its concave curvature measured on the field,
  where creases and grooves are, plus occluding contours and normal
  breaks), inside both outlines (2 pixels in, so the silhouette is left to
  IoU and the contour). The reply's `features` gives `chamfer` (the mean
  distance between the two line maps, both ways; world units and
  `chamfer_pct` of the crop's radius), `recall` (the share of the
  reference's lines within 2 pixels of a model line) and `precision` (the
  share of the model's within 2 pixels of a reference line; many creases
  everywhere would raise recall but not precision), and a fourth panel
  draws both (red reference, cyan model, white both). A model with no
  line where the reference has some matches none of them (recall 0, the
  chamfer the crop's diagonal); `features` is skipped only when the
  reference crop has no line. Drawn lines match
  best where the drawing shows the groove as seen (a groove 2 cells deep on
  a head sits inward of the surface drawn around it).
  For a close-up, add `center` (a world point, such as an eye found with
  `surface_point`) and `radius` (half the crop's size in world units): the
  same lineup, cut to that square. The model is rendered again there at
  `size` pixels (64–1024, default 512) and the reference is cut from its own
  pixels, so small features compare at full detail. The reply's `closeup`
  gives `crop_px` (the square on the whole-model panel),
  `reference_px_per_unit` and `interior` (true when the crop is nearly all
  inside both outlines, where a silhouette says little); the IoU, regions
  and `pixel_to_world` are the crop's. A crop under 8 reference pixels
  across is refused.

The loop is: block out blobs, compare, fix, repeat; then sculpt the details;
then fill a base color from the palette, project the reference, and paint the rest.

### Wing membranes

A `membrane` blob makes one rounded thin sheet from 3–20 ordered boundary
`points` in object space. The polygon must be simple when projected onto
its average plane; points can be nonplanar. `thickness` is the full thickness.
`sag` bows the interior along the winding normal, leaving its edges in place.
`scallop` cuts inward by a fraction of edge length on `scallop_edges` (edge
index i connects point i to the next point, including the closing edge).
The curved sheet is triangulated; blending, carve, cover, rigging and baking
use its distance field. Use thickness of at least two mesh cells: thinner
sheets can disappear between grid samples, and `thin_blobs` reports this.

```json
{"type":"membrane", "name":"wing", "points":[[0,0,0],[2,0,0],[1.5,0,1],[0,0,1]],
 "thickness":0.04, "sag":0.12, "scallop":0.1, "scallop_edges":[1,2], "blend":0.03}
```

### Materials and eyes

- Materials set how a surface reflects light; colors still come from blobs,
  paint and the object color. Name one on a blob (`material` in a blob) or an
  object (`material` in `create_object` / `update_object`; for clay it is the
  default for blobs that name none). Presets: `clay` (default), `skin`,
  `lips`, `cloth`, `velvet`, `leather`, `scales`, `hair`, `wood`, `plastic`, `rubber`, `metal`,
  `polished_metal`, `gloss`, `glow`, `glass`, `water`, `crystal`, and the
  stylised `skin_stylized`, `lips_stylized`, `cheek_blush`, `fabric_soft`. `set_material` defines new ones or changes a
  preset for this scene (roughness, metallic, clearcoat, sheen, subsurface,
  pores, mottling, skin_specular, detail) and lists what each object uses. The viewport shades them (GGX highlights, a soft
  studio, a weak light near the camera for catchlights).
- Stylised looks (the soft, saturated, forgiving skin of feature
  animation) are presets too: `skin_stylized` (subsurface 0.6 with
  `subsurface_radius` 1.6, a saturated scatter, no pores, mottling 0.1,
  roughness 0.45, skin_specular 0.4, sheen 0.15 for a soft rim),
  `lips_stylized`, `cheek_blush` (redder skin for cheeks, the nose tip and
  ears) and `fabric_soft` (soft sheen, no weave). Use the name as a
  material, or `set_material {name, preset, ...overrides, apply_to}`:
  `preset` starts again from the preset's values (even over a scene
  material of that name), the other fields override them, `apply_to` (an
  object or up to 64) puts it on objects in the same undo step (it is
  refused with a listing, rename, replace or delete_unused), and the
  reply gives the stored values. `subsurface_radius` (0.1-4, default 1)
  scales any material's scatter distances: wider, softer light under the
  surface; in film its shadow edges measure 1.5x as wide as `skin`'s and
  backlit ears glow through. The attribute editor's material tab has the
  same looks as Stylised Look buttons.
- Detail textures give surfaces real structure: a built-in library of CC0
  scanned PBR maps from [ambientCG](https://ambientcg.com) (see
  `assets/textures/LICENSE.md`): `linen`, `jersey`, `knit`, `felt`,
  `velvet`, `denim`, `leather_smooth`, `leather_rough`, `leather_grain`,
  `suede`, `skin_pores`, `wrinkles`, `metal_brushed`, `metal_scratched`,
  `wood`, `stone`, `scales`. A material names one with `detail` and sets
  `detail_scale` (tile size in scene units; 0 = the texture's real size),
  `detail_strength` (relief: normal and roughness, default 1),
  `detail_color` (how much the texture's light and dark pattern shows in
  the color, 0..1, default 0.35; the hue stays the surface's) and
  `detail_rotate` (degrees). Maps are projected along the object's three
  axes and blended by the normal (triplanar), so clay needs no UVs. The
  viewport, `render_film` and `bake` (into the exported color, normal and
  roughness textures, at a mip level matching the texel size) all show
  them. Presets come with one: `cloth` linen, `velvet` velvet, `leather`
  leather_rough, `scales` scales, `skin` skin_pores, `lips` wrinkles,
  `wood` wood, `metal` metal_brushed; a scene material named like a preset
  keeps the preset's detail unless it sets its own (`detail: "none"` turns
  it off). Images are compiled in (about 10 MB), decoded the first time a
  material uses them and shared by every object.
- Smart materials (like Substance Painter's) are layered looks driven by
  masks, for worn, lived-in surfaces instead of clean plastic. One name
  gives the whole stack, with its own colors: `worn_iron` (dark iron,
  polished worn edges, rust and grime in the crevices), `rusted_iron`,
  `polished_steel`, `gold_trim`, `bronze` (verdigris in the creases),
  `old_leather`, `cracked_leather`, `bone`, `charred_flesh` (black crust,
  glowing ember cracks, ash), `wet_flesh`, `demon_skin` (dark hide, black
  markings, hardened ridges, a faint glow deep in the creases),
  `dungeon_stone` (dust on top, soot and damp in the cracks, chipped
  edges), `mossy_stone`, `grimy_cloth` (grime in the folds, stains, mud at
  the hem) and `blood_splatter` (a layer stack to add on top of another).
  Use the name as a blob or object material. Tune with `set_material` and
  the preset's name: `wear`, `rust` and `dirt` (0..1, default 0.5) spread
  or shrink the layers with that role.
- `set_material layers` builds your own stack (bottom first). A layer is a
  look (`color`, `tint`, `roughness`, `metallic`, `clearcoat`, `sheen`,
  `emission`, `variation`, its own `detail` map) and a `mask`: terms folded
  left to right with `op` (multiply, max, min, add, subtract, screen), each
  with `invert`, `threshold` and `contrast`, from `edges` (convex),
  `cavity` (concave), `ao` (hidden places, sampled from the clay's distance
  field), `up` / `down` / `direction`, `height` (object y range: water line,
  mud on boots), `noise` (`scale`, `octaves`), `grunge` (a library map's
  height as a ragged pattern), `painted` (a channel painted with `paint
  channel`) and `constant`. `{"smart": "worn_iron"}` splices a preset in;
  `add_layers` puts layers on top. Masks default to object space; orientation and height terms can set
  `world:true`. (`edges`,
  `cavity` come from the mesh's curvature, as weathering does).
- Vehicle finishes: `military_paint` adds fine, low orange-peel relief and handling
  roughness; `dusty_khaki`, `olive_drab`, `gunmetal`, and `worn_steel` supply
  paint/metal finishes while preserving authored blob/object colors. Recoloring
  requires an explicit `base_color`, applied before sparse clay paint;
  `keep_paint` defaults to true so flags/stencils survive. `set_material` accepts both fields. `wear` / `damage` presets update **shared materials**:
  every user receives the finish, and later `set_material` base edits still
  apply. Reapplying a preset replaces its layers and generated runoff;
  `amount: 0` disables every layer, `factory_new` removes added wear.
  `battle_worn` adds primer/steel edge chips, fine relief, smudges and cavity
  grime; `desert_dusty` pools dust on top faces and in cavities; `muddy` uses
  visible earth tones on dark rubber. Paint detail uses meter-sized tiles
  (uniform object scaling is compensated; nonuniform scale uses the mean
  axis scale). Color, normals and ORM bake through the same layer evaluator
  and survive GLB export. See [paint checks](../paint_check/README.md).
  World-space multi-target damage sends point stamps to the nearest surface;
  gouge/scrape paths belong to their starting surface. Local-space requests
  deliberately repeat in each target's local coordinates. Material edits
  return changed fields and counts; omit `name` to inspect the full library.
- `decal` stamps a shape on clay at a point, facing out of the surface:
  `rune` (carved; give `emission` and a color to make it glow), `scratch`
  (a cluster showing bright metal), `splat` (blood with droplets),
  `emblem` (a raised ring around a star), or `image` (an absolute .png/.jpg path: its color
  and alpha), with `size`, `rotate`, `color`, `roughness`, `metallic`,
  `emission`, `depth` (relief, negative = cut in) and `seed` (the variant).
- Layers and decals show everywhere: per vertex in the viewport and in
  plain exports (vertex colors), per pixel in `render_film` (CPU and GPU
  tracers alike), per texel in `bake` (color, normal and ORM maps; glow is
  film only). Recipes:
  - plate armor: `worn_iron` on the plates, `gold_trim` on the rim blobs,
    `old_leather` straps;
  - a bloody blade: `{"name": "bloody_steel", "layers": [{"smart":
    "polished_steel"}, {"smart": "blood_splatter"}]}`;
  - a demon: `demon_skin`, `wet_flesh` in the mouth, `charred_flesh`
    wounds, `bone` horns and claws, glowing `rune` decals;
  - a dungeon: `dungeon_stone` walls, `mossy_stone` near the floor,
    `rusted_iron` gates;
  - a peasant: `grimy_cloth` tunic, `old_leather` boots and belt;
  - blood or moss exactly where you want it: `paint` with `channel:
    "blood"`, then a layer with mask `[{"source": "painted", "channel":
    "blood"}]`.
- `create_object` with `shape: "Eye"` makes an eyeball of diameter 1 that
  looks down local +Z: a sharp iris (fibers, dark outer ring) and pupil, a
  cornea bulge, and glossy materials. Paint on clay cannot make an eye that
  sharp. Size it with `scale`, set colors and sizes with `eye`, turn it with
  `aim` (a world point to look at), and add the other eye with `mirror: true`
  (also works for any object: flips across the parent's X = 0 and swaps
  `.L`/`.R`). Put it on the head bone so it moves with the head.

The `lips` preset gives a soft, slightly moist subsurface surface; use a
rose blob/object color such as `[0.70, 0.43, 0.42]`. Material presets set
reflection, so paint supplies the color. Cover both lips continuously,
including their meeting line: two isolated paint strokes leave a pale stripe
through the mouth. Preserve the natural shallow mouth crease. To assign
`lips` to an existing human surface, use a small `op: "cover"` ellipsoid
around the lips with `offset: 0.00002`, `blend: 0`, and material `lips`.
Keep its base color close to skin and paint a soft rose gradient across both
lips and their meeting line; this avoids a hard ellipse of color. Erase old
lip paint first. The 20 micrometer cover follows the existing lip shape.

### Game-ready output

- `bake` makes a clay object game-ready the way studios do: it simplifies
  the mesh to a triangle budget (meshoptimizer), lays it flat into one UV
  atlas (xatlas), and bakes the exact clay surface into textures: color,
  a tangent-space normal map (the detail the simple mesh lacks), and one
  AO/roughness/metallic image. Skin weights carry over. The settings are
  saved on the object; exports then write the baked mesh with its textures
  (the `.glb` embeds PNGs; the `.fbx` writes PNGs next to itself and links
  color and normal maps). `render_views baked: true` or `set_display baked`
  shows the result. Bakes cast short rays at the clay's fine mesh (the one
  `render_film` and fine views build; without one, the rest mesh refined
  where sculpt or paint is finer than it) instead of solving the clay
  field at every texel, and measure weathering once per bake: a 4K bake of
  a whole sculpted body takes about 15-30 seconds, 8K about 30-40 (the elf body from the
  friction log: 15 s, down from 441 s). `exact: true` reads every texel
  from the clay field instead, for comparisons. `texture` goes up to 8192
  for hero assets. Every gap between UV islands is filled from the nearest
  island, so mipmaps of any level never bleed background across a seam.
  Detail regions get more of the triangle budget and more texels
  (`region_density`, default 0.5; 1 makes texels shrink with the region's
  cell). `export_glb` / `export_fbx` with `lods: true` add LOD1-LOD3 (half,
  a quarter and an eighth of the triangles, same UVs and maps, so the
  normal maps keep the finest detail at every level).
- Displacement maps: `bake` with `maps: ["displacement",
  "vector_displacement"]` and an absolute `save_dir` also writes
  `<object>_displacement` and `<object>_vector_displacement` over the
  bake's UVs, for renderers and sculpting apps that displace the simple
  mesh back into the clay. `map_format: "png"` (default) is 16-bit, mid
  grey zero, decoded as `(code / 65535 - 0.5) * scale` with each map's
  `scale` in the reply; `"exr"` holds 32-bit floats in the clay's units.
  `map_size` is 512, 1024, 2048 or 4096 (default the texture size, at most
  2048). `verify: true` reapplies the maps as geometry and reports
  `max_error_cells` and `mean_error_cells`. With the default `lowpoly:
  "meshopt"` the bake looks for the surface only along the simple mesh's
  normal, so the vector map's R and G are about 0: it is the height in
  vector form and keeps no overhangs. With `lowpoly: {poly, level}` (a
  retopology as the game mesh, below) the maps follow the Poly's
  projection and do keep sideways detail and overhangs. The maps are
  one-off files, not saved on the object.

```json
{"target":"Head", "triangles":30000, "maps":["displacement","vector_displacement"], "map_format":"exr", "save_dir":"/abs/out/maps", "verify":true}
```

- Clay resolution goes up to 512 cells across (about 3.4 mm on a full
  body). The meshing grid is stored only in blocks near the surface.

### Hair, surfaces and color edges

- `create_object` with `shape: "Hair"` makes hair cards, the way games do
  hair: thin strips with a generated strand texture (see-through gaps as an
  alpha mask, root-to-tip color), grown along guide curves the agent draws
  (`hair.guides`, root first; `mirror`, `cards`, `width`, `taper`,
  `spread`). Exports mark the material as alpha-masked. `render_film`
  grows real strands from the same guides instead (`strands`,
  `strand_width`, `frizz`): ribbons turned to the camera, shaded
  with shifted reflection (R), transmission (TT), and internal reflection
  (TRT) lobes, colored by pigment absorption. Light also scatters between
  strands. This is a compact Marschner-style approximation, rather than
  a full measured fiber model. Light the lobes miss comes back as a broad
  lobe in the fiber's color (a white fiber loses nothing), paths sample the
  lobes and get extra bounces inside hair, and the set color is converted
  to a per-fiber albedo, so a full head renders the `root_color` /
  `tip_color` you set rather than darker.
- Hair has volume and locks. `volume` (meters, default 0.008) lifts strands
  off the scalp just after the root and spreads them in layers up to that
  far out, denser inside and looser outside. `clump` (0..1) gathers strands
  into locks toward the tips; `clump_size` sets the lock width at the root
  (default: the guide spacing) and `clump_levels` (1-3, default 2) splits
  locks into smaller locks. `flyaways` (default 0.015) is the share of
  stray strands leaving the surface. Strands vary in length
  (`length_variation`, default 0.15), width, and pigment
  (`melanin_variation`, default 0.25: darker and lighter hairs of the same
  hue, slightly sun-lightened tips and outer layer). Game cards follow:
  card layers stack through the same volume, and the card texture is drawn
  from the same model (strands gathering into locks, strays, per-strand
  pigment, lighter tips).
  `hair.grow` places the guides: roots spread evenly over a scalp region of
  a clay head (by default hairline to nape, leaving the face bare), and each
  guide drapes like a hanging chain: combed along the head (back from the
  face, away from a part line) while it touches it, then falling under
  gravity onto the shoulders and back of the body clay below.
- Brows and lashes: `hair.grow style: "brow"` follows the surface along
  `comb` without gravity; `style: "lash"` leaves the lid and curls toward
  `comb`. Supply a small world `center` / `radii` region, or `roots` from
  `surface_point` along the brow/lid (preferred for lashes). Facial defaults
  are 7 mm length, 0.1 mm lift, 0.06 mm strands, narrow tapered export cards,
  and 600 brow / 120 lash strands. Explicit hair settings override these.
  Erase the painted strokes, parent/bone the hair to the head, and use the
  hair object's local X plane for `mirror`. For upper lashes use
  `comb: [0,1,0]`; for a rightward brow use `[1,0.4,0]`. Set `strands` near
  the root count for individual lashes. All coordinates/lengths use meters.
- Scalp hair uses area-distributed roots between several guides, projected
  onto the curved scalp. Locks gather toward the free ends; shorter,
  thinner, sparser edge hairs soften the hairline. Film also adds a soft
  root-color tint to nearby scalp skin, reducing white glints between roots;
  brows and lashes are excluded. Set `hair.center` to the head's
  middle in hair object space, especially for asymmetric or sparse grooms.
  One card stays anchored to each guide to cover boundary roots; the other
  cards share the blended flow, overlap in layers, and have a shallow arch,
  fine alpha strands, root-to-tip color and strand normal relief. Export UV
  U runs across hair, V runs root to tip: tangents follow U, and the
  reconstructed bitangent follows the strand for engine anisotropy. Native
  textured hair cards also use that direction with the film hair shader.
  Scalp card widths adapt to root spacing within `width`; a guide
  `width` overrides this. Card lengths and root starts vary. Each card costs 48 triangles (12 segments, two curved panels).
- `hair` and `hair.grow` reject unknown keys. There is no `grow.region`:
  put `center` and `radii` directly in `grow`. Create/update replies report
  `card_triangles`, `root_spacing`, `largest_root_gap`, `sparse_patches` and
  coverage `warnings`. They also sample up to 256 generated strands against
  nearby head clay, reporting `strands_inside_head` and `collision_check`.
  This sampled check can miss small intersections. Scalp diagnostics cover
  the region spanned by roots; they cannot infer an intended missing region.
  Add guides in thin crown/part areas and keep all guide points outside
  the head. A clay braid or existing hair cap remains visible in film;
  remove an old cap if its hard edge defeats the softer hairline.

- `groom` shapes the guides directly, the way a groomer combs: each stroke
  is a brush along a path of world points (or one `point`) with `radius`,
  `strength` and a `falloff` as in sculpt. Brushes: `comb` (turn segments
  toward `direction`, or along the path's own direction; lengths kept),
  `lengthen` / `shorten` (by `length`), `cut` (keep at most `length`, or
  trim at a `plane` whose normal points at what goes), `clump` (gather the
  guides in the brush toward their centre), `twist` (turn them about that
  centre by `angle` degrees), `smooth` (straighten kinks), `puff` (lift the
  root side off the scalp) and `relax` (even out the spacing between
  neighbours). `affect: tips | roots | all` sets where along each guide the
  brush acts (comb, clump and twist default to tips, puff to roots, the
  rest to all); roots never move. `mirror` (default the hair's own) also
  runs the stroke across the hair's x = 0. After every brush the guides are
  kept at least `hair.lift` outside the clay the hair is under (`collide`
  names another or turns it off), segment lengths restored from the root;
  segments are checked too (sampled at the clay's cell), so a guide cannot
  cross a thin part such as an ear between two outside points: the segment
  turns about the point before it, around the surface. `hair.lift` is what
  `grow` used (0.0015 for scalp hair, 0.0001 for brows and lashes); a hair
  made without grow, or saved before groom, has lift 0.
  The reply gives the guides changed, the mean and largest move, the mean
  length before and after, the points pushed out, and a front and right
  picture (`render: false` leaves it out). Each call is one undo step;
  grooms are not stroke records (no `stroke_log`). At most 64 strokes a
  call, 256 points a stroke, 4,096 guides of 512 points a hair, and a total
  work bound (guide points times path points per stroke, more with a head
  and for smooth and relax): a call over it is refused before anything
  changes; split it.
- Film strands are kept out of the clay the hair is under the same way, at
  `hair.lift`: every strand point near the scalp is pushed out and every
  segment that dips in turns around it, when the strands are built, so
  `render_film` and the hair summary's `strands_inside_head` agree. Strands
  the head does not reach are unchanged. `render_film` replies with
  `hair_kept_out` (per hair: the head and how many strands moved, or why
  they could not be kept out, also a warning). The viewport and exports
  show cards, which are not kept out.
- The Groom tool (toolbox, or `brush_preset set_current {tool: groom,
  groom: comb}`): select a Hair and drag over it; the drag runs across the
  view at the depth you pressed and sends the same `groom` call with the
  HUD's radius, strength and falloff and the shelf's groom brush.

Useful starting settings (meters; supply `grow.on` and appropriate colors):

| Groom | `grow` | Film settings | Game settings |
| --- | --- | --- | --- |
| Long | `count:80, length:0.3, lift:0.002` | `strands:60000, strand_width:0.00008, clump:0.5, clump_levels:3, volume:0.014, frizz:0.0018, flyaways:0.02` | `cards:4, width:0.025, spread:0.004` |
| Short | `count:100, length:0.035, lift:0.001` | `strands:40000, strand_width:0.00007, clump:0.3, volume:0.004, frizz:0.0004` | `cards:3, width:0.015, taper:0.1, spread:0.002` |
| Braid | Draw separate winding guides around the braid axis; `interpolate:false` | `strand_width:0.00008, clump:0.4, frizz:0.0003` | `cards:3, width:0.01, spread:0.001` |

Use a separate interpolated scalp groom above a braid. More strands improve
coverage in close-ups; increasing width makes hair look coarse. Clumping
cannot create a braid from straight guides. The scattering approximation
uses the R/TT/TRT decomposition described in [PBRT's hair model](https://pbr-book.org/4ed/Reflection_Models/Scattering_from_Hair).

- Fine detail: `edit_clay detail_cell` (e.g. 0.0005 on a head) makes the
  sculpt and paint grids finer than the mesh, resampling what is there.
  The `noise` sculpt brush lays pores, bumps, wrinkles, overlapping
  `scales` or belly `plates` over an area in one stroke (it visits only the
  surface, so a whole face or flank is quick); scales and plates point
  their free edges along `normal`. A `detail_cell` too fine for the clay's
  size is refused with the smallest that fits, and a fine mesh that fails
  says why (`fine_mesh_errors`) instead of rendering nothing. Wide
  brushes and paint dabs on a fine grid work too: strokes too big to read at
  once go in tiles, skipping tiles far from the surface.
  `render_film` (by default) and `render_views fine: true` draw a fine mesh
  at that cell size; bakes put the detail in the normal and color maps.
  `detail_regions` make parts of one clay finer still (see "Adaptive
  detail").
- `surface_point` finds exact surface points: look in from a view at screen
  coordinates, cast rays, or snap points to the nearest clay surface.
  One call takes up to 1024 rays and 1024 points; more is refused, never
  cut short, so send batches. A long `at` list is read a page at a time
  with `offset` and `limit` (at most 1024); the reply's `next_offset`
  (null at the end) starts the next page; `at` needs a `view`. Replies
  past about 8k characters are summarized unless `verbose: true`.
  `paint` and `sculpt` strokes take `snap: true` to do the same. These
  tools (and `render_views` / `render_film` unless given a `frame`) always
  work in the rest pose, so points stay right while a clip plays.
- Clothing: a blob with `op: "cover"` dresses everything before it: the
  surface grown by `offset`, kept only inside the blob's own shape (a torso
  box = a fitted tunic, an arm capsule = a sleeve). It fits exactly, has a
  crisp edge and its own color and material, and moves with the rig.
- `color_blend` on a blob sets how wide its color fades into earlier blobs;
  0 gives a crisp color edge (a sleeve end) while the shape blends smoothly.

#### Fur over a body

`hair.grow {fur: true}` covers a whole clay (or chosen parts) with short
fur. Parent the Hair to the clay:

```json
{ "shape": "Hair", "name": "Pip_fur", "parent": "Pip", "hair": { "grow": {
    "fur": true, "on": "Pip",
    "exclude": ["nose", "mouth", "mouth_line"],
    "lengths": [{ "blobs": ["brow.L"], "length": 0.026 },
                { "center": [0, 0.72, 0.17], "radius": 0.12, "length": 0.005 }] } } }
```

- Roots: `count` guides (default 2,000, at most 4,096) spread evenly over
  the surface by area: area-weighted candidates thinned by weighted sample
  elimination (blue noise; the reply's `root_spacing_cv` is the
  nearest-neighbour spacing's variation, about 0.1, where random roots
  give 0.5). Film strands (`strands`, default 160,000, at most the
  200,000 budget; or `density` per square unit, refused when the clay's
  area needs more) grow in each guide's cell on the surface, a sunflower
  spiral cut to the cell, so they cover the body evenly with no bald gap.
- Where: `exclude` and `include` take blob names (the blob making the
  surface at a root decides; `brow.L` also names `brow.R`) or `{center,
  radius}` spheres. The clay's Eye objects are left bare by themselves (no
  root within 1.15 eye radii plus the length) and no strand may enter an
  eye (`eyes: false` turns that off). Exclude the nose and mouth by name.
- Lids: the caps of the clay's `lid` blobs grow very short, dense fur
  (`lid_length`, default 0.35 of the length, at most 0.06 of the lid's
  radius; a few strands a root) lying down the lid toward its margin and
  stopping short of it, so the lid reads as part of the furred face and
  its edge stays a clean bare line over the eye (the eye's bare sphere does
  not apply on its own lid's cap). `lid_length: 0` or excluding a lid by
  name leaves it bare. On bare skin left on a furred character (the lid's
  margin, the ring round the eye) a matte, velvety skin material (Pip:
  `roughness` 0.75, `sheen` 0.6, `skin_specular` 0.2) reads better than
  gloss; give a glossy part such as the nose its own material.
- Comb: a flow field from the rig, by the bone of the blob at each root:
  down the body, out along the limbs (toward each bone's tail), and on the
  head away from the nose (back over the head, out over the cheeks);
  smoothed between neighbours. `direction: [{blobs or center/radius,
  direction}]` overrides it. `lie` (default 0.8) lays the fur flat.
- Length: `length` (default 1.2% of the clay's size) and `lengths`
  regions (first match wins: keep the face short). Regions (and
  `direction` regions) apply to every short strand by its own root, not
  only to the guides rooted in them, so a small part few guides land on
  grows its own fur: darker brow blobs with `lengths {blobs: brow.L,
  0.026}` and a `direction` sphere on each brow (outward and up) read as
  tufts of fur. A blob name names both sides, so give each side its own
  sphere to comb the two brows apart.
- Colour: each strand takes the clay's colour at its root (a blue body
  grows blue fur, a pale belly pale fur); `tint` multiplies it, `tip_color`
  and `tip_mix` shift the tips (default slightly lighter tips).
- Kept out of the clay as other strands are (`groom`'s keep-out), and the
  guides can be groomed. `update_object hair {fur: {...}}` edits the fur
  settings later.
- The viewport shows one short card a guide lying on the surface in the
  clay's colour (48,000 triangles at 2,000 guides, built off the window's
  thread); film draws the strands (on the CPU under `device: auto`).
- Create > Fur on Selected Clay and Create > Eyelids on Selected Clay send
  the same requests.

### Crisp anatomy, thin tips and cloth

`sculpt` adds `crease`, `pinch`, `layer`, and `stretch` (alias `extrude`).
`crease` cuts a sharp V along the path; `strength` is depth in scene units.
Negative depth or `invert: true` makes a ridge. `pinch` compresses surface
features toward the line, sharpening a lip border, jaw or horn ridge;
its strength is 0–1. `layer` raises a plateau by a fixed signed height
(`strength`, scene units) with a flat influence across its middle. Dabs
and segments within **one stroke** do not accumulate height; separate
strokes deliberately build another layer. Negative height lowers it.

Use `stretch` for elf ear tips, horn tips or wing fingers. It integrates
small continuous pulls along `delta`, tapering around the moving path;
the continuous deformation stays connected. Delta may span up to eight
radii. Center the brush on the tip, include its base in the radius, and
keep the narrowest visible part at least two mesh cells thick (or use a
finer mesh). Grid sampling can still lose sub-cell features. `grab` keeps
its existing broad displacement behavior; use `stretch` for thin parts.
All sculpt operations remain deterministic, saved as detail, and undoable.

Every brush accepts `mask: {blobs:["flank"], above:0.4, below:1.2}`.
Names restrict the nearest pre-stroke blob surface, mirrored copies share
the label, and height limits intersect with that restriction. Heights are
always **object-local Y**, even with world-space stroke points. Unknown
names are errors; an empty name list masks everything. Masks also apply
to large tiled strokes and surface-band noise.

### Brush controls: falloff, pressure, spacing, symmetry, stroke kinds

Every `sculpt` and `paint` stroke takes the same controls (they resolve
into dabs before anything is applied, so a stroke replays the same way
whatever the camera or selection):

- `standard`: dab by dab, the surface rises along its area normal (the
  mean normal over the middle of the dab) by `strength * radius / 2` at the
  center, following the falloff; `invert` carves. Along a path the dabs are
  spaced `spacing` x radius apart (default 0.15) and scaled so the ridge
  rises about as high as one dab.
- `falloff`: `smooth` (the classic clay curve), `linear`, `sharp`, `flat`
  (a plateau to 0.9 of the radius) or `sphere`, or `[[t, value], ..]` (up
  to 16 points, a monotone cubic), or `{preset | curve, focal_shift}`;
  `focal_shift` (-1..1) makes the dab narrower (+) or fuller (-). Strokes
  without one keep the curve they always had.
- Points can carry pressure: `[x, y, z, pressure]` or
  `{p, pressure, normal, t}`. `pressure: {size, strength}` maps it
  (`linear` by default, `off`, or `[[pressure, factor], ..]`).
- `symmetry: {axes: ["x", "y", "z"], radial: {axis, count}, about:
  pivot | origin}`; `mirror: true` is `axes: ["x"]`.
- `kind`: `path` (default), `dots` (a dab per point), `{spray: {count,
  jitter, seed}}` (scattered dabs; the same seed gives the same dabs),
  `{drag_rect: {from, to}}` (one dab as wide as the drag) or `drag_dot`
  (one dab where the drag ends) or `{curve: {closed}}` (the points are
  control points of a smooth centripetal Catmull-Rom curve, back to the
  first when `closed`). `lazy` (0 off, or 0.01 to 1) smooths the path like
  LazyMouse; `taper: [start, end]` fades the ends (a crease with taper is
  the Dam Standard).
- A curve stroke's record keeps its control points, so moving one is
  `stroke_edit {action: replace, id, spec}` with that point changed
  (MoveCurve): the layer replays and the result equals a fresh stroke
  through the new points. There are no curve handles in the viewport yet.
- `smooth_mode`: `gentle`, `normal`, `peaks` (only lowers) or `valleys`
  (only raises). `flatten` takes `side: above` (only cuts) or `below`
  (only fills).
- `frame`: `world` (the call's `space`), `object`, `{surface: {offset}}`
  (points snapped onto the clay, then lifted by offset) or `{screen: {view,
  at: [[h, v], ..]}}` (rays from a named view, like `surface_point`) or
  `{screen: {camera: {view, proj, viewport}, px: [[x, y], ..]}}`, or
  `{project: {along, plane, both_ways, reach, offset}}` (below).
- `frame: {project: ...}` casts each point (in the call's space) onto the
  clay instead of snapping it to the nearest surface point. Where two
  blobs meet at a concave junction, nearest-point snapping leaves a
  strand's plane and bunches points up at the junction; a projected stroke
  stays where it was drawn. `along` is a direction `[x, y, z]`,
  `"view:front"` (back, left, right, top, bottom: that view's rays) or
  `"plane"` with `plane: {normal}`: the rays then lie in the plane through
  each point, across the path there (its direction over a radius each
  way, so the rays fan across a bend). `both_ways` (default true) casts
  backward too and the nearer hit wins; with false, plane rays go toward
  the clay's middle. `reach` (default 4 radii) is the farthest hit taken
  and `offset` lifts each point along the surface normal. Each ray starts
  a quarter radius behind its point (distances still count from the
  point), so a point already on the surface lands where it is, not on the
  far side of a thin sheet. Points that hit nothing are refused by index
  before anything is recorded. A curve's smooth path is cast again after
  it is made, and a spray's jittered dabs are each cast along their
  center's ray (a dab that lands on nothing is left out, the same ones on
  every replay). The record keeps the points
  where they landed (a replay casts nothing), and its spec keeps
  `project`, so `stroke_edit replace` with it projects again (onto the
  surface as it is then).

  ```json
  {"target":"Head", "strokes":[{"brush":"crease", "points":[[0.1,0.16,-0.15],[0.1,0.02,-0.25],[0.1,-0.05,-0.4]], "kind":{"curve":{}},
   "radius":0.03, "strength":0.002, "frame":{"project":{"along":"plane", "plane":{"normal":[1,0,0]}}}}]}
  ```
- `paint: {color, hardness, strength}` on a sculpt stroke colors the same
  stroke as it sculpts (MRGB); a `paint` call's strokes are the same spec
  with color.

`noise` with `pattern: "folds"` makes soft draped folds running along
`normal` (default `[0,-1,0]`, gravity). `scale` is fold spacing; strength
sets amplitude as `strength * scale / 2`. Use a path over the sleeve,
skirt or cape, with a radius covering the cloth and a mask if needed.

A cover accepts `smooth` (alias `loose`) in object units: a low-pass body
envelope with clearance, bridging pits and concealing small protrusions.
Zero preserves the original exact fit. Try 0.02–0.04 for a tunic and
0.02–0.05 for boots on a meter-scale human. Smoothing adds looseness;
`offset` adds further stand-off. Enlarge the limiting cover box/capsule so
it contains the resulting garment. Covers remain solid over the body.

A `chain` blob creates connected ellipsoidal beads evenly along its path,
joined by a narrow core. `size` gives full bead width, length along the
path and depth; `twist` rotates successive beads about the tangent in
degrees; `taper` is end/start size ratio (default 1); `tilt` leans the
beads left and right in turn (degrees). Paths have 2–512
points (as do sweep, band and array paths) and at most 512 beads; long paths increase spacing at that limit.
Keep the connecting core at least two mesh cells across; `thin_blobs`
warns about undersampled links. Color, material, blend, mirror, carve, cover and rigging work as for other
blobs. For a braid, use flat beads that lean in turn:

```json
{"type":"chain", "name":"braid", "points":[[0,1.7,-0.1],[0,1.5,-0.13],[0,1.2,-0.1]],
 "size":[0.035,0.045,0.022], "tilt":35, "taper":0.4, "blend":0.005}
```

Useful starts for models in meters (set `detail_cell` several times finer
than the feature spacing, and render a fine mesh to inspect details):

| Surface | Settings |
| --- | --- |
| Skin | crease/layer depth 0.0005–0.002; pinch 0.2–0.5; pores strength 0.03–0.08, scale 0.0008 |
| Cloth | crease depth 0.002–0.006; folds strength 0.5–0.9, scale 0.02–0.05 |
| Creature | pinch 0.4–0.7; scales strength 0.3–0.7, scale 0.03–0.06; layer height 0.005–0.015 |

```json
{"target":"Elf_head", "space":"local", "strokes":[
 {"brush":"crease", "points":[[-0.02,1.65,0.08],[0.02,1.65,0.08]], "radius":0.004, "strength":0.001},
 {"brush":"stretch", "point":[0.09,1.72,0], "radius":0.012, "delta":[0.018,0.025,0], "mask":{"blobs":["ear"]}}
]}
```

### ClayPolish: lumps off, creases kept

`smooth` blurs the field, so it rounds a lip line or a hard edge as fast
as a lump. ClayPolish (`edit_clay polish` over a region or the whole clay,
or the `clay_polish` brush under each dab) is mean-curvature flow on the
sculpted surface with a guard read again before every pass: where the
normal turns by more than `angle` (default 30 degrees) across a detail
cell, a crease or an edge, the surface stays; where it is a long,
strongly curved line (one principal curvature at least 0.15 per cell and
much larger than the other: lip and lid lines, grooves) it stays too;
short isotropic lumps flow away. With `keep_volume` (default true) each
pass is shifted back to the volume it had, so a polished ball keeps its
size. On a lumpy box with a crease groove, `amount: 0.6` took the lumps
to 23% of their height and kept the 90-degree edge to 0.1 degrees and the
groove's depth to 99.5%; plain smooth for as long rounded the edge to 147
degrees and filled the groove to 17% of its depth.

```json
{"target":"Head", "polish":{"amount":0.5, "region":{"center":[0,1.62,0.08], "radius":0.06}}}
```

- `amount` 0..1 is how long it flows: amount x 128 passes (default 0.1,
  13 passes: it takes the lumps `quality_report` reads, up to about 4
  cells across, and keeps short grooves on a curved head; 0.5 rounds
  them to 36-52% of their depth); `passes` (a whole number 1-256; 20.0 counts) overrides it. The flow is a heat equation
  on the surface, so small lumps go first: lumps two cells across fall to
  about a quarter by 0.6, wide swells take longer.
- `region` is `{center, radius or radii (along the clay's axes), space}`,
  world space by default (`space: "object"` for the clay's own); it fades
  out over its outer fifth and is clipped to the clay, so a region larger
  than the clay polishes all of it. Without it the whole clay polishes.
- The persistent mask and hidden polygroups hold points still, as they do
  a stroke's (the record keeps what it read by hash, so it replays the
  same after the mask changes); the surface around them flows up to them.
- It runs on what you see (the blobs and every enabled sculpt layer) at
  the detail cell and in every detail region it reaches, and writes
  offsets to `layer` (default the recording layer, else base).
- It is one record on that layer's ledger (`stroke_log`: kind `action`,
  with the call's polish and, with `full`, the resolved action: region in
  the clay's space, passes and the cell it ran at). It replays with the
  same kernel, the same on any thread count; `stroke_edit` disables,
  deletes or moves it.
- At most 3M surface points in each detail level it reaches (the coarse
  grid and each detail region count apart): the bound is checked while
  the surface is read, which stops at the cap, and the call is refused
  naming a smaller region or a coarser cell. A level's working memory
  stays under about 135 MB (a smooth 1M-point band takes about 35 MB).
- About 1M points take 0.16 s for 8 passes (1.3 s at the default 64).
  With `polish`, `edit_clay` runs as a background job: the window stays
  live, later edits wait in line behind it, and the reply waits for the
  mesh up to `wait_s` as without polish (past about 40 s it answers with
  a job id; see `job_status`).
- The guard keeps any long, strongly curved ridge, a seam between two
  blobs too: lower `angle` or polish that region alone.

### Measuring quality: lumpiness, curvature and quality_report

`quality_report` measures a clay in one call and says where to fix it.
The loop is: `quality_report`, fix the worst (an `edit_clay polish` with
the region it names, a stroke, a blob), and report again until it passes.

```json
{"target":"Head", "regions":[{"name":"face", "center":[0,1.62,0.08], "radius":0.08}]}
```

- **Lumpiness** is measured on the clay's field, not on the mesh or a
  camera (D12), so it reads the same at any mesh resolution. At points
  spread evenly over the surface (`samples`, default 40,000; 1,000 to
  200,000), it fits a smooth surface through the surface around each point
  (24 heights within 4 cells, a weighted quadric) and takes how far the
  surface lies off it: a band-pass residual that reads lumps up to about
  4 cells across in full, longer swells less. The score is the RMS in
  cells, the finest sculpt cell at each point (else `detail_cell`, else the
  mesh cell). A sphere with lumps of RMS 0.05 to 0.4 cells scores 0.034 to
  0.29 against its surface's own RMS of 0.035 to 0.27; a clean sphere 0.0002.
- **Intended detail** is left out: the footprints of texture strokes
  (`noise`, `crease`, `pinch`, `vdm`, any brush with an alpha) and sharp
  edges. Along a line (a groove, a ridge, a thin tube, an edge: one strong
  principal curvature), the fit runs along the line, so the line's own
  section is kept and lumps along it still count. A carve that is meant to
  be there (a dimple) can be left out with `regions: [{..., exclude: true}]`.
- The reply gives `pass`, `failures` (`"lumpiness face 0.081 > 0.05"`),
  `hints` (a ready-made polish at the worst lump, at polish's default
  amount 0.1, a light pass that keeps short grooves;
  legacy sweeps that own lumps, with the update that upgrades them),
  `lumpiness {overall, regions [{name, score, worst_at}], worst}`,
  `curvature {p95_abs_h_cell}` and `topology {watertight, non_manifold,
  min_wall, thin_at, triangles}`. `min_wall` is in world units along each
  point's normal through the object's scale (a squashed shell is as thin
  as it looks). A report that measured nothing does not pass: no point
  measured, most samples unfit, every point intended detail or excluded,
  or a region holding no counted point each fail as `unmeasured`. `worst` lists up to 8 points lumpier
  than the threshold, at least 16 cells apart, with their residual in cells
  (+ above the smooth surface, - below). Points are world space at rest,
  as `sculpt` and `polish.region` take them.
- **Close-ups:** `closeups: [{name, center, radius, view, size}]` (up to
  16) compare each square as `compare_reference` does with `center`, with
  feature lines: `closeups [{name, view, iou, feature_pct, recall,
  precision, reference_line_px, model_line_px, interior}]`, failing
  `feature_pct` over the threshold, or no model lines where the reference
  has some.
- **Silhouettes:** with reference images loaded (`set_reference`, one per
  view slot), each view is compared as `compare_reference` does, drawing
  only the target and the objects under it: `silhouette {views [{view,
  slot, iou, contour_pct, contour_p95_pct, missing, extra, regions}],
  mean_iou, worst_view}`. `views` picks them (default every loaded slot;
  none loaded: skipped). More than 6 views run as a job.
- Thresholds (D14) default to lumpiness 0.05, silhouette IoU 0.95,
  contour 1% of the subject's height (the mean, `contour_pct`) and 3% (the
  95th percentile, `contour_p95_pct`: a thin missing antenna or limb that
  IoU and the mean barely see), close-up features 1.5% of the crop radius
  and walls 0.002 units; `thresholds: {...}` overrides any.
- `images: true` adds the curvature and lumpiness sheets of the target:
  the mesh the viewport draws (fine or baked when shown), colored from
  what was measured and drawn at rest (a job draws the clay as it
  measured it).
- Over 40,000 samples it runs as a background job. The reply is the same
  every time and on any thread count.

`render_views shade` draws the same measures: `curvature` (mean curvature
x cell, blue concave, white flat, red convex, clamped at +-0.3), `lumpiness`
(white smooth to magenta at 0.2 cells, intended detail grey), `normals` and
`wire` (mesh edges over the beauty render). Without `shade` it draws the
viewport's (`set_display shade`, wire too); an explicit one wins over the
viewport's. `set_display shade` (and
Shading > Quality in the viewport menu, which sends the same call) shows
one in the viewport, as does the Shade row in the brush HUD. Curvature and
lumpiness are measured again when a clay's mesh changes, on another thread
and only while a shade is on: while strokes come in the clay draws without
the colors (its sculpt chunks), and they show again once the strokes pause
and the mesh is measured.

### Proportion guides: head units, lines and targets

`guides` keeps proportion guides with the scene (saved; `set` and `clear`
are undo steps) and measures the model against them. Guides are not
objects: they are drawn over the viewport (`set_display guides`, on by
default; Display > Proportion Guides) and over `render_views guides:
true` tiles after the render, so exports, print, film, masks, silhouettes
and rays never see them.

```json
{ "action": "set", "target": "Creature",
  "head_units": { "count": 3 },
  "lines": [{ "name": "eye_line", "y": 0.62 }, { "name": "center", "x": 0 }],
  "targets": [
    { "name": "head_ratio", "of": ["head"], "value": 0.4, "tolerance": 0.02 },
    { "name": "eye_spacing", "between": ["eye.L", "eye.R"], "per": "head_unit", "value": 0.35, "tolerance": 0.02 },
    { "name": "eyes_on_line", "between": ["eye_line", "eye.L"], "value": 0, "tolerance": 0.01 }
  ] }
```

- `head_units {count 2-8, bottom, top}`: a line every (top - bottom) /
  count (world y). Without `bottom` or `top` they come from `target`'s
  height and are stored as numbers, so the guides stay put while the model
  changes.
- `lines`: at most 32 named planes, `{name, y}`, `{name, x}` or `{name, z}`.
- `targets`: at most 32, each `{name, value, tolerance}` and either a
  ratio, `of: [parts]` over `to` (the parts' extent along `axis`, default
  y, together; `to` is `total_height` (default), `total_width`,
  `total_depth`, `head_unit`, `sheet_height` or a part), or a distance,
  `between: [end, end]` (a part's centre, a line's plane or a world point;
  euclidean, or signed along `axis`, or along the line's axis), perhaps
  `per` a length (`total_height`, `total_width`, `total_depth`,
  `head_unit`, `sheet_height` or a part: the part's height, whatever
  axis the distance takes, unless `per_axis` names another; `set` stores
  the y, and guides saved before `per_axis` keep their old meaning, the
  distance's axis).
- A part is a blob of the measured clay by name (both sides of a mirrored
  blob; `eye.L` with mirror on is the side as written and `eye.R` its
  copy), else an object by name (eye objects, other clays by their field,
  the rest by their mesh).
- Each `set` replaces the parts it names (null clears one); `clear {parts}`
  removes some or all; `get` lists them.

`measure {target}` replies the model's height, width and depth; with head
units, the unit, the model's height in heads and every named blob's top
and bottom in heads counted down from the top line; each target's
measured value, error and pass (or the problem: a name that resolves to
nothing); `pass` and `failures`. With a turnaround sheet loaded, `sheet`
gives each view's subject height (and in heads), the model's height over
it, and at each y line the sheet subject's width and the model's section
across the view. Extents are measured on the field, not the mesh: a
blob's on its own shape (all of it, also where it is buried in another
blob), the model's on the composed surface, to about 1e-5 at any mesh
resolution. The search goes box by box from the outside in, skipping only
boxes the field shows to be empty, down to a quarter of the clay's cell
(or 1/1024 of the model, when that is coarser), so a thin part reaching
past the rest (a horn, an ear tip, a finger) is found even where it is
about as thin as the cell; a part much thinner than that can still be
missed. For a part sunk into another, pick landmarks with
`surface_point` and measure `between` the points. The reply is the same
every time.

`quality_report` includes the guides as `proportions` when any are set,
and every target out of tolerance is a failure (`proportions: false`
leaves them out). Display > Measure Proportions sends `guides measure`
for the selection and shows the reply in the Proportions window.

### Clay brushes, alphas, presets and posable symmetry

The plane brushes work dab by dab toward the plane of the area under the
dab (its mean surface along the mean surface normal), so they follow the
form and fill or cut against it:

- `clay` fills hollows up to that plane, with a square footprint; it never
  cuts (`invert` cuts instead). `clay_buildup` fills them at once and lays
  a layer over the plane, about `strength * radius / 10` per stroke, so
  hollows fill first and passes build up.
- `clay_tubes` lays an even strip `strength * radius / 2` high along the
  path over the surface the stroke starts on, flat across; overlapping
  dabs do not stack, so the ridge stays even.
- `trim_dynamic` cuts what stands above the plane (never adds) and
  `polish` presses both ways onto it; both have a flat, sharp-edged
  falloff unless given one.
- `clay_polish` is curvature flow under each dab that keeps creases, hard
  edges and long lines (see ClayPolish below); strength sets its passes
  per dab (1 to 8), `angle` (default 30) and `keep_volume` (default true)
  tune it.
- `snakehook` pulls a horn out of the surface along `points` (the first on
  the surface, the rest out in the air, up to 16 radii; or one point and
  `delta`). `curl_deg` curls it about `normal` (default level, across the
  pull): the tip ends `curl_deg` degrees from the base direction.

Alphas are 16-bit grayscale images in the scene's library:
`alpha {action: import, name, path}` reads a PNG (8 or 16 bit), JPEG or
OpenEXR (channel Y, else R; values 0..1), at most 1024 pixels a side; `from_view {name, target, view, size}` captures a
clay's height map from a named view; `list`, `preview` and `delete` manage
them. A stroke names one with `alpha: {name, depth, mid, rotate, scale,
mode: stamp | drag | tile, use: height | mask}`: on `standard` (use height)
the dab rises `depth * (gray - mid)`; on any dab brush, use mask scales the
dab's weight. Records keep the image's content hash and each clay keeps
the images its strokes use, so strokes replay without the library.
Importing a name again with other pixels marks the records made with the
old image `stale_context`; `stroke_edit {action: rebase}` takes the new one.

```json
{"target":"Slab", "strokes":[
 {"brush":"standard", "point":[0,0,0], "radius":0.1, "falloff":"flat", "alpha":{"name":"checker", "depth":0.01}},
 {"brush":"clay_buildup", "points":[[-0.2,0,0.1],[0.2,0,0.1]], "radius":0.05, "alpha":{"name":"ring", "mode":"drag"}}
]}
```

VDM stamps (Alpha 3D) hold shapes a height image cannot: overhangs,
horns that curl over, undercuts. `alpha {action: create_vdm, name,
target, center, radius}` captures one from any clay as sculpted: a box
`radius` across each way, standing on the surface at `center` (moved onto
it along the normal), from `below` (default radius / 2) under the surface
to `above` (default 2 x radius) over it, each at most 8 radii. Only what
stands out of (or sinks into) the flat base plane is kept, as a sparse
volume at `cell` (default the clay's detail cell, coarsened to at most 2M
lattice points). `normal` and `tangent` turn the stamp; `space: local`
takes the values in object space. `alpha list` shows it as `kind: vdm`.
`brush: "vdm"` stamps it under each dab, overhangs included:

```json
{"action":"create_vdm", "name":"horn", "target":"Sculpt", "center":[0,1.2,0.1], "radius":0.04}
{"target":"Head", "strokes":[{"brush":"vdm", "alpha":{"name":"horn"}, "points":[[0.03,1.75,0.05]], "radius":0.04, "kind":"drag_dot"}]}
```

The stamp's width is the dab's (a radius equal to the captured one stamps
it at its own size) and its up is the area normal. `strength` (default 1)
blends it in, `invert` carves its shape in (a horn becomes a hole),
symmetry mirrors it, and spacing defaults to 2 (a stamp's width apart);
a single point or `drag_dot` stamps once. A vdm alpha takes `rotate`,
`scale` and `mode: stamp | drag` but no `depth`, `mid`, `use` or `tile`.
Stamps are stored, hashed and rebound like images, so strokes replay
without the library. Importing VDM images made in other tools is not
supported yet.

Brush presets are a stroke's settings without its points. Twenty-one are
built in (Standard, Clay, ClayBuildup, ClayTubes, DamStandard, TrimDynamic,
hPolish, Inflate, Move, Pinch, Snakehook, Layer, Flatten, Planar, Smooth,
SmoothPeaks, SmoothValleys, Pores, Wrinkles, Scales, and MaskPen for the
mask tool's strokes); `brush_preset {action:
save, name, spec}` (or `from_record: {target, id}`) saves one in the
scene, and `list`, `get` and `delete` manage them. `sculpt` takes `preset`
per call or per stroke: the preset goes under the stroke, the stroke's own
fields win, and the record keeps the merged stroke. The Sculpting shelf's
preset list sends `brush_preset {action: set_current, preset}`, so the
human's strokes carry the same preset.

Posable symmetry, `symmetry: {axes: ["x"], posable: true}`, sculpts a
posed character: the points are on the clay as posed at the editor's frame,
each goes back to the rest pose through the skin weights where it is, the
copies are made there, and the rest pose is sculpted. A stroke on the
posed left arm lands on both arms at rest, as if made there.
`symmetry.frame` names the pose (a clip frame, or "rest"); left out, the
editor's frame is used and written into the record and the reply
(`pose_frame`), so a stored stroke sent again lands the same wherever the
timeline is.

### Masks, polygroups and the masked transform

A clay has a persistent sculpt mask: 0 (free) to 1 (frozen) on its
surface. Every `sculpt` and `paint` stroke moves each point by 1 - mask,
so a mask protects what it covers until it is cleared. The `mask` tool
edits it with a list of actions, each a record on the clay's mask ledger
(`stroke_log {layer: mask}`):

```json
{"target":"Head", "actions":[
 {"lasso":{"view":"front", "at":[[-0.1,1.5],[0.1,1.5],[0,1.7]], "through":false}},
 {"grow":2}, {"blur":1},
 {"paint":[{"preset":"MaskPen", "points":[[0,1.62,0.1]], "radius":0.02}]},
 {"by_cavity":{"mode":"add"}}
]}
```

- Painting: `{paint: [strokes]}` are `mask_paint` strokes with every
  stroke control (falloff, alpha with `use: mask`, symmetry ...);
  strength is how far the mask rises, `invert` lowers it.
- Selections: `lasso` and `rect` (a named view with `at` / `from`, `to` in
  world units, or a `camera {view, proj, viewport}` with pixels; the
  record keeps the camera, so sending it again selects the same however
  the viewport moved; `through: false` takes only surface facing the
  camera), `by_cavity`, `by_curvature`, `by_normal {dir, angle}`,
  `by_height {axis, min, max}`, `by_blobs {names}`, `by_groups {ids}`,
  `connected {seed_point}`. Each takes `mode`: set (default), add or sub.
- `"invert"`, `"clear"`, `"all"`, `{grow: cells}`, `{shrink: cells}`,
  `{blur: cells}` (walking the surface band, so a grow never jumps a thin
  wall; blur keeps the mask's sum), `{sharpen: amount}`.
- `{to_channel: name}` and `{from_channel: name}` copy to and from a smart
  material's painted mask channel.

A stroke can also mask itself: `mask: {channel: false}` ignores the
persistent mask, `invert_channel: true` works on the masked part instead,
`groups: [ids]` keeps to some polygroups, and auto-masking reads the
surface around each dab: `backface` (leave surface facing away alone),
`normal_angle` (degrees from the dab's normal), `cavity` (-1..1: positive
spares cavities, negative spares ridges) and `connected` (Move
Topological: only surface joined to the dab's center within 1.5 radii, so
a grab moves one finger and not its neighbour). Each record keeps the mask
and polygroups it read (by hash, on the clay), so it replays the same after
they change.

The `transform` brush moves, turns or scales the unmasked part:

```json
{"target":"Body", "strokes":[{"brush":"transform", "transform":{"rotate":[0,0,-30], "pivot":[0.18,1.42,0]}}]}
```

Mask what should stay (the torso), then transform the rest (the arm about
its shoulder). Offsets are capped at 127 detail cells: big changes of form
belong to blobs. With a mask on the selected clay, the Move, Rotate and
Scale gizmo sends this same stroke on release instead of moving the
object.

Polygroups are ids on the surface with a name and visibility each; the
`group` tool's actions are records on the group ledger: `"from_blobs"`
(group n + 1 is blob n, both sides of a mirrored blob in one, named after
the blob or `blob n`),
`{from_mask: {threshold, id}}`, `{from_normals: {angle}}`, `{paint: {id,
strokes}}`, `{rename: {id, name}}`, `{show|hide|isolate: {ids}}`,
`"show_all"`, `"clear"`, `"list"`. Brushes skip hidden groups and the
viewport leaves them out. `set_display {overlay: mask | groups | none}`
picks how the viewport shows the mask (masked surface darker, the
default) and groups (each tinted); film renders, bakes and exports never
show either. `probe_clay`, `surface_point` and `get_clay` report the mask
and group at a point and the mask's coverage.

### Sculpt and paint layers, morphs and blend shapes

A clay's layers, bottom first: `base` (its own sculpt) and `paint` (its
own paint), then up to 14 more made with `sculpt_layer`:

```json
{"target":"Head", "action":"add", "name":"wrinkles"}
{"target":"Head", "layer":"wrinkles", "strokes":[{"brush":"crease", "points":[[-0.03,1.68,0.09],[0.03,1.68,0.09]], "radius":0.006, "strength":0.001}]}
{"target":"Head", "action":"amount", "name":"wrinkles", "amount":0.5}
```

The clay shows base plus each enabled sculpt layer times its amount
(-2..2), and paint layers (`kind: "paint"`, painted with `paint {layer}`)
laid over paint at their amount (opacity 0..1); meshes, bakes, film
renders and exports all take that. A stroke on a layer reads every
enabled layer (what you see) and writes its own, divided by its amount, so
amount 1 is what the brush did, 0 hides it and 2 doubles it; strokes are
refused below amount 0.05. Each layer has its own stroke ledger
(`stroke_log` / `stroke_edit {layer: "wrinkles"}`). Because strokes read
the other layers, changing one (its amount or new strokes) marks the
others' records `stale_context`; `stroke_edit {action: rebase}` replays
them against the layers as they are.

Other actions: `enable` / `disable`, `rename {to}`, `move {to}`, `bake`
(fold the layer into base at its amount: the surface stays, the layer's
records stay readable as `stroke_log {layer: "archived:NAME"}`),
`merge_down`, `delete`, `split_by_mask {name, to}` (the masked part of a
layer moves to a new one), `record {name}` (the layer `sculpt` calls
without `layer` write, saved with the clay) and `select {name}` (the
layer the editor's Sculpt and Paint tools stroke; editor state, never an
undo step). base and paint stay at amount 1. Layers, morphs, the mask and
groups share one grid budget (`grid_mb` in `get_clay`; at most 512 MB).
A file has no sharing, so a clay with layers or morphs is also held to
1 GiB of saved grids (`saved_mb` and `max_saved_mb` in `sculpt_layer
list`): its own grids, its ledgers' outside-edit grids, and every morph's
grids and ledgers again. A full-size sculpt (262,144 bricks) saves about
260 MB, so that is the clay and about 3 morphs, fewer once its ledgers hold
outside-edit grids (a `detail_cell` change records the whole grid; `bake`,
`blend` and `split_by_mask` the bricks they changed). A call that would
pass either budget is refused and changes nothing.

`morph` keeps snapshots of the sculpt layers (grids, ledgers, amounts),
shared rather than copied: `store {name}`, `switch {name}` (swap the
clay's sculpt with the morph: switching again goes back), `blend {name,
weight}` or `{weights: {name: weight}}`, `delete`, `list` and `diff`
(how far each is from the clay). `sculpt_diff {from: "morph:NAME"}`
measures against one. Morphs are free in memory but each one is written
out again in a file, so the 1 GiB saved budget above is what limits them.

Blend shapes: `export_glb` and `export_fbx` take `blend_shapes:
["morph:smile", "layer:wrinkles"]` (a layer's shape is the exported mesh
with that layer fully on: export with it off or at 0). Each rest vertex is
moved onto the target surface, so vertex counts stay the same; a shape
the vertices cannot follow (a long pull stretches the surface past them)
is refused with its residual (`max_residual`, default half a mesh cell).
`check_export {morph_weights: {"morph:smile": 1}}` shows the file with a
target applied.

### Importing meshes, clay from anything, booleans and printing

`import_mesh {path}` reads `.obj`, `.stl` (binary or ASCII), `.ply`
(ASCII or binary), `.glb` (every mesh at its node's placement) and binary
`.fbx` (its Geometry nodes, in meters by the file's UnitScaleFactor; model
transforms are not applied). Files are capped at 1 GiB and 4M triangles,
and malformed ones are refused. `as: "clay"` (the default) makes a clay
with one mesh blob: the triangles (decimated to at most 200k for the
distance field) kept with the clay, so it saves, sculpts, paints and
replays like any other. A mesh that is not closed is read by its
generalized winding number, so its holes close (`close_holes: false`
refuses it instead). `as: "mesh"` makes a Mesh object that draws the
file's triangles; `close_holes` fills its boundary loops. `scale`, `up:
"z"` (a Z-up file) and `center` place it; File > Import… sends the same
call.

`to_clay` turns objects into clay and combines them (Make PolyMesh3D,
DynaMesh, Remesh All and booleans across objects):

```json
{"target":"pCube1"}
{"targets":["Hull","Cutter"], "op":"carve"}
{"targets":["pSphere1","pCube1"], "op":"intersect"}
{"target":"Scan", "mode":"volume", "resolution":160, "blur":1, "polish":0.5}
```

Primitives become analytic blobs (a cube a box, a sphere a sphere or
ellipsoid, a cylinder or cone a lathe, a torus a torus, a plane a thin
slab); clay, imported meshes and every other mesh become mesh blobs of
their mesh. The first target is converted in place (it keeps its name,
placement and children) and the others are added with `op` (`add`,
`carve`, `intersect`) and hidden; `into` puts every target into an
existing clay instead. `mode: "volume"` remeshes the result into one
closed surface at `resolution` (sculpt and layers folded in), with `blur`
(a Gaussian over the field, in cells) and `polish` (curvature flow) kept
as sculpt offsets and the volume kept. Modify > Remesh as Clay… sends it.
Blobs also take `op: "intersect"` directly in `edit_clay`.

InsertMesh: `kitbash {target, part: {object, op: "carve"}, at}` puts any
object's shape into a clay at a surface point (its +Y along the normal,
half sunk), so a carved cylinder makes a hole; `kitbash remove {group}`
takes it out again.

`export_print {path}` writes `.stl`, `.obj`, `.ply` or `.3mf` for a
slicer: one welded mesh of the targets (default every visible solid) in
millimeters, Z up, on the plate. `size_mm: {axis, value}` scales it to a
size, `hollow: {wall_mm}` hollows clay inward, `fine` uses fine meshes
(with `detail_regions`, fine only where they are, so a whole figure can
print with 0.25 mm pores on the face and hands at a few percent of a
uniform 0.25 mm mesh). `max_triangles` (default 20,000,000, at most
50,000,000) refuses the export before anything is built when the estimate
(made on the clay as shown, every sculpt layer) is larger, and refuses it
again, writing nothing, when the mesh built (after `simplify`) is larger:
the estimate can run low, so leave headroom. `simplify: {error_mm}`
(0.001–1) simplifies the welded mesh to within that many millimeters and
checks it again, writing the unsimplified mesh with a warning if it would
not stay watertight. Where two sheets of a
fine mesh touch at a single point (a pore's rim, two parts meeting), the
copies of the shared corners are moved apart by max(0.005 mm, 0.02 x the
edges there), above slicers' weld tolerance, so the file stays manifold.
The reply says whether it is `watertight` (every edge used by exactly two
triangles) and gives `non_manifold_edges`, `non_manifold_by_kind {open,
over_used, pinched}`, `pinches_separated` (copies moved) and `pinch_offset_mm`,
`estimated_triangles`, `simplified_from`, `warnings`, `volume_cm3` and
`bbox_mm`. File > Export for Print… sends it.

#### 3D printing workflow

1. Build the model as clay, or bring it in with `import_mesh` (a scan or
   another app's model) and combine parts with `to_clay` (`op: carve` to
   cut holes or flats, `mode: "volume"` to fuse everything into one closed
   surface).
2. Export once and read the report. `watertight: true` and
   `non_manifold_edges: 0` mean a slicer gets a closed solid; otherwise
   remesh with `to_clay {mode: "volume"}` and export again. `bbox_mm` and
   `volume_cm3` show the size and material before you slice.
3. Size it: one scene unit is one meter, so give `size_mm {axis, value}`
   for a model built at another scale (`axis` is a scene axis: `y` for
   height).
4. Hollow large clay prints with `hollow: {wall_mm}` to save resin or
   filament; add drain holes yourself (a carved cylinder through
   `kitbash`).
5. Use `fine: true` on clays with a `detail_cell` (pores, scales,
   engraving), so the print gets the fine mesh rather than the view mesh.
6. Clide3D does not add supports or pick a print orientation beyond
   turning the model Z up and setting it on the plate; do that in the
   slicer. `.3mf` keeps millimeter units; `.stl` is binary.

```json
{"path":"/abs/out/bust.3mf", "targets":["Bust"], "size_mm":{"axis":"y", "value":120}, "hollow":{"wall_mm":2}, "fine":true}
```

### Stroke ledger, selective undo and history

Every `sculpt` stroke is a record on the clay's base layer, and every
`paint` color stroke a record on its paint layer: the stroke as sent and
as resolved (the object-space dabs it applies), with what it changed. The
`sculpt` reply lists them (`record_ids`, and per stroke `changed_points`,
`bricks`, `bbox`, `max_disp`, `mean_disp`; `measure: true` adds
`volume_delta`). `dry_run: true` runs the strokes on a copy and reports
the same numbers without changing anything; `label` names the records.

- `stroke_log {target, layer: base | paint}` lists the records (`full:
  true` adds the spec, to edit and resend, and the resolved dabs). A record
  is `stale_context` when the blobs or detail cell changed since it was
  made (replaying it now would give another result) and `engine_stale`
  when an older brush kernel made it.
- `stroke_edit` edits them and replays the layer from the first record
  changed (from the nearest checkpoint, every 32 records): `disable`,
  `enable`, `delete`, `replace {id, spec}`, `move {ids, to_seq}`, `scale
  {ids, factor}`, `rebase` (take the current blobs and kernel), `replay`
  (check), `squash`. `{action: scale, last: true, factor}` is Adjust Last:
  the newest stroke rescaled from 0 (gone) to 2 (twice as strong); it is
  stale after an undo or any other change, then scale by id.
- `sculpt_diff {target, from, to}` measures the base layer between two
  seqs (or `{id}`, `"blobs"`, `"current"`): max, mean and rms change,
  volume and area changed, a histogram. A seq or `{id}` is replayed as a
  background job.
- `probe_clay {target, points | rays | screen}` reads the surface: the
  nearest surface point, signed distance, normal, curvature, how far the
  sculpt moved it there (`layer_offsets.base`, mm) and the blob that owns it.

Grid changes made outside strokes (a new `detail_cell`, `clear_detail`, a
paint `fill`, a scene from before the ledger) become opaque records, so a
replay always ends at what the clay shows. Saved files keep the records;
on open each layer replays to check its saved grid (`get_clay` `layers`
reports `replay_verified`), and sculpt and paint grids are stored deflated
(about 5x smaller).

`history` lists the undo steps, one per agent call ("sculpt standard x3
on Head [agent]") or UI edit, with `goto`, `mark` and `label`; undo keeps
the last 300 steps and 2 GB of grid data (dropped steps are counted).
QuickSave writes a backup every 5 minutes while there are unsaved
changes, keeping 10 in `~/Library/Application Support/Clide3D/backups`
(`history {action: quicksave}`, or File > QuickSave).

### Sculpt, Paint, Mask and Groups tools (mouse and pen strokes)

The toolbox's brush tools turn a drag on a clay object into the same
`sculpt`, `paint`, `mask` or `group` request an agent sends. While the
button is down a worker previews the stroke so far on a copy of the clay;
nothing in the scene changes until release. Then the samples become one
stroke with a `screen` frame (the pixels and the view's camera matrices,
so the request means the same thing after you orbit), and the request runs
through the same tool handler as an MCP call, in the same line. Each drag
is one record with `source: ui` and one undo step ("sculpt inflate on Ball
[ui]"). The preview is exact for plain strokes; symmetric, tapered and
Backtracked strokes preview approximately (a tapered stroke can look
shallower in the middle) until the record replaces the preview. Alpha
strokes, lassos and posed clays commit on release without a preview.

Modifiers on a Sculpt drag: Shift smooths, Alt inverts, Ctrl paints the
mask, Ctrl+Alt erases it and Ctrl+Shift draws a mask lasso. With a brush
tool on, the right button orbits (Alt no longer does). Grab and Snakehook
drag across the view plane at the depth of the press. On a posed clay a
Sculpt drag sends posable symmetric strokes, which land on the rest pose;
Paint, Mask, Groups, Grab and Snakehook are still refused there.

The brush HUD sits over the right side of the viewport while a brush tool
is on. Every control is a front end for a tool call:

- size, strength, focal shift, the falloff curve, the alpha, X / Y / Z
  symmetry, LazyMouse and pressure: `brush_preset set_current`;
- the layer list (select, enable, record, add, and an amount slider that
  previews while it drags and sends one `sculpt_layer amount` on release);
- mask and group controls (`set_display overlay`, `mask` actions,
  `group show | hide | isolate | show_all`);
- the quality shade (`set_display shade`: beauty, curvature, lumpiness,
  normals or wire, as Shading > Quality sends it);
- Adjust Last (`stroke_edit {action: scale, last: true}`), a history
  scrubber (`history goto` and `mark`) and Save brush (`brush_preset save`).

Hotkeys with a brush tool on and the mouse over the view: hold S, U or O
and move the mouse sideways to change size, strength or focal shift; X
toggles X symmetry; D and Shift+D go to the next finer and coarser detail
level (`detail_level`, see below), but not while a stroke is drawn. X, D
and Shift+D are the Hotkey Editor's SymmetryX, LevelFiner and LevelCoarser
actions, so they can be rebound; with a brush tool on they win over Snap
to Grid's X and Edit Pivot's D.

Pressure comes from a touch's force (pens that report touches), from the
HUD's Tablet setting on macOS (an opt-in pen-pressure monitor, saved with
the editor's settings), or, with Velocity on, from the pointer's speed.
The request carries the pressure in its samples and names the source in
`pressure_source`. LazyMouse (string radius, LazySnap, Backtrack) smooths
the samples before they are sent, so a replay smooths nothing again.

The brushes are standard, clay, clay_buildup, clay_tubes, trim_dynamic,
polish, clay_polish, add, carve, inflate, deflate, smooth, flatten, crease, pinch,
layer, grab, snakehook, vdm and (for the Mask tool) mask_paint; the
Sculpting shelf has the preset list, brush, radius, strength, invert, X
symmetry and the Paint tool's color. Agents read and set the same brush
with `brush_preset {action: get_current}` and `{action: set_current,
tool, brush, preset, radius, strength, invert, color, symmetry, falloff,
alpha, pressure, lazy, velocity, tablet, group}`; it is editor state, not
saved and not an undo step. `set_display {brush_cursor: false}` hides the
depth-tested brush ring.

Agents see what the human did: `get_scene` and `job_status` list
`ui_activity`, the latest strokes and HUD calls with their job, status,
undo `step` and `record_ids`. `get_scene {detail: true}` adds each one's
`request`, which an agent can send to that tool again verbatim (it gives
the same record and reply).

### Live sculpt sessions, matcaps and Sculptris Pro

A clay under sculpt (a stroke in the last minute, or the Sculpt or Paint
tool on it while it is selected) is drawn in chunks of 32 x 32 x 32 cells
at its detail cell (`edit_clay detail_cell`; else the cell of its first
stroke): a stroke remeshes only the chunks it touched, and the GPU takes
only those, so a dab shows in a few milliseconds even on a clay of
millions of triangles. Paint, the sculpt mask and polygroups change only
chunk colors and overlay attributes. Everything else (snapping,
`surface_point`, bakes, film renders, exports) still reads the clay's
regular mesh, which builds again once strokes pause; a clay posed by its
rig, or shown baked or fine, draws as before. Agents see the same: each
`sculpt` record reports `dirty_chunks`, and `get_clay` reports
`session_cache` (the blob field cached for the session: `bricks`, `mb`,
`chunked`, `last_remesh_ms`). Strokes read that cache too, so agent
sculpt calls are faster on big clays as well.

MatCaps: `set_display {shading: "matcap"}` shades every fill with a lit
sphere looked up by the normal as the camera sees it (creases on sculpted
clay a little darker), the way sculpting apps show forms; `{matcap:
"clay" | "red_wax" | "skin" | "metal" | "/path/to/sphere.png"}` picks one
(a square image at most 1024 a side) and turns matcap shading on;
`{shading: "shaded"}` turns it off. The Display > MatCap menu sends the
same calls. `ao` (true, false or ambient_occlusion's object) and
`ao_radius` are short for `ambient_occlusion`'s settings.

Sculptris Pro: give a stroke a `cell` finer than the clay's detail cell
(or give `cell` to the whole `sculpt` call for strokes without one), and
before the stroke a detail region of that cell grows over it (its dabs
grown by their radius): a region of the same cell that already holds it
stays as it is, one it overlaps grows to hold both, and past 16 regions
the smallest of the nearest cell merges into its neighbour. The stroke
then writes the finer grid there; the reply's `detail_regions` counts
them, and `edit_clay detail_regions` or `get_clay` show them. Each
growth is one small `regions` record in `stroke_log` (the regions, not the
grid), so it replays exactly and a long drag costs the saved file next to
nothing: a clay without layers or morphs is held only to its per-grid caps
(about 270 MB in a file), well inside the 1 GiB saved budget. With morphs
(each written out again) a drag can still reach it, and the stroke that
would pass it is refused and changes nothing.

```json
{"target": "Head", "strokes": [{"brush": "crease", "points": [[0.02, 1.71, 0.11], [0.05, 1.7, 0.1]], "radius": 0.004, "strength": 0.001, "cell": 0.0005}]}
```

### Detail levels and detail transport

Detail levels work like subdivision levels. Level 0 is the clay's own
detail cell; `detail_level {target, add: cell}` adds a finer cell as a
level (up to 8, coarse first) and makes it active. `level` (an index) or
`cell` picks the active level, and a call with neither lists them:

```json
{"target":"Head", "add":0.0005}
{"target":"Head", "level":0}
{"target":"Head", "strokes":[{"brush":"grab", "point":[0,1.6,0.1], "radius":0.05, "delta":[0,0,0.02], "level":0}]}
```

`sculpt` strokes without `level` work at the active level (`level` can
also be given per call or per stroke). A stroke at level k writes level k
and the coarser ones; above 0 it first grows a Sculptris Pro region of
that level's cell over itself. Finer detail on the layer it writes is
carried along (transport): grab, stretch, snakehook and transform move it
with the material through their backward map, and other brushes move it
with the surface along the normal, so pores made at a fine level survive a
coarse grab or inflate. Other sculpt layers keep their detail where it was,
so their records show `stale_context` and `stroke_edit rebase` replays
them. Records keep `level` and `cell`.

Adding a level is an undo step. Picking the active level is like a
selection: it is saved with the clay but is never an undo step, and undo,
redo and `history goto` keep it. The reply lists `levels` (`level`, `cell`,
`regions`, `active`), and `get_clay` reports them too. In the editor, D
and Shift+D step the selected clay's level while a brush tool is on.

Transport changed what several brushes produce, so the brush kernel is
version 2: every record made by the version-1 kernel (before detail
levels) is reported `engine_stale` in `stroke_log`, and `stroke_edit
{action: rebase}` replays it with the current kernel.

### Polygon objects: subdivision levels and creases

A Poly object is a polygon cage (mostly quads; triangles and n-gons are
fine) with Catmull-Clark subdivision levels and semi-sharp creases. Clay
stays the place to sculpt; a Poly is made from a clay and is what later
stages retopologize, project detail onto and export with maps. One tool,
`poly`, does everything with an `action`:

```json
{"action":"from_clay", "source":"Head"}
{"action":"subdivide", "target":"Head_poly", "levels":3}
{"action":"set_level", "target":"Head_poly", "level":1}
{"action":"crease", "target":"Head_poly", "angle":40, "sharpness":2}
{"action":"crease", "target":"Head_poly", "edges":[[12, 13], [13, 40]], "sharpness":10}
{"action":"info", "target":"Head_poly"}
```

- `from_clay` (a background job) makes a new Poly in the clay's place from
  its rest surface-nets mesh: the triangle pairs the mesher wrote for each
  quad become quads again, points are welded, pinched sheets get their own
  corners and points where parts touch are split, so every edge is in two
  faces. It is dense and unclean (about half its points are not
  four-way): a quick start, not a retopology. The reply has the `info`
  below plus `hausdorff_cells` and `mean_distance_cells`, the largest and
  mean distance from the cage's points to the clay (in its mesh cells),
  measured at `distance_samples` of them: at most 20,000 evenly picked, so
  the largest is a sampled one-sided Hausdorff distance, not the exact
  one. The clay is hidden unless
  `hide_source: false`. A clay whose mesh fell back to marching
  tetrahedra (ambiguous cells) gives a cage of triangle pairs.
- `subdivide {levels}` keeps 0-6 levels. Every level makes one quad per
  face corner, so faces grow four-fold; a call whose finest level would pass
  16,000,000 faces is refused from the counts, before anything is built,
  and says how many levels fit. It shows the finest kept level of at most
  4,000,000 faces.
- `set_level {level}` picks the level the viewport draws and renders and
  exports take (0 is the cage). Its drawn mesh (up to 4,000,000 faces)
  builds in the background: the old level shows until it is in, and later
  calls wait in line for it (read-only ones go ahead). Page Up and Page Down step it for the
  selected Poly; Modify > Poly from Clay and Modify > Subdivide Poly send
  `from_clay` and `subdivide`.
- `crease` (a short background job) sets semi-sharp creases on cage edges, by point pairs (`edges`,
  up to 65,536 a call, each point 0 to `info`'s `cage_points` less one) or
  by `angle` (every edge whose faces meet more sharply; refused when that
  picks more than 65,536 edges, and with sharpness 0 it picks only edges
  creased now). A crease of sharpness s uses the sharp rules for s levels and
  then the smooth ones (fractions blend; DeRose, Kass and Truong 1998), so 2
  is a tight bevel, 0.5 a soft one, 10 stays sharp at every level and 0
  removes the crease. Open borders always stay sharp, and a border corner
  stays put.
- `info` (a short background job: it builds the cage's adjacency off the
  window's thread): `points` (those the faces use) and `cage_points` (every
  index), faces, edges, quads_pct, the valence histogram, irregular_pct
  (interior points without four edges), boundary loops, open,
  non-manifold and flipped edges, `non_manifold_points` (where parts meet
  only at a point), components, Euler characteristic and genus (a
  non-manifold point counts once per fan of faces around it, as if split
  the way `from_clay` splits them), the levels and each level's faces,
  creases, `made_by` and a content hash.
- `rebuild` runs the call that made the cage (`from_clay` or `retopo`,
  kept in `made_by`) again on the clay it came from (levels kept while
  they fit; creases kept if the faces are the same).
  `to_clay` (with `resolution`, `mode`, `blur` and `polish`) turns the
  shown level into clay in place, as `to_clay` does.

A Poly is drawn, picked, rendered and exported (`export_glb`,
`export_fbx`, `export_print`) at its shown level as triangles, with its
quads' sides as the wireframe. It saves compactly (the cage, its creases
and the level count; levels are rebuilt on load) and every action is an
undo step. `quality_report` on a Poly reports its topology, `quads_pct`,
`irregular_pct` and `hausdorff_cells` (the shown level's points, edge
middles and face middles against its source clay, at most 20,000 evenly
picked, as `retopo` measures; threshold 1 cell), with silhouettes and
close-ups as for clay. A linked mirror copy of a Poly follows its levels
and creases. Damage stamps are not drawn on a Poly (the `damage` tool
refuses one, as it does an imported mesh).

### Retopology: a clean quad cage

`retopo` (a background job) turns a clay into a clean, mostly four-way
quad cage: a new Poly object in the clay's place, ready for `poly
subdivide`, creases and (later stages) projection and export. It is
in-house field-aligned retopology (Instant Meshes, Jakob et al. 2015):
an orientation field that follows the form and a lattice position field,
both solved coarse to fine on the clay's remeshed surface; quads are
taken where the lattices meet, odd faces are paired away, a final split
makes every face a quad (`pure_quads`, default on), and the points are
fitted onto the clay. The same call gives the same cage on any machine
thread count.

```json
{"target":"Head", "faces":8000}
{"target":"Head", "faces":4000, "guides":[[[0.09,1.84,0.33], [0.0,1.86,0.34], [-0.09,1.84,0.33]]], "density":[{"center":[0,1.9,0.3], "radius":0.1, "scale":0.5}]}
{"target":"Bolt", "faces":2000, "keep_creases":30, "symmetry":"none"}
```

- `faces` (500-200,000, default 8000) is the quad count after the final
  split; the cleaned cage before it has about a quarter of that.
- The edge flow follows the clay's principal curvature, and lines it must
  follow become edge loops: creases (edges whose faces meet at more than
  `keep_creases` degrees, default 40: a box's edges and a cylinder's rims;
  their corners stay), `guides` (world polylines at rest, up to 64 of 2-256
  points; lay them on the surface, from `surface_point`), polygroup
  borders (`keep_group_borders`, default on) and the symmetry plane.
  Where two lines cross, the crossing is a fixed corner.
- `symmetry`: `x` mirrors the clay's x >= 0 half (in its own space), so the
  cage is exactly symmetric; `none` does not; the default does when the
  clay and its polygroups are X-symmetric within a cell. When the edge flow
  finds no clean loop on the plane the cage is made whole and a note says
  so.
- `adaptive` (0-1, default 0.3) makes curved parts denser; `density`
  regions (up to 16 `{center, radius, scale}`, scale 0.25-4) scale the
  edge length near a point. `source: fine` fits the fine detail surface
  (for parts under two cells thick).
- The reply has the object, `faces`, `quads_pct`, `irregular_pct`
  (interior points without four edges), `before_split` (the cleaned cage's
  faces, quads, triangles, pentagons and valences), `edge_cv` (edge length
  deviation over mean), `hausdorff_cells` and `mean_distance_cells` (the
  cage's points, edge middles and face middles to the clay, in its cells),
  `coverage_cells` (the clay's surface to the cage), `symmetric`,
  `symmetric_error`, the cage's topology, timings and a wireframe image of
  the front and right views (`images: false` leaves it out).
- Sizes are checked before any work: faces, guides, density regions and
  the input the faces need (8 points per face, at most 2,000,000).
- Loose crumbs (separate bits of the clay's mesh smaller than four output
  faces, as a strong crease stroke can leave) are dropped before
  remeshing, and a note says how many; the largest part always stays.
- Modify > Retopologize… sends the same call; `poly rebuild` runs it
  again (guides and density are kept in the clay's own space), and
  `quality_report` on the result measures it against the clay.

### Projecting detail onto levels, UVs and maps

A retopologized cage is smooth; the clay's sculpted detail goes onto the
Poly's subdivision levels with `poly project`, as tangent-space vector
displacement per level, and from there into maps with `bake`.

```json
{"action":"uv", "target":"Head_retopo"}
{"action":"project", "target":"Head_retopo", "levels":4}
{"target":"Head", "lowpoly":{"poly":"Head_retopo", "level":2}, "maps":["vector_displacement","displacement"], "map_size":4096, "map_format":"exr", "save_dir":"/abs/out/maps", "verify":true}
```

- `poly project` (a background job) works coarse to fine: each level's
  points are subdivided from the level above as projected, moved onto the
  clay, and the move kept in the point's own frame. Each level holds only
  its own band of detail, the finest lies on the clay, and the detail
  turns with the cage when the cage is edited. `source` defaults to the
  clay the Poly was made from; the clay's unchecked strokes settle first
  (`settled`).
- `levels` (1-6, default the levels kept) subdivides as needed (refused
  above 16M faces). `mode: "vector"` (default) moves each point to the
  surface along its normal, or to the nearest surface point within `cone`
  degrees of the normal (default 45) when that is under half as far or
  the normal finds nothing; `"scalar"` uses the normal only. `reach`
  (default 3 mean cage edges) is the farthest a point moves: detail
  farther out than that is missed and counted (`per_level[].missed`,
  `notes`); send a larger reach for big overhangs.
- The clay holds no detail finer than its sculpt cell, and its surface
  turns in small steps at the cell's faces. Where a level's edges are
  shorter than the cell, the level's moves are smoothed over about half a
  cell (`per_level[].smoothing_passes`), so the fine levels do not copy
  those steps into renders; levels with edges of a cell or more are not
  touched.
- The reply has, per level, the RMS and largest move in cells and how
  each point was placed; for the finest level `hausdorff_cells`,
  `mean_distance_cells`, `rms_cells`, `normal_rms_deg` and
  `coverage_cells` (the clay to the level). It shows the finest projected
  level. `info` lists `displaced_levels` and `projected`. A 5,000-quad
  cage over four levels (1.3M points) takes about a second.
- `poly uv` (a background job) unwraps the cage into one UV tile (`size`
  512-8192, default 2048; `padding_px` 0-32, default 4) with every face
  whole in one chart; levels take face-varying linear UVs, so seams stay
  seams. The reply has charts, seams, `utilization`, `area_spread` and a
  picture of the layout. `udim: true` spreads the charts over UDIM tiles
  (see UDIM texture tiles below).
- `bake` with `lowpoly: {poly, level}` uses that level of the Poly,
  subdivided with no displacement (what another program's Catmull-Clark
  makes of the cage), as the game mesh: two triangles a quad, the Poly's
  UVs (or an unwrap made for the bake). Each texel of the maps finds the
  Poly's projected surface at the same place and snaps it onto the clay,
  so vector maps reapplied to that level reproduce the projection,
  overhangs included, where a height map cannot. The bake is made again
  when the Poly changes.
- Modify > Unwrap Poly UVs and Modify > Project Clay Detail… send the
  same calls. A linked mirror copy keeps the levels' detail and the UVs.

### Exporting the subdivision cage and maps

`export_poly` (a background job) writes what a studio pipeline takes from
a Poly: geometry another renderer subdivides, plus maps that put the
sculpt back on. File > Export Subdivision Cage… sends the same call for
the lead Poly.

```json
{"target":"Head_retopo", "path":"/abs/out/head.obj"}
{"target":"Head_retopo", "path":"/abs/out/head.obj", "maps":["vector_displacement","displacement","normal","color","ao"], "map_size":4096}
{"target":"Head_retopo", "path":"/abs/out/head.glb", "maps":["normal","color","ao"]}
{"target":"Head_retopo", "path":"/abs/out/head.usda", "maps":["displacement","normal"], "space":"tangent"}
```

- The geometry is `level` (default 0, the cage; a `.glb` defaults to the
  shown level) with no displacement. `.obj` keeps the polygons (quads,
  n-gons), a `vt` per face corner (v up) and the creases as OpenSubdiv's
  `t crease 2/1/0 i j sharpness` tags (0-based points). `.glb` holds the
  level as triangles with the baked textures, and the cage itself in the
  mesh's `extras.clide3d_cage`. `.usda` is a Catmull-Clark `Mesh` with
  `creaseIndices`/`creaseLengths`/`creaseSharpnesses`, a faceVarying
  `primvars:st` and a `UsdPreviewSurface` reading the maps. A finer level
  carries the cage's creases less one per level, so subdividing it
  further gives what subdividing the cage gives.
- The conventions are Catmull-Clark with edge-and-corner borders,
  semi-sharp creases (sharpness s stays sharp for s levels; 10 is
  infinite) and linear face-varying UVs; `<stem>.json` beside the file
  (the sidecar) states them with each map's file, format, scale and
  decoding.
- Maps (default `vector_displacement` and `normal`; `[]` for none) sit on
  `map_level` (default the finest projected level; a `.glb`'s on the
  level it holds): the written level subdivided `map_level - level` more
  times with no displacement. Vector maps are 32-bit EXR and height maps
  16-bit PNG by default (`map_format` sets both); normal, color and AO are
  8-bit PNG. `space: "tangent"` (default) has R along +u, G up the image
  (the bitangent), B along the normal; `"object"` is the Poly's x, y, z
  and needs no frame to match. Values are in the Poly's units. Maps need
  `poly uv` first and the clay (`source`, default the one the Poly was
  projected or made from; its unchecked strokes settle first), and the
  Poly placed like the clay up to a move, a turn and an even scale.
  `map_size` is 512-4096.
- `verify` (default on) reads the files back alone: the cage against the
  one written, then the cage subdivided to `map_level` with the map from
  its file added (diced to half a cell), measured against the clay
  (`max_error_cells`) and the Poly's shown level (`to_shown_level`).
- Nothing is overwritten but this writer's own kind of file (or an empty
  one): a folder, a link, an unreadable file or another format at any of
  the paths is refused before any work.
- `import_mesh` with `as: "poly"` reads an `.obj` (or the `.glb` and
  `.usda` this writes) back as a Poly with the same cage, creases and
  UVs.
- `udim: true` (`.obj` or `.usda`) also writes the Poly's texture tiles
  and per-tile maps (next section).

### UDIM texture tiles: painting, baking and exporting

A Poly can carry texture maps on UDIM tiles 1001-1100 (ten a row: tile
1001 + u + 10 v by the UVs' whole parts), painted with the same strokes
as clay paint and exported as standard UDIM files.

```json
{"tool":"poly", "target":"Head_retopo", "action":"uv", "udim":true, "tiles":4, "size":4096}
{"tool":"bake", "target":"Head", "lowpoly":{"poly":"Head_retopo"}, "udim":true, "texture":4096}
{"tool":"paint", "target":"Head_retopo", "resolution":4096, "strokes":[{"points":[[0,1.6,0.1],[0.05,1.62,0.1]], "radius":0.02, "color":[0.8,0.3,0.3], "snap":true}]}
{"tool":"paint", "target":"Head_retopo", "channel":"roughness", "value":0.3, "strokes":[{"point":[0,1.7,0.1], "radius":0.05, "snap":true}]}
{"tool":"export_poly", "target":"Head_retopo", "path":"/abs/out/head.obj", "udim":true}
```

- `poly uv` with `udim: true` packs the charts at `texel_density`
  texels per unit of surface on tiles of `size` texels (or, without it,
  at the density that fills about `tiles` tiles, 1-100, default 4); each
  chart stays on one tile. The reply's `udim` lists the tiles, the
  density and each chart's (`chart_density`), and how many charts cross
  a tile (0). A chart larger than a tile at the density is refused.
- Channels: `color` (sRGB), `roughness` and `mask` are painted;
  `normal` and `ao` come from a bake. Each channel has one resolution
  for all its tiles (256-8192 texels a side; `bake udim` bakes at
  512-8192), kept as 256 x 256 pages of
  16-bit RGBA made where something is written; a Poly holds at most
  4 GiB of them, and a call that would pass that is refused before it
  writes.
- `paint` on a Poly resolves its strokes on the shown level (frames,
  snapping, pressure, symmetry, falloff as clay paint) and paints each
  texel whose point on that level is under the stroke and faces the way
  the surface under it does. Both sides of a UV seam (and of a tile
  border) see the same stroke, and texels just outside each face are
  painted too, so seams do not show. Each stroke is a record: undo,
  save and load keep it, and a new `poly uv` paints the strokes again in
  the new layout. Alphas, masks, layers and fills are clay paint's.
- Two colour sources: paint on the clay before the retopology, `bake
  udim: true` to the tiles (it replaces colour, roughness, normal and AO,
  and drops strokes on colour and roughness, saying how many), then paint
  on the Poly. A later `poly project` does not touch the tiles.
- The colour tiles show in the viewport and in `render_film` (as the
  colour of the diced surface, so about a pixel fine there; both
  tracers). Roughness, mask, normal and AO tiles are exported, not
  rendered.
- `export_poly` with `udim: true` writes `<stem>.<channel>.<UDIM>.png`
  for each tile with texels (16-bit RGBA, straight alpha: A is the
  coverage over the material) and the displacement maps per tile as
  `<stem>.<map>.<UDIM>.<ext>`; the sidecar lists every file. In the
  `.obj` and `.usda` each tile's v is flipped in place, so tile 1011
  stays above 1001. `import_mesh as: "poly"` reads the tiles the sidecar
  lists back texel for texel.
- The Paint tool works on a Poly in the editor (other brushes are for
  clay) and sends the same `paint` call.

### Viewport graphics: shadows, ambient occlusion, bloom and sky

The viewport (and `render_views` and `render_animation`, which draw
through it) is lit like `render_film`, so a film look can be previewed
live. Everything is on by default and set with `set_display`:

- `lighting`: `"studio"` (default: render_film's studio environment and
  key, fill and rim lights), an environment preset (`overcast`, `sunset`,
  `golden_hour`, `forest`, `night`, `storm`, or the physical sky:
  `physical`, `sky_noon`, `sky_afternoon`, `sky_golden`, `sky_sunset`), a
  look preset (`trailer`, `desert_heat`, `night_ops` ...), `"film"` (the
  last `render_film`'s lighting), `"classic"` (the old fixed studio), or
  `{environment, lights, extra_lights, look}` as `render_film` takes them.
  The environment lights the scene diffusely and in reflections
  (image-based lighting); its sun and the studio lights become
  directional lights.
- `shadows`: key-light shadow maps. `true` / `false` or `{enabled, soft
  (contact-hardening PCSS), quality: normal | high (2048 or 4096 map),
  strength (0..1), softness (the key's angular radius, 0.005..0.3),
  ground (catch shadows on y = 0 where there is no floor), key_direction}`.
  Other lighting shadows its brightest sun.
- `ambient_occlusion` (or `ao`): screen-space contact darkening, `true` /
  `false` or `{enabled, radius (0.01..10 world units, default 0.5),
  strength (0..2, default 1)}`.
- `post`: HDR finishing, `true` / `false` or `{exposure, white_balance,
  bloom, vignette, tonemap: aces | agx | filmic | none, grade, look,
  reset}`. The default is ACES with bloom `{strength 0.08, threshold 1}`,
  so only light above the threshold (emitters, torches, lit windows)
  glows.

```json
{"lighting":{"environment":{"preset":"physical", "sun_elevation":12}, "lights":"none"}, "shadows":{"soft":true, "quality":"high"}, "post":{"exposure":0.5, "bloom":{"strength":0.15}}}
```

Scenes over the triangle budget draw without shadows or ambient occlusion;
the reply's `shadows.last_frame` and `ambient_occlusion.last_frame` say
whether the last frame had them and why not. Masks never change. The
Lighting menu offers the same settings. Repeated geometry (`scatter`,
`kitbash` parts, `instance_array`) is drawn instanced. `render_film`
always traces its own shadows and light. `graphics_check/README.md` has
before and after pictures for each feature.

### Film renders

- `render_film` renders with Clide3D's own path tracer (like Blender's
  Cycles): soft studio lights with real shadows, light bouncing between
  surfaces, GGX glossy and metal materials, clear coat and sheen, baked
  textures and normal maps, see-through hair cards, a shadow-catching floor,
  an edge-aware denoiser, and a filmic tone curve. It uses every CPU core
  (a 640x640 face close-up at 256 samples takes about 20 seconds on 12
  cores). Framed like `render_views`; `save_path` writes the PNG. Views
  share one ray tree and take turns adding samples, so each view gets an
  equal share of `time_limit`. Any view (here, in `render_views` and
  `check_export`) may frame its own point: `{view, center, radius}`.
- Poly objects (see Polygon objects) render as their Catmull-Clark
  surface with their projected displacement, every stored level included
  (not only the shown one), diced so edges are about `dice_rate` pixels
  (default 1) where the cameras see them. Each cage face takes the level
  its size on screen asks for, up to `subdiv_max_level` (default 6, and
  at most 8M evaluated faces); faces next to coarser ones snap their
  points along the shared edge to the coarser side's, so the surface stays
  watertight. `displacement: false` renders the smooth surface;
  `subdiv_max_level: 0` renders the shown level as before. `max_diced`
  (default 24,000,000 triangles, the sum over all Polys of the render) is
  kept by dicing every face of every Poly a level coarser (the reply's
  note says so). When level 1 everywhere still does not fit, the Poly
  with the most triangles renders at its shown level instead, as before
  subdivision existed (its `note` says so and names `max_diced`), and so
  on until the rest fit; a Poly whose level 1 passes the evaluation bound
  renders at its shown level too. The render itself is never refused for
  this. The frame's width and height both decide what is on screen, and a
  large face partly on screen is diced for the part the camera sees. The reply's `subdivision` lists each Poly's levels,
  `evaluated_level`, `triangles` and `displaced`. Both tracers trace the
  same triangles. A shown baked mesh (`baked: true`) renders as baked.
- GPU: `device` (`auto` default, `gpu`, `cpu`). Auto traces on the GPU
  (`src/film/gpu.rs` + `trace.wgsl`, the same light paths as the CPU
  tracer, checked by `gpu_matches_cpu`) when the scene has no hair strands
  and fits its buffers, and on every CPU core otherwise. On an M2 Max it is
  about 1.5-2x faster for clay, props and creatures; hair strands are still
  faster on the CPU (wgpu cannot use Metal ray tracing yet, and strand trees
  are millions of thin ribbons). `gpu` forces it for strands too. Work goes
  up in small tiles, so a long render never stalls the GPU and cancel stays
  prompt. Time it on a scene with
  `SCENE=path OUT=dir cargo test --release gpu_timing -- --ignored --nocapture`.
- Weathering: materials take `cavity` (darker creases and gaps between
  scales), `edge_wear` (rubbed lighter edges), `dirt` (grime in low places,
  `dirt_color`), `variation` (color blotches) and `gloss_variation`
  (patchy shine). They are measured from the shape, so one `set_material`
  call weathers a whole creature; clay meshes show it and bakes keep it.
- Lighting: `environment` defaults to `"studio"`; file-free skies are
  `"overcast"`, `"sunset"`, `"forest"`, `"golden_hour"`, `"night"` and
  `"storm"`. `"physical"` is a physical sun and sky (Rayleigh, Mie and
  ozone scattering): the sun and sky redden and dim as the sun goes down.
  `{"preset":"physical", "sun_elevation":10, "sun_azimuth":120,
  "turbidity":3, "ground_albedo":0.3}` sets it (elevation -5..90 degrees,
  below 0 is twilight with no sun; turbidity 1 clear to 10 hazy);
  `"sky_noon"`, `"sky_afternoon"`, `"sky_golden"` and `"sky_sunset"` are it
  at 60, 30, 8 and 1.5 degrees. Fog does not follow the sky: set its color
  to match.
  They supply sky and ground light; sunny presets include a small finite
  sun disc for soft-edged shadows. Forest approximates canopy gaps.
  Use `lights: "none"` for natural light, or retain `"studio"` (default)
  for the original three soft lights. HDR lighting takes
  `environment: {"hdr":"/absolute/sky.hdr", "rotate":30, "intensity":1}`:
  linear equirectangular Radiance RGBE, including raw and RLE files, up to
  16 million pixels. Rotation is degrees around +Y. The renderer samples
  luminance with exact texel solid angles and combines it with BSDF
  sampling using MIS; procedural suns get their own disc sampler.
- `background: "environment"` (or `true`) shows that lighting behind the
  subject; `"studio"` (or `false`, default) keeps the plain backdrop.
  `floor: true` still catches shadows and receives the chosen lighting.
  A `lights` array replaces the studio lights; `extra_lights` appends to
  any setup. Colors are linear RGB, default `[1,1,1]`, intensity defaults
  to 1. Light objects:

  | Type | Fields and units |
  | --- | --- |
  | `sun` | `direction` toward the light; `size` angular diameter in degrees (default 0.53, range 0.01–90); `intensity` integrated radiance, approximately face-on irradiance |
  | `area` | `position` in world meters; `direction` emitting normal (default `[0,-1,0]`); `size` disc diameter in meters (default 1); `intensity` radiance; one-sided, inverse-square falloff |
  | `point` | `position` in world meters; `intensity` radiant intensity with inverse-square falloff |
  | `sphere` | a glowing ball: `position`, `radius` in meters (soft shadows), `intensity` as a point light's |

  For example, `extra_lights: [{"type":"area", "position":[-2,3,-1],
  "direction":[1,-1,1], "color":[0.6,0.75,1], "intensity":8, "size":1}]`
  adds a cool rim. Analytic lights illuminate surfaces but are not visible
  geometry in the picture.
- Camera: `camera: {"focal_length_mm":85, "dof":{"f_stop":2.8,
  "focus_point":[0,1.7,0.08]}}` gives a portrait lens focused on the eyes.
  `fov` specifies vertical degrees and overrides `focal_length_mm`; focal
  length assumes a full-frame 24 mm sensor height. Changing either dollies
  the camera to preserve framing; `position` and `target` can override its
  world pose. `aperture` is an f-stop, equivalent to `dof.f_stop` (which
  takes precedence). Omit both for the default pinhole. Focus defaults to
  the framed center, or use `dof.focus_distance` in meters along the camera
  axis instead of `focus_point`. A circular thin lens gives smooth bokeh;
  lower f-stops give more blur. Use a perspective view (`ortho: false`)
  for DOF and more samples for deep blur. Denoiser guides average the lens
  samples, including their blurred surface colors.
- Finishing: `exposure` is a brightness multiplier (default 1);
  `white_balance` is a temperature to neutralize in Kelvin (2000–12000,
  default 6500), or three positive linear RGB gains. `tonemap` is `"aces"`
  (default), `"agx"` for smooth highlight gamut compression, or `"filmic"`
  for a shared shoulder that preserves saturated glowing colors.
  `bloom: true` adds a subtle glow, a number sets its strength (0–2), or
  use `{"strength":0.08,"threshold":1}`. Threshold is scene-linear after
  exposure and white balance; bloom runs before tone mapping.
  `vignette: 0.2` gently darkens corners (0–1, default off).

  A useful portrait starting point is `golden_hour`, `lights: "none"`,
  an 85 mm camera at f/2.8 focused on the eyes, `tonemap: "filmic"`, and
  subtle bloom. All lighting and lens settings apply to every requested
  view; each view still has its own center/radius framing.

- Glow and glass: `emission` makes a surface give off light in its own
  color (the `glow` preset: glowing eyes, lava, runes; on an Eye object it
  lights the eye's own sclera and cornea), which also lights
  what is near it. `transmission` with `ior` makes it see-through, bending
  light like real glass (the `glass`, `water` and `crystal` presets; the
  color tints it by hue, roughness frosts it). The editor view shows glow
  and see-through glass too; exports use glTF emissive (+ emissive
  strength), KHR_materials_transmission and KHR_materials_ior, and FBX
  EmissiveColor / TransparencyFactor.
- Skin glows: materials with `subsurface` (the `skin` preset, or any
  material via `set_material subsurface / scatter`) let light enter, spread
  under the surface per color (Christensen–Burley diffusion; red goes
  furthest) and come back out nearby, softening shadow edges. Skin also has
  a tight and broad GGX reflection (`skin_specular`, default 0.8), 0.25 mm
  procedural pore relief with roughness variation (`pores`, default 0.65),
  and subtle pigment mottling
  (`mottling`, default 0.4). Both controls accept 0 to disable, apply only
  to subsurface materials, and use object-space meters so the pattern stays
  attached to the character. Bake writes the same relief into normals and
  roughness, and pigment into color; use enough texels to resolve pores.
  Film uses the baked detail once when rendering baked meshes.
  Thin parts let light through as well: a ray straight in finds the far
  side, and each color crosses that thickness (red furthest), so ears and
  nose wings glow red against a back light. Human faces get regional skin
  instead of the even pores (see Faces below).
- Render passes for compositing (like ZBrush BPR passes): `passes:
  ["depth", "normal", "albedo", "ao", "shadow", "mask", "emission"]` with
  an absolute `passes_dir` writes `<pass>.png` (or `<view>_<pass>` with
  several views) next to the picture. `passes_format: "png"` (default) is
  16-bit for depth, normal, ao, shadow and mask and 8-bit sRGB for albedo
  and emission; `"exr"` writes 32-bit floats (depth in world units, linear
  colors). `pass_samples` (default 8), `ao_distance` (default a quarter of
  the framing radius) and `normal_space: world | camera` tune them. The
  reply's `passes` gives each file and `passes_info` how to decode it
  (depth PNG: hits 1..65535, nearest 65535, background 0). Passes come
  from their own CPU first-hit trace of the same scene, after the picture
  and outside `time_limit` (well under a second at 640 px), and not with
  `motion_blur`.

```json
{"views":["three_quarter"], "size":640, "passes":["depth","normal","mask"], "passes_dir":"/abs/out/passes", "passes_format":"exr"}
```

### Adaptive detail: pores, scales and chips in one object

One clay can carry fine detail where it matters and coarser detail
elsewhere, so a whole character keeps pore-level skin on the face and
hands without splitting the head into its own object, and a dragon can
have fine scales over its whole body.

- `edit_clay detail_cell` is the sculpt cell everywhere (1 mm on a body,
  4 mm on a 5-unit dragon); `edit_clay detail_regions` adds finer cells
  inside ellipsoids: `[{name: "face", center, radii, cell: 0.00025},
  {name: "hand.L", center, radius, cell: 0.00025, mirror: true}]` (object
  space, like blobs; up to 16). The list replaces the current regions;
  a new region starts from the surface as it is, and removing a region
  drops only its finer detail (every stroke also lands on the coarse
  grid).
- `create_human detail: {cell: 0.001, face: 0.00025, hands: 0.00025}`
  makes the whole person one rigged clay with face and hand regions placed
  from the skeleton (the reply lists them).
- Every `sculpt` stroke lands on the coarse grid and on each region it
  reaches, at that region's cell (a region stores only what it adds to the
  coarser grids, so it costs nothing until something finer is sculpted in
  it). Inside a region its detail counts fully; outside, it fades back to
  the coarse grid over a few coarse cells, so there is never a step.
  `noise` finer than a grid can hold (pores on a 1 mm grid) skips that grid
  when the stroke reaches a region fine enough for it, so pores stop
  softly at the face's edge instead of turning into lumps. Paint and masks
  stay at `detail_cell`.
- The fine mesh (`render_film`, `render_views fine: true`, bakes) is then
  adaptive: each coarse cell near the surface is cut into 2, 4 ... 32
  cells per side down to the cell the sculpt or paint there needs (the
  regular mesh where nothing is sculpted). Quads come from the smallest
  grid edges around each place (dual contouring on an octree), so the mesh
  stays closed across every change of size; a cell gets one vertex per
  piece of surface inside it, and the rare pinch left where two sheets
  touch is split, so the mesh is also manifold. Thin gaps a coarse mesh would close (lips,
  between fingers) open wherever a region is fine. The fine mesh is
  limited to 24 million triangles (one that fails is reported in
  `fine_mesh_errors` and the regular mesh is drawn).
- How to pick detail per region: make the cell about a third of the
  smallest feature there, and keep regions tight (triangles grow with
  area / cell^2):

| Region | Feature | Cell |
| --- | --- | --- |
| Face (pores, crow's feet) | pores 0.8 mm | 0.00025 |
| Hands (knuckle creases) | creases 1.5 mm | 0.00025-0.0005 |
| Body skin, cloth folds | bumps, folds 5-50 mm | 0.001-0.002 (detail_cell) |
| Dragon face and limbs | scales 10-15 mm | 0.002-0.003 |
| Dragon body | scales 30-60 mm | 0.004-0.006 (detail_cell) |
| Wings, back of a statue | nothing sculpted | leave at detail_cell |

  A 0.25 mm face is about 1-2 million triangles. The `detail_human` check
  below (pores, forehead lines, crow's feet, knuckle creases, 1 mm body)
  meshes to 8.4 million triangles in about 30 seconds.
- Render-time displacement: a material's `detail_displace` (scene units)
  moves the surface by its detail map's height in `render_film`, as real
  geometry in both tracers: the mesh is diced to about a pixel where the
  cameras see it (never finer than the map's texels; off-screen and far
  surfaces stay coarse) and each vertex moves along its normal. Chipped
  stone walls, bark and scales then stand out of silhouettes and shadow
  themselves; the map's normal relief still shades the finer slopes. The
  reply reports `displaced_triangles`. Stone walls 0.005-0.02, bark 0.005.

Reproduce the pictures and timings in `detail_check/`:

```
cargo test --release detail_human -- --ignored --nocapture
SCENE=/path/dragon.json cargo test --release detail_dragon -- --ignored --nocapture
cargo test --release detail_wall -- --ignored --nocapture
BAKE_TIMES=1 cargo test --release bake_timing -- --ignored --nocapture
```

### Environments: dungeons, ground, rocks, props, light and mist

Clide3D builds game environments the way it builds characters: one call
per piece, real geometry, film-rendered.

- `kit` places a modular dungeon piece, rock or prop. Every stone of a
  wall is its own block: rounded, chipped at corners and edges, its face
  uneven, its tone its own, over a dark mortar core; weathering darkens the
  joints, wears the edges, adds damp stains and rising damp at the foot.
  Pieces are sized in meters on a 1 m grid with y = 0 the floor:

  | Piece | Shape and settings |
  | --- | --- |
  | `wall` | along local X from -length/2 to +length/2 (default 2 x 3 m, 0.5 thick); `block`, `course`, `gap`, `damage` (missing stones, crumbling top) |
  | `corner` | a quoin column on its grid point covering two wall ends; long stones alternate toward +X and +Z (turn it to fit the corner) |
  | `doorway` | a wall (default 3 m) with an `opening` (1.3 m) up to `opening_height` (2 m), `arch` round, pointed or flat, voussoirs `ring` deep, keystone |
  | `pillar` | `kind` round (drums) or square (blocks), plinth, base, capital, abacus |
  | `stairs` | `steps` x `rise` x `run`, `width`, climbing +Z |
  | `floor` | `length` x `depth` of `pattern` flagstone, bricks or cobble, tops at y = 0 over a dark grout bed; `unevenness`, cracked and missing stones with `damage` |
  | `vault` | a barrel vault along Z, span `width`, springing at `height` |
  | `rock` | `size`, `roughness`, `strata` (layers), `facets` (flat broken faces) |
  | `rubble` | a pile `width` across and `height` tall of `count` broken stones and rocks on a dirt mound |
  | `torch`, `candle`, `brazier` | iron, wood, glowing wax, coals and flames, each carrying a light; `kind` wall/stand for torches, `count` for candle clusters, `lit: false` for none |
  | `bones` | `kind` femur, skull, ribs or pile (smooth clay bones) |
  | `chain` | iron links hanging `length` down from the origin |
  | `sarcophagus` | plinth, panelled body and gabled lid |

  Architecture snaps: `at` X and Z to a 0.5 m grid, `turn` to quarter
  turns, `length`/`depth` to whole meters, so a wall at `[0,0,-4]` spans
  x -1..1, the next goes at `[2,0,-4]`, and a corner at `[-4,0,-4]` covers
  both wall ends (`snap: 0` frees it). `seed` gives other stones; `target`
  changes a piece. The kit's materials are `stone`, `mortar`, `grout`,
  `iron`, `wood`, `wax`, `flame`, `ember`, `coal`, `bone`: a scene material
  of the same name (`set_material`) restyles every piece, and `material` on
  a piece replaces its main one.
- `create_object` with `shape: "Terrain"` makes a heightfield (`size`
  [w, d], `resolution` up to 512 cells) for cave floors and outdoor ground:
  rolling or `ridged` noise, a `cracks` network (dried mud, split rock),
  dirt on the flats and `rock_color` on slopes. `edit_terrain` sculpts it
  (`raise`, `lower`, `smooth`, `flatten`, `noise` brushes) and paints it
  (`paint`, `erase`); strokes are stored as grids saved with the scene.
- `scatter` places copies of objects (rocks, bones, debris, candles) over
  an area (rectangle or disc) or dropped `on` surfaces (floors, terrain,
  rubble), with `density` or `count`, `min_distance` (a Poisson disk),
  random `scale`, `yaw`, `tilt`, `align` to the ground, `sink`, `avoid`
  circles and `avoid_objects`, and per-source `weights`. Copies are
  instances (a source and a transform): the viewport draws them instanced,
  film renders them, glTF exports point many nodes at one mesh and FBX
  many Models at one Geometry, and `get_scene`'s `budget` counts what they
  draw (`triangles_drawn`, `copies`, `lights`). The reply says how many
  were placed and why fewer than asked (too close, avoided, off the
  surfaces, too steep).
- `instance_array {target, blob}` turns an `array` blob of a clay into
  instances: one copy is meshed (a hidden clay under the object) and a
  scatter under the object holds every copy, drawn instanced in the
  viewport and shared by `render_film` and the exports. The blob leaves
  the clay. For nested arrays `level: "outer"` (default) instances the
  inner array as one unit and `"leaf"` the innermost shape. It is refused
  when separate copies would not look the same: the blob is not an added,
  unmirrored array, other blobs blend with, carve into or cover the
  copies, or the clay has sculpt, paint, masks or decals. `force: true`
  instances anyway (the copies are then separate closed shapes that do not
  merge). The reply gives the unit, the scatter, the copy count and
  triangles before and after (drawn and unique).

```json
{"target":"Colonnade", "blob":"pillars"}
```
- Any object can carry a `light` (`create_object` / `update_object` /
  `kit`): a preset (`candle`, `torch`, `brazier`, `lantern`, `fire`,
  `magic`) or `{offset, temperature (Kelvin), color, intensity, radius,
  flicker}`. Torches, candles and braziers come with one, and every
  scattered copy of a lit object brings its own. The viewport shades with
  the 16 brightest near the view (warm pools; the studio light dims), and
  glTF exports them as `KHR_lights_punctual` point lights (50 cd per unit
  of intensity).
- `render_film` lights the scene with every object light as a soft
  spherical light (`radius`: bigger, softer shadows), flickering per copy
  and per `frame`. Past four local lights, each point picks two by how
  much they could give it, so 30 candles cost about what one does.
  Flames are seen by the camera but let every other ray through: their
  light is the object's light. Light picked up after the first bounce is
  clamped (like Cycles' indirect clamp), so a bounce landing beside a
  candle does not speckle a dark wall. `fog` adds mist: `{density, color,
  height, falloff, anisotropy, distance}` is denser below `height` and
  thins above it; lights scatter in it (halos and shafts through gaps;
  single scattering on camera rays, equi-angular toward local lights) and
  it dims what is behind it, on CPU and GPU alike. `aspect` makes a wide
  picture, `object_lights: false` leaves object lights out.
- Camera: the `iso` view (`views: ["iso"]`, or `{view: "iso", yaw: 135,
  center, radius}`) is a Diablo-style high three-quarter view, 52 degrees
  down through a long 18-degree lens; `set_camera {preset: "iso"}` puts
  the editor camera there.

#### Build a crypt room

`env_check/build_crypt.py` builds this room through MCP calls and renders
`env_check/crypt_iso.png`. The core of it:

```text
kit {piece: floor, length: 8, depth: 8, at: [0,0,0], damage: 0.35}
kit {piece: wall, at: [-3,0,-4]}            # back walls, full height
kit {piece: doorway, length: 4, opening: 1.6, opening_height: 1.7, at: [0,0,-4]}
kit {piece: wall, at: [3,0,-4]}
kit {piece: wall, at: [-4,0,-3], turn: 90}  # ... and z = -1, 1, 3
kit {piece: wall, at: [-3,0,4], height: 0.6, damage: 0.8}  # low, broken near walls
kit {piece: corner, at: [-4,0,-4]}          # NE turn -90, SW 90, SE 180
kit {piece: pillar, at: [-2,0,-1]}           kit {piece: pillar, at: [2,0,-1]}
kit {piece: rubble, width: 1.8, at: [-2.9,0,-2.7]}
kit {piece: torch, at: [-1.9,1.9,-3.75]}    # on the wall face, sticking out +Z
kit {piece: torch, at: [1.9,1.9,-3.75]}
kit {piece: brazier, at: [0.3,0,0.9]}
kit {piece: sarcophagus, at: [-2.4,0,1.4], turn: 90}
kit {piece: bones, name: src_femur, at: [0,-20,0]}   # sources, out of sight
kit {piece: bones, name: src_skull, kind: skull, at: [0,-20,0]}
kit {piece: candle, name: src_candles, count: 3, at: [0,-20,0]}
scatter {sources: [src_femur, src_skull], on: [floor, rubble], size: [6.5, 5.5],
         density: 0.9, min_distance: 0.32, align: 0.8, avoid_objects: [pillar_l, pillar_r]}
scatter {sources: [src_candles], on: [floor, sarcophagus], center: [-2.4, 1.4],
         radius: 1.5, count: 9, min_distance: 0.45}
render_film {views: [{view: iso, center: [0,0.3,-0.5], radius: 4.3}], size: 1600,
         aspect: 1.78, samples: 256, floor: false, environment: night,
         lights: [{type: sun, direction: [-0.4,1,0.6], color: [0.55,0.65,0.9],
                   intensity: 0.08, size: 25}],      # a dim cool fill
         background: environment, tonemap: aces, exposure: 1.5,
         white_balance: 4200, bloom: {strength: 0.15, threshold: 1.2},
         vignette: 0.4, fog: {density: 0.04, height: 0.3, falloff: 1}}
```

Keep the walls toward the camera low (the iso camera looks in over
them), light with a few strong sources and let corners fall dark, add a
dim cool fill against the warm fire, and use `white_balance` near 4200 K
so stone reads gray-brown rather than orange. A hero and a monster come
from `create_human`, dressed with cover blobs and posed with `animate`
(render with `frame`). An 8 x 8 m room with 20 lights and about a million
triangles renders 1600 x 900 at 256 samples in about a minute on an M2 Max GPU.

### Stylised character bases

`create_character` makes a cartoon character base from proportions alone
(original and generic: no existing design is involved). It is an ordinary
clay of named, blended blobs, ready to shape with `edit_clay` and `sculpt`,
`polish`, rig, retopologise and render:

```json
{ "name": "Buddy", "build": "biped",
  "proportions": { "height": 1.0, "head_units": 2.5, "head_width": 1.0, "body": "round",
                   "limb_length": 1.0, "limb_thickness": 1.0, "hands": "four_finger",
                   "feet": "round", "eye_size": 1.0, "eye_spacing": 1.0, "ears": "round", "nose": "button",
                   "mouth": "smile" },
  "eyes": true, "rig": true, "symmetry": true, "guides": true, "look": "stylized" }
```

- Every field is optional; the defaults are a friendly 2.5-head creature
  (the plan's benchmark proportions). Ranges: `height` 0.1-10, `head_units`
  2-5 (need not be whole), `head_width` 0.5-1.5, `limb_length` 0.6-1.4,
  `limb_thickness` 0.6-1.6, `eye_size` 0.5-2, `eye_spacing` 0.5-1.5;
  `body` round, pear or slim; `hands` mitten, four_finger (three fingers
  and a thumb) or five_finger (bipeds only); `feet` round or long; `ears`
  none, round or pointed; `nose` none or button (a round, blended button
  blushed toward pink); `mouth` smile (a soft smile line carved under the
  nose, its corners turned up), grin (wider) or none. `build: quadruped` stands
  the same kit on four legs with a neck and a tail.
- Everything is measured in head units, u = height / head_units, the
  head's height. The soles stand on y = 0 and the crown is the model's top
  (ears and tails stay under it), so `guides measure` reads the head's
  share of the height as exactly 1 / head_units.
- Blobs are named (pelvis, torso, neck, head, ear.L, nose, mouth,
  mouth_line, eye_socket.L, thigh.L, shin.L, foot.L, upper_arm.L,
  forearm.L, hand.L, finger1.L, thumb.L; a
  quadruped's front_ and hind_upper_leg.L, _lower_leg.L, _paw.L and tail).
  With `symmetry` each pair of sides is one mirrored .L blob; `symmetry:
  false` writes both sides out (the same surface) to sculpt each alone.
  Legs are capsules; arms, fingers and tails are exact round tubes (arms
  full at the shoulder, swelling past the elbow, slim at the wrist).
- `eyes` carves sockets and seats Eye objects `NAME_eye.L` / `.R` in them,
  riding the head bone. A socket is a cone from the eye's centre (62
  degrees round from its axis), so its wall meets the eyeball square-on
  and no gap or dark ring shows round the eye; its blended edge reads as
  a soft lid. `look: stylized` (default) makes them stylised eyes and
  puts the `skin_stylized` preset on the clay as `NAME_skin` (or the
  first free `NAME_skin2`, ...), its `subsurface_radius` scaled to the
  height (0.45 per unit), so the film tracer's subsurface gather stays
  within the gaps between arm and body (at the preset's 1.6 a 1 m base
  showed dark red patches there).
- `lids` (with eyes): `upper` (default), `both` or `none`. Each lid is a
  shell wrapping the eyeball (a `lid` blob, below): it clears the cornea's
  bulge, covers `upper_cover` of the eye's height seen from the front
  (default 0.3; 0.5 reads sleepy) down to a rounded margin, and is
  blended into the socket and the brow in the skin's colour, on the head
  bone (`lid.L`, and `lid_lower.L` covering `lower_cover`, default 0.15,
  from below; together at most 0.9). They are mirrored blobs: `edit_clay
  {split: ["lid.L"]}` then an update of `lid.R` closes one eye more.
- `rig` places bones inside the blobs (hips, spine, a neck helper, head,
  arms and hands, legs and feet; a quadruped's legs, paws and tail), each
  blob on its bone.
- `guides` replaces the scene's proportion guides with the base's: head
  units counted down from the crown in whole heads (for 2.5 heads the
  last line falls under the soles), center and eye_line lines, and the
  targets `head_ratio`, `eye_size` (eye height over head height),
  `eye_spacing` (per head unit) and `eyes_on_line`. The reply carries the
  measure, so the call shows at once that the base is what was asked.
- The resolution is chosen so the thinnest round parts (fingers, ear
  tips) are at least 2.6 cells thick (192-384 cells; `resolution`
  overrides it, and those parts are thickened to 2.6 cells if needed).
- The same arguments give the same blobs, bit for bit. Out-of-range
  values, unknown keys, eyes too big for the head, a name already in the
  scene and hands on a quadruped are refused with nothing changed. One
  call is one undo step. Create > Character Base sends the call with the
  defaults.

#### Eyelids on any clay and Eye object

A `lid` blob wraps an eyeball in skin: a spherical shell round the Eye
object, its inside clear of the cornea's bulge, a few cells thick,
covering the eye from the top (or the bottom) down to a rounded margin,
blended into the socket. Name the eye and the rest is filled from it:

```json
{ "target": "Pip", "remove": ["lid.L"],
  "add": [{ "type": "lid", "eye": "Pip_eye.L", "upper_cover": 0.42, "mirror": true }] }
```

- `upper_cover` (or `lower_cover` for a lower lid) is how much of the
  eye's height the lid hides seen along the eye's forward: 0 open, 1
  closed. The margin is where a lid turning about the eye's sideways
  axis stops: an arch over the iris below half, a dip past it (sleepy).
  `tilt` (degrees) turns the margin, raising its +x end (the outer corner
  of a `.L` eye; a mirrored copy raises its own outer corner).
- From the eye as it sits at rest in the clay's space: the centre, radius
  and forward, and `gap` = the cornea's bulge (`cornea` + 0.012 eye
  widths) + `clearance` (default half a cell). `thickness` defaults to
  0.08 of the eye, at least three cells; `blend` to twice the thickness
  (at most a fifth of the eye), so the dome rolls into the brow with no
  crease round it; the colour is the skin's over the eye; the bone the
  eye's; the name `lid.L` / `lid_lower.L` from the eye's side (`lid_EYE`
  without one). The stored blob is geometric (`{center, radius, gap,
  thickness, forward, up, cover, lower, tilt}`), so it saves, poses (it
  rides its bone exactly) and mirrors like any blob. Put it after socket
  carves.
- One eye more closed: `{split: ["lid.L"], update: [{blob: "lid.R", eye:
  "Pip_eye.R", upper_cover: 0.6}]}` (the split side is filled from its
  own eye). A head tilt by `pose_bone` splits the mirrored lid by itself.

### Human bases

- `create_human` starts a character from a correct human: MakeHuman's CC0
  base mesh and 800 shape targets are built in (`assets/makehuman`, converted
  by `tools/convert_makehuman.py`; data only, no MakeHuman code). One call
  makes a body clay with a full rig (spine, neck, head, arms with three bones
  per finger, legs, feet), a finer head clay on the head bone, and Eye
  objects in the sockets. Controls: sex, age, weight, muscle, height,
  proportions, ancestry, and any named face or body shape
  (`human_controls` lists them; `ears/ear-shape-pointed` makes elf ears).
  Skin color is independent of ancestry; the default is a warm
  `[0.82, 0.64, 0.54]`, adjustable with `skin_color`.
- The human is a clay blob (`type: "human"`, with `part` whole / head /
  body): its exact distance to the subdivided mesh, so sculpting, paint,
  rigs, bakes and film renders all work on it. `set_human` changes the
  controls later; the head, eyes and bones follow and clips are kept.

### Faces

A `create_human` head knows where its eyes, nose, mouth, chin and ears are
(MakeHuman markers and fixed base-mesh vertices, which follow every shape
target), and its skin is shaded by region (`src/face.rs`), in film (CPU and
GPU alike) and in bakes:

- pores in three sizes: fine on the forehead, medium on the cheeks, large on
  the nose; pits with slightly darker, rougher insides, plus soft skin-cell
  bumps and a fine cross-hatch of micro-lines everywhere;
- wrinkles along real expression lines, deeper with age: forehead lines,
  frown lines, crow's feet, under-eye creases, nasolabial folds, and for
  older skin marionette and upper-lip lines; they break up along their
  length like real lines;
- an oily T-zone (lower roughness and a thin glossy coat), matte cheeks;
- color: redness on nose, cheeks, chin and ears, a cool tint under the
  eyes, mottling, and when asked freckles, moles, age spots and stubble
  (cut hairs and beard shadow over the beard area);
- lips: a rose vermilion from the skin color (or `lip_color`) with a soft
  border and pale rim, darker toward the meeting line, fine vertical lip
  lines and a moist highlight.

One call sets it: `set_human {target, skin: {age: 35, pores: 1, wrinkles:
0.6, freckles: 0.2, redness: 0.4}}` (also `create_human skin`). Other keys:
`moles`, `under_eye`, `oil`, `stubble`, `fuzz`, `lip_color`, `lips`, `seed`,
`enabled: false`. The face fades out down the neck into the body's plain
skin.

- `face_detail {target}` sculpts the broad part of the folds (nasolabial
  folds, forehead furrows, crow's feet, under-eye creases...) into the head
  as geometry on a 0.25 mm detail grid (`detail_cell`, `strength`); about
  two minutes. Finer detail stays in shading.
- `face_hair {target}` grows brows (hairs in the real direction pattern,
  dense in the body and sparse at the edges), lashes (two staggered upper
  rows that curl up and fan out, a sparse lower row, in small clumps, from
  the lid margins) and peach fuzz (thin pale vellus hair that shows in rim
  light) as Hair objects on the head. Each part takes `true`, `false` or
  `{density, color}`; `create_human face_hair: true` does it at once. Film
  renders strands (`device: "gpu"` renders them on the GPU too).
- Eyes are textured: iris fibers, crypts, a lighter collarette, contraction
  furrows and a dark limbal ring; a white with fine red veins toward the
  corners (`veins`, `limbal_ring`). The iris sits in a shallow bowl under a
  clear cornea (glass, index 1.376) that refracts it and gives sharp catch
  lights. `create_human` sizes the eyeballs to the sockets and seats them
  so the lids rest on them, and records the lid margins: the white darkens
  where the lids meet it, a wet tear line runs along the lower lid and a
  pink caruncle fills the inner corner; these stay with the lids as the eye
  turns.
- Stylised eyes: `eye: {style: "stylized"}` gives the big, clear, catch-lit
  eyes of animation: a clean radial iris (lighter toward the pupil, a few
  soft fibers, a crisp limbal ring), no veins, a flatter cornea, and a
  catch-light, a small glowing spot on the iris up and to the right in the
  eye's own frame, seen through the cornea whatever the lighting.
  Switching stores `iris_scale` 1.3 (1-1.6: scales `iris_size`, at most
  0.9 of the eye), `cornea` 0.03, `veins` 0, `limbal_ring` 0.8 and
  `catchlight: true`, except what the same call sets; `pupil` is
  `pupil_size` (0.05-0.95). The material tab's "Stylised eye" button sends the same
  `update_object`.
- `bake` on a face head defaults to 4096 textures, so pores, lines, lips and
  color variation survive in the exported color, normal and roughness maps
  (the oily coat goes into roughness).

### Background work

Long agent work (`bake`, `render_film`, `check_export`, `export_glb`, `export_fbx`, `sculpt`, `face_detail`, and
File > Export) runs on its own thread with a progress bar in a "working on"
panel above the help line, so the window stays live. Agent requests are
served in parallel, so other calls answer while a job runs. Clay mesh
rebuilds, skin weights and fine meshes are built on other threads too: the
window keeps showing the old mesh, agent calls wait in line (calls that read
no meshes, such as `screenshot`, go ahead), and edits that report the new
mesh answer once it is built. `edit_clay` waits at most `wait_s` (default
20 s, 0 answers at once) for its rebuild: a slower one answers with
`mesh_pending: true`, `estimated_build_s` (the object's last build of that
kind: regular or fine; shading-only refreshes do not count) and
`build {object, fine, started, fraction}` instead of the bounds, while the
build goes on. `object` is whose build it is (an object with the same shape
as one already building waits for that build), `fine` says it is the fine
mesh's, and `started` and `fraction` are seconds since it began and 0–1
done. `job_status` without an id lists running mesh `builds` with their
`fraction` done.

No agent call waits more than 40 s (agent connections give up after about a
minute). Past that it answers with a job id and a status: `running` (a
job), `done, answer pending` (the change is made; its summary waits for
meshes) or `waiting in line` (not run yet; it runs later, in order).
`job_status {id, wait: 45}` answers as soon as that job is done (or with
percent and seconds left), and `cancel: true` stops a job or drops a call
still in line. On macOS the editor opts out of App Nap, so it keeps full
speed behind other windows. Undo skips playback changes (which clip plays).

### Animating a character

- `rig`: add bones inside a clay object (head, tail, parent; `points` for a
  chain; `mirror` for the other side). Skin weights are made automatically:
  a vertex follows the bones in its own blob, then the nearest one it can
  reach through the clay, smoothed so joints bend softly. The reply lists how
  many vertices each bone moves and warns about bones outside the clay.
  By default a rig is split: each moving part (body, head, each limb) is
  its own closed shell, so a limb that swings far leaves no dent where it
  was melted into the body. Surfaces that were buried take the color of the
  surface that covered them. `split: false` keeps one melted shell.
- A blob's `bone` overrides its automatic assignment (for example, horns
  with `bone: "head"`), with a soft blend from neighbors at seams. Mirrored
  blobs swap `.L`/`.R` and other supported side suffixes. Use an existing
  deform bone; an unknown name falls back to automatic weights.
- `rig weights` paints named bones with world-space rest-pose paths. Each
  stroke has `bone`, `points`, `radius`, `strength` (0–1), and `mode`:
  `add`, `subtract`, `replace` (strength is the target weight), or `smooth`
  (averages the named bone over surface neighbors). Brushes have soft
  falloff; other weights redistribute proportionally, then the strongest
  four normalize. Paint applies after automatic weights and blob overrides,
  survives rebuilds and save/load, and carries through bake and both exports.
  `clear_weights: true` removes paint; renaming/removing bones updates paint.
  Use `split: false` for a continuous wing blending between arm and spine.
  Inspect it with `render_views weights: "spine"`.

```json
{"target":"Dragon", "split":false, "weights":[
  {"bone":"spine", "points":[[0.1,1,0],[0.1,1,0.5]],
   "radius":0.3, "strength":0.5, "mode":"replace"}
]}
```

- `animate`: keyframes in named clips (`idle`, `walk`...): per bone `rotate`
  (degrees), `translate`, `scale` at time-slider frames, eased `smooth`,
  `linear` or `step`, with `mirror` for the other side. Clips loop by default.
- `playback`: go to a frame, play or stop, set fps, pick which clip plays
  (`clip: null` shows the rest pose).
- `render_animation`: one view at several frames as a contact sheet. It is
  a background job like `render_film` (a snapshot of the scene renders on
  its own thread): a long one answers with a job id, `job_status {id, wait:
  45}` fetches the sheet. `clip` picks the clip to show, `save_path` also
  writes the PNG; 16 frames of a big scene no longer time out.
  `format: "png_sequence"` also writes every frame (`frame_00000.png` ..)
  into the absolute folder `path`; `format: "mp4"` writes a movie at an
  absolute `.mp4` `path` (H.264 through libx264, else mpeg4). `frames`
  then goes up to 1200 (default every whole frame of the clip; the sheet
  shows 16), `fps` defaults to the timeline's and `size` is the frame size
  (default 512, at most 1024, made even). mp4 needs an `ffmpeg` binary,
  found through `CLIDE_FFMPEG` (a path to the program), then the PATH,
  `/opt/homebrew/bin`, `/usr/local/bin` and `/usr/bin`; it is run with an
  argument list, never a shell. Without one the frames stay as a PNG
  sequence in `<name>_frames` next to the movie path and the reply warns.

```json
{"clip":"walk", "view":"three_quarter", "format":"mp4", "path":"/abs/out/walk.mp4", "fps":24, "size":720}
```

  `render_views` also takes `frame`, `show_bones`, and `weights` (a bone name:
  shows its skin weights as colors).

### Export

- `export_glb` (or File > Export glTF): writes a `.glb` that Blender, Unity,
  Unreal, Godot and three.js open. It holds every visible object with its
  colors (as vertex colors) and materials (one primitive per material;
  clearcoat and sheen as KHR extensions), and for rigged clay the skeleton,
  skin weights and every clip as an animation, sampled once per frame. Files
  pass the Khronos glTF validator with no errors or warnings.
- `check_export` loads an absolute `.glb` `path` from disk and renders the
  exported file with the film path tracer. Use it after exporting to check
  the delivered geometry, textures and pose. It returns a picture plus
  vertex/triangle counts, meshes, materials and texture slots, image sizes,
  skins/joint counts, animation names/frame ranges, bounds and warnings.
  `views` defaults to `["three_quarter"]` (up to four, including `{yaw, pitch}`),
  `size` to 512, `samples` to 32, and `time_limit` to 40 seconds; `save_path`
  saves the PNG. `animation` selects a clip and `frame` samples it at the
  file's fps (frame 0 is clip time zero); frame alone selects the first clip.
  Omit both for the rest pose. New exports record fps; older files infer it
  from sample spacing and report that assumption. This is a background job:
  fetch late results with `job_status`. It supports embedded PNG/JPEG,
  triangle meshes, skeletal TRS animation with LINEAR/STEP sampling and
  baked color/normal/ORM textures. Unsupported required extensions,
  compression, external resources and cubic animation return errors;
  morph targets and alpha blending are reported as render limitations.
- `export_fbx` (or File > Export FBX): the same content as binary FBX 7.4
  for Maya, Unity, Unreal and Blender. Each material becomes a Phong
  material on its triangles, colored with the average color of its part for
  tools that ignore vertex colors; roughness and metallic become Shininess
  and ReflectionFactor. One unit is one meter.

Props can ride on a bone: `create_object` / `update_object` with `parent`
(the rigged clay) and `bone` (e.g. `hand.R`). The prop is placed in the rest
pose and then moves rigidly with that bone; both exports keep the link.

The viewport draws skeletons (Show > Joints) and the time slider marks keys
in red. Sculpting, paint and projection work on the rest pose.

Agent edits run on the UI thread and go into the same undo history as mouse
edits, so ⌘Z undoes what an agent did.

### Simulated garments

`cloth` makes a separate sewn garment on a clay parent and drapes it as a
cancellable background job. Unlike a clay `cover`, its hem hangs freely,
fabric bridges body contours, and folds have real geometry. One call:

```json
{"parent":"Elf","name":"Tunic","pattern":"tunic","length":"mid_thigh",
 "sleeves":true,"belt":1.0,"fabric":"linen","color":[0.2,0.35,0.12]}
```

Patterns size themselves from the parent's body and rig in **parent rest
coordinates**, in meters. `length` is meters or `waist`, `hip`, `mid_thigh`,
`knee`, `ankle`. `shirt` and `tunic` have fitted shoulder seams and
`neckline: "round" | "v" | "boat"`. `sleeves: true` extends both armholes
into the same connected simulation mesh, through elbows to fitted wrist
cuffs. Cap rings and elbow seam tacks keep the sleeves on the arms; the
remaining fabric drapes freely. The collar and shoulder anchors snap to
the body surface. Omit sleeves for a sleeveless cut. A tube's default
length is its bone length. `tube` needs `bone` or explicit `center`; optional `radius`
is `[width_radius, depth_radius]` in its cross-section. Without a radius,
tubes follow a smoothed body cross-section along the bone, tapering at
wrists and ankles. For boots, use a
leather shaft tube on the shin and model the shoe/sole separately.

`skirt` pins its waist ring; `flare` increases hem circumference (default
0.3). `gather` supplies excess waist fabric for pleats (default 0.18, range
0–0.75). `cape` is a widening panel with two shoulder clasps. `panel` accepts
3–20 ordered points of any simple polygon and `pins` as polygon corner
indices (default first two). On other patterns `pins` adds simulation vertex
indices. Explicit `center` and `radius` override automatic sizing.
`belt` is the local Y of a cinched pinned band beneath a visible belt.
A mid-thigh tunic with sleeves and a belt pin is a complete garment;
there is no need to overlap separate torso and skirt hems. Keep or create
the visible leather belt separately. Remove old clothing covers/filler
blobs before draping; they participate in body collision. When a belt pin is present,
collision automatically excludes parent blobs named with `belt` or
`buckle`, so existing hardware remains visible over the fabric.

Presets `cotton`, `linen`, `wool`, `silk`, `leather`, and `denim` control
stretch/shear compliance, bending compliance, areal mass, damping and
friction. The solver uses fixed substeps and deterministic constraint order,
clay SDF contact, contact against the displayed body triangles, and
vertex/triangle self-contact between nearby folds.
`fit` is initial ease (default 2 cm); collision clearance is half thickness
plus one tenth of fit; leather adds a 3 mm guard against the meshed body
showing through. `thickness` is full thickness, default 2 mm, with
rounded edge/hem rims. The default `seconds: 2` settles the garment;
`{"target":"Tunic","seconds":4}` settles it further. Changing pattern
parameters on a target regenerates its cut and clears its bake/cache.
`job_status` reports progress or cancels; a cancelled job makes no edit.

Skin weights transfer from the nearest body surface; waist/belt pins follow
hips. Playback follows the parent rig. During cached simulation, sleeved
shirt/tunic upper pieces follow those skin weights, and the free hem below
the belt (or hips) simulates inertia. This avoids ambiguous inverse body
contact at bent elbows; skirts, capes and panels still simulate their free
vertices. To simulate inertia and skirt swing:

```json
{"target":"Skirt","clip":"walk","frames":[0,1,2,3,4,5,6,7,8],"fps":24}
```

Use consecutive frames through the whole clip (up to 240 cache frames).
The solver interpolates body motion at 120 substeps/second; the viewport
and film interpolate the saved cloth positions within the cached range.
Outside that range they use ordinary skinning. Re-simulate after editing
the body or clip. Animated SDF collision uses the nearest posed body's
inverse blended skin transform, supplemented by the actual posed body
triangles. Extreme joint deformations and fast motion can need more mesh
resolution and smaller frame gaps. Self-contact is
local and discretized, rather than an exact continuous collision solver.

`resolution: 8..96` controls simulation tessellation (default 56).
`subdivision: 0..2` applies Loop smoothing to the simulated midsurface
(default 1), then builds smooth normals, thickness and rounded hem rims.
Subdivision also interpolates skin and cached motion, without increasing
solver cost. Sleeves use separate UV pattern pieces while sharing seam
vertices. Use 48–80 around a belt or elbow for smaller folds. Material colors, weathering, film, save/load,
duplication and undo work like other objects. `bake` simplifies to the
requested triangle budget, keeps pattern UVs and skin, and bakes lost folds
and woven microrelief to a normal map. A sewn UV seam prevents stretched
weave; the two cloth faces occupy separate halves of the texture. If the
budget cannot preserve seams, bake reports an error: lower resolution or
raise triangles. Generated pattern UVs run across/along the fabric.

GLB and FBX export the draped garment with its parent's animated skeleton.
For cached garments, exports instead bake the cached vertex motion and
normal rotation to a temporary joint per render vertex. This portable
vertex animation is supported by `check_export`, but makes larger files and
many joints; use ordinary skinning for a compact game rig, and a lower
cloth/bake budget for animation caches. Other clips and frames outside the cache retain ordinary skin motion in
the exported vertex animation. No extra dependencies or MakeHuman program code are used.

Reproduce the film comparison with:

```sh
CLOTH_SCENE=/absolute/path/to/elf8/elf.json cargo test --release elf_cloth_film_check -- --ignored --nocapture
```

It writes the original, draped outfit, side view, walk frames, scene, GLB
and an independently loaded export render into `cloth_check/`.

### Forged armor and weapons

Clay preserves sharp geometry by default. Plain boxes and unbeveled polygon
extrusions use exact, closed geometry tessellated to the requested density;
sculpt, paint layers and combined CSG use the field mesh. Cells with sharply
differing field normals use a box-constrained Hermite QEF; smooth cells retain
surface nets. Refined crossings also correct planar cells next to corners,
and winding-safe surface fans retain sharp vertices when a quad needs them.
Sharp dihedral creases also split render normals, so metal highlights stop at
plate corners. Positions remain coincident and geometric connectivity is
closed; exports retain the separate normals. Smooth regions keep field
normals. Hard forms target three cells across authored thicknesses and bevels at the
default grid setting, bounded to 192–512 cells across the object; explicit
other resolutions are honored. Fine meshes, rigs and bakes
use the same placement. Saddle faces that need more than one dual vertex
use a consistent tetrahedral fallback to retain closed, manifold geometry.
`edit_clay sharp:false` restores the previous placement for comparisons.
Use `blend:0` for hard unions and cuts. `join:"sharp"` also forces a hard
join; `join:"chamfer"` creates a diagonal join, and `join:"round"` (default)
creates a smooth join. `join_size` sets the size (defaults to `blend`);
`size` remains the box/bead size vector.

New blob forms, accepted in `create_object blobs` and `edit_clay`:

| Form | Fields | Use |
| --- | --- | --- |
| `extrude` | `profile`, `depth`, `center` (0), `axis` (+Z), `bevel` (0) | Polygon plate, diamond blade, buckle |
| `lathe` | `profile`, `center` (0), `axis` (+Y) | Helmet, pommel, ring, stud; profile is radius/height |
| `sweep` | `profile` (a polygon, or `{circle: r}`), `points`, `taper` (1), `twist` (0 degrees total), `path_smooth` (true), `radii` (circle only), `field` (`segments` for new sweeps) | Trim, strap, ridged horn; wires, cables, strands, piping and grooves with a circle |
| `shell` | `source` (geometry form), `thickness` | Hollow one form inward, preserving its exterior |
| `array` | `source`, `count`, `step` | Repeat along a line; `radius`, `center`, `axis` select a circle; `points` selects an arc-length path; with `orient: "path"` copies turn with the path (the first copy as authored), so grousers wrap around a track loop |
| `band` | `points`, `width`, `normal` (+Z) | Use with `op:"engrave"` or `"emboss"`; `wall` is constant depth |

Profiles are simple closed polygons (omit a repeated closing point), either
winding, with 3–128 `[u,v]` points. An individual rounded corner is
`{"point":[u,v],"radius":0.002}`. A smooth profile control is
`{"point":[radius,height],"smooth":true}`: neighboring sections interpolate
with cubic tangents while ordinary points retain sharp profile transitions.
Use these on lathe domes and bosses; keep the base flange and rolled rim
endpoints sharp. Smooth profile controls should have enough spacing to avoid
self-intersection. Extrusion is centered on `center` along
`axis`; +Z uses an XY profile. Other axes rotate that frame from +Z. Sweeps
start with the same XY section, transport it along the path and interpolate
taper and twist by arc length. Sweep paths use distance-weighted cubic
splines, sampled at about 2 mm intervals (8–96 steps per input segment),
and interpolate transported frames. A perpendicular-section solve follows the continuous curve, avoiding
internal segment caps and seams. Set `path_smooth:false` for a deliberate angular path.

That describes the `legacy` field, which sweeps saved without `field` keep so
old scenes mesh as they did. Sweeps an agent adds (`create_object blobs`,
`edit_clay add` or `replace`) without a `field` get `field: "segments"`.
`get_clay` (verbose) shows a saved sweep's `field: "legacy"`, so blobs read
there and sent back with `replace` keep their mesh (and sculpt and morphs
made on them stay in sync); saved files still leave it out. Segments: the path is cut once
into straight spans short enough that each chord strays at most 1/128 of the
thinnest radius from the curve (at most 4,096 spans; a path too curved for
its radius loosens that), and the field is the union of one exact piece per
span. With `profile: {circle: r}` (0.0005–10) each piece is a round cone, so
the tube is exact and smooth at any size and counts as a smooth form (no
192-cell floor, no sharp-feature placement), like a capsule. `radii` gives
one radius per path point instead of `taper`; twist does not apply to a
circle. Polygon profiles become rigid prisms on rotation-minimising frames,
joined by mitre planes; past a 120° turn each span ends square, which leaves
a notch on the outside of the bend. `edit_clay update [{blob, field:
"segments"}]` upgrades a legacy sweep. A circle profile is exact only with
segments: `update [{blob, profile: {circle: r}}]` on a legacy sweep keeps it
legacy, a hard 16-gon that refuses `radii`, and the reply's `notes` say how
to upgrade it. Use a circle sweep for thin tubes
rather than a 16-point polygon or a chain of spheres: on a 4–6 mm tube along
a curved path the legacy 16-gon's field changes up to six times faster than
distance (lumpy, fuzzy normals), the circle's never faster.

Carves and tubes are cheap far from their path: a carve is looked up only as
near as could still change the value there, and a tube's lookup walks a tree
of its spans, so dozens of carve sweeps at a fine cell rebuild in seconds.
`path_smooth` belongs to sweep geometry; the numeric blob `smooth` controls
body fitting. Fine cells and sufficiently round profiles still matter: a
hexagonal profile makes a ridged horn, a 20–32 point circle a round horn. Arrays accept 1–512 copies;
nested arrays are limited to 4096 evaluated copies and nesting to 8 levels.
Circle arrays rotate each copy around the center; line/path arrays translate
copies without rotating them. Put the source near the origin. `source`
contains geometry only; color, material, operation, bone and join belong to
the outer blob.

`edit_clay shell:0.004` hollows the entire blob CSG inward before sculpt
detail; `shell:null` removes it. A shell form hollows only its source.
`op:"plate"` on any limiting shape makes a shell at `offset`
(default 0.004), with full thickness `wall` (default 0.004) and a `rim`
bevel footprint. For forging, use a **large `smooth` footprint**: 0.08–0.14 m
on a torso, 0.03–0.06 m on a greave or vambrace. This averages the complete
earlier body field over a broad volume, bridges anatomy, and reconstructs
a smooth shell from a cached fitting grid. A sampled clearance pass inflates
the entire filtered shell to cover body protrusions, preserving its sculpted
curvature. Earlier plate operations are excluded from this fit, so stacking
limb plates does not repeatedly grow the body. Zero retains the exact anatomical
fit for compatibility. Increase the limiter to contain the expanded shell.
`ridge:0.015` creates a sharp center crest about the limiter’s local X
center; `flare:0.006` turns its outer edge outward over four wall thicknesses.
The same options work on shoulder and limb plates. Add a round sweep along
the rim for a rolled lip, or a second plate for overlapping lames. Keep `rim` below half the wall. Set `bone:"spine"` or another
existing deform bone for rigid armor; plates retain their body source when
the rig splits parts. Use `op:"cover"` for a solid fitted garment.

Bands project their path along `normal` onto the earlier surface and lower
or raise it by `wall`, with a flat constant-depth interior and crisp side
walls. Their footprint has rounded path ends. Place paths with
`surface_point`; the footprint extends 1.5 depths on either side of each path point. Use a
separate band for each surface direction, with paths close to the surface.

```json
{"target":"Hero", "add":[
 {"type":"box","name":"breastplate","center":[0,1.18,0.12],
  "size":[0.45,0.48,0.40],"op":"plate","offset":0.003,
  "smooth":0.10,"ridge":0.015,"flare":0.006,
  "wall":0.005,"rim":0.0015,"blend":0,"material":"metal","bone":"spine"}
]}
```

```json
{"shape":"Clay","name":"Blade","resolution":384,"material":"metal",
 "blobs":[{"type":"extrude","profile":[[-0.025,0],[0,0.003],[0.025,0],[0,-0.003]],
 "axis":[0,1,0],"depth":0.8,"bevel":0.001,"blend":0}]}
```

All lengths are meters: rivets 4–6 mm across, plate walls 3–5 mm,
blade edge bevels about 1 mm. Keep thickness, grooves and rivets at least
two mesh cells wide. A body-sized mesh cannot resolve a 1 mm bevel at normal
resolution; put small props in separate clay objects and set `detail_cell`
for film/bake detail. Grid sampling can still miss sub-cell parts, and
tightly folded sweep paths need clearance for their cross-sections.

Reproduce the film comparisons and mesh timings with:

```
cargo test --release hard_film_checks -- --ignored --nocapture
```

Images and editable scenes are saved in `hard_check/`: before on the left
(surface nets and shared normals), after on the right (feature placement and
split crease normals). Each pair uses identical geometry, materials, cells,
camera and lighting. The 768 px macro views inspect a plate rim, sword edge,
crossguard corner, rivet and engraved groove at 0.1–0.3 mm cells. Local
feature replicas keep the same cross-sections and construction with less
distant geometry, allowing finer sampling within the 1600-cell limit; overview cameras alone cannot
prove millimeter features. `HARD_CHECK=shield` (or another scene name) limits
the run. Source recipes are in `src/hard_showcase.json`; the breastplate adds
surface-following trim over the same human and rig as `create_human`.

For worn metal, use `metallic:1`, `roughness:0.3–0.45`, restrained
`edge_wear`, `cavity` and `dirt`, and `detail:"metal_scratched"` with low
`detail_strength` (about 0.2). Steel and aged brass should differ in color
and roughness. Avoid excessive relief that overwhelms the plate’s shape.
Layered lames, mounting collars, a fullered blade, raised shield boss,
engraving and visible rivet heads make the silhouette and construction
read at overview size.

### Kitbash: hard-surface detail (panels, bolts, vents, cables)

AAA vehicles, weapons and mechs read as real machines because of three
levels of detail: primary forms, **secondary** (panels, plates, hatches,
vents, pistons, cables) and **tertiary** (bolts, rivets, seams, latches,
handles, lights, stencils). Model the primary forms (clay, kit pieces,
primitives), then add the other two levels quickly:

1. `detail_pass {pass: "panel", targets, mirror: true}`: big flat faces
   become raised chamfered plates with seams between them, bolts at their
   corners and along long seams, and a hatch or two;
2. `detail_pass {pass: "greeble"}` (vents, junction boxes, canisters,
   lights, handles, connectors, latches) and `{pass: "bolt"}` (rows of
   bolts along hard edges and around every flat face, rounded edges too);
3. `detail_pass {pass: "cable"}`: hoses and cable bundles across the joints
   between the targets' bones, with P-clips and connectors;
4. a few hand placements with `kitbash`: pistons behind the knees, hoses
   at the hips, lamps, handles, latches, spare track links, exhausts,
   antennas, stencils.

```text
detail_pass {pass: panel,   targets: [Torso, Thigh.L, Shin.L], mirror: true, hatches: 3}
detail_pass {pass: greeble, targets: [Torso, Shin.L], mirror: true, density: 1.6,
             parts: [{kind: vent, style: louver, weight: 2}, {kind: greeble, style: junction}],
             paint: gunmetal, color: [0.2, 0.2, 0.21]}
detail_pass {pass: bolt,    targets: [Torso, Thigh.L, Shin.L], mirror: true}
detail_pass {pass: cable,   targets: [Pelvis, Thigh.L, Shin.L], mirror: true}
kitbash {part: piston, targets: [Thigh.L, Shin.L], from: [1.02,2.85,-0.32], to: [1.14,1.75,-0.36], mirror: true}
kitbash {part: {kind: light, style: cage}, target: Torso, at: [0.72,4.45,0.98], normal: [0,0,1], mirror: true}
kitbash {part: stencil, target: Thigh.L, at: [1,2.9,0.6], text: "NO STEP"}
kitbash {action: remove, group: kb_greeble_1}     # undo a whole pass
```

- **Parts** (`kitbash action catalog` lists them with sizes): bolt (hex,
  socket, rivet, countersunk, carriage), nut, washer, hinge, latch, clamp,
  handle, vent (louver, slots, grille, fan, mesh), hatch, piston, cable,
  hose, corrugated_tube, pipe, strap (these run between points), light,
  antenna, sensor, exhaust, armor_plate, track_link, gear, spring,
  connector, ammo_box, jerry_can, net, stencil (raised letters A-Z 0-9),
  seam, greeble (box, canister, pipes, junction, tank) and clip. Each is
  parametric (`size` [along X, out of the surface, across], `style`,
  `count`, `radius`, `sag`, `clips`, `text`, `seed`...) and deterministic,
  built with chamfered hard edges (flat faces stay flat, round ones are
  shaded round), UVs in meters over the part's size, and a material per
  role: `paint` takes the surface it sits on (its material and color),
  `metal`, `dark`, `rubber`, `chrome`, `glow` (lit lenses), `glass`,
  `brass`, `seam`, `marking`, `strap` default to built-in `kb_*` materials
  that a scene material of the same name replaces; `materials {role:
  name}` picks others. Height masks of smart materials are left out on
  parts (one mesh serves copies at every height). `kitbash_check/catalog.png`
  and `styles.png` show every part and style.
- **Scale-aware**: unsized parts and every pass size scale with the asset
  (its root's largest side over 6.5 m): bolts are 4.5 cm on a 6.5 m mech
  and 7 mm on a 1 m rifle. `scale` multiplies a pass's sizes.
- **Placement**: `at` (snaps to the nearest surface, or along `normal`),
  `points`, `path` + `spacing`, `array` ({count, step} | {count, to} |
  {grid, spacing}), `along` (a direction, or `"edge"`: the nearest hard
  edge, with `count_along` copies along it), `rotate`, `offset`. Parts
  never float or sink: each is projected onto the surface and sunk just
  enough that no footprint corner floats (reported as `max_sink`); a part
  overhanging an edge slides inward to where it fits (`slid`). Point parts
  take `from`, `via`, `to` with their ends snapped onto the surface.
  `mirror: true` also places the reflection across `mirror_x` (default 0),
  landing on whatever surface is there (the other leg).
- **Passes** look at the targets' surfaces: flat regions (grown from
  triangles within 4 degrees of one plane), hard edges (over 25 degrees
  between faces), and per face a room mask of where there is space:
  stencils and flags (paint far from the face's color), emblem, image and
  rune decals, and parts already placed are kept clear; faces turned to
  the ground are skipped unless `facing` asks. Panels cover `coverage`
  (0.6) of a face, leaving a ring for border bolts and room for greebles.
  With `mirror`, a pass runs on the +X side and mirrors, keeping off the
  other side's own markings. `budget` caps the triangles drawn (bolts go
  first, then greebles, then plates). Replies say what was placed and why
  spots were skipped (`no_room`, `crowded`, `curved`, `off_face`).
- **Rig and export**: parts hang on the bone their surface rides (an
  object on a bone, or the bone moving that spot most on rigged clay), so
  they animate and glTF / FBX export them under the joint. Repeated parts
  are instances: one hidden source per distinct part under
  `kitbash_library`, copies in a `Scatter` per group, drawn instanced in
  the viewport and film, and exported as many nodes sharing one mesh.
  Parts stay real geometry with their own UVs (glTF writes TEXCOORD_0) and
  vertex colors; bakes of clay do not include them. `get_scene`'s budget
  and `kitbash action list` count the triangles drawn per group.
- **Groups**: every call adds a group (`group` names it); every object it
  adds (and every seam it engraves into clay, as blobs named
  `<group>/seamN`) carries the tag, so `kitbash {action: "remove", group}`
  takes it all out, and library sources nobody uses go with it.
- **Parts follow their surface**: a group remembers the object its parts
  sit on (`follows` in replies). Moving, aligning or re-posing that object
  (`update_object`, `pose_bone`) takes the parts along, and `mirror_object`
  mirrors them onto the copy (and onto the other side's bones). Moving a
  group itself keeps it where it was put, relative to its object. Scenes
  made before this find the object each group sits on when mirrored.
- **Parts on parts**: a target may be a kitbash group or its copies
  (`target: "upper_panels_chest"`): bolts on a plate, a latch on a hatch.
  A detail pass over parts keeps clear of the other parts, not of its
  targets.
- **Two-bone hoses, cables and pistons**: when the two ends of a piston,
  cable, hose, corrugated tube, pipe or strap land on different bones of
  one rig (drum on the chest -> a gun in the hand, thigh -> shin), each end
  rides its own bone (`bones: [first, last]` picks them, `two_bone: false`
  keeps one). Every pose solves the part again: points between the ends
  blend from one bone to the other along the length, hoses hang and bend
  anew, pistons keep their cylinder and rod lengths and slide. The
  viewport, film and `render_animation` show the solved
  part; glTF export writes a hose as a mesh skinned to both joints
  (weights blended along its length) and a piston as two rigid nodes, the
  cylinder under the first joint and the rod under the second, each
  animated to aim at the other. `kitbash {action: "span", target: group or
  part, bones}` turns existing parts into two-bone parts (bones default to
  the nearest bone to each end); `pose_bone` moves their rest ends with
  the bones. FBX writes them at rest on the first bone.
- **Greebles** take turns by kind (weighted): each kind gets its share
  before another goes again, and a kind that keeps failing gives up so the
  others still place. Sizes shrink to the room left on a face (after a
  panel pass, the gaps between plates) down to half the smallest size.
  Replies list each kind with `placed` and why copies were skipped
  (`no_room`, `crowded`, `curved`), and `shrunk_to_fit`.
- `path` placements take `orient: "path"`: each copy's X follows the
  path.
- Limits: faces that are mostly stencil or already detailed get little;
  a two-bone part's middle blends between its bones (it does not collide
  with the body).

Reproduce the pictures in `kitbash_check/` (the mech's scene is not in
the repository: pass a copy of it):

```
cargo test --release kitbash_catalog -- --ignored --nocapture
CLIDE3D_MCP_PORT=7891 cargo run --release &   # the editor, for the mech
python3 kitbash_check/build_mech.py before /abs/copy/mech.json
python3 kitbash_check/build_mech.py after  /abs/copy2/mech.json
cargo test --release kitbash_before_after -- --ignored --nocapture
```

### Mirror, align and pose

Symmetric parts and posed props without hand-made matrices:

- `mirror_object {target, plane, linked, replace, name}` makes a real
  mirror copy across a plane (default world x = 0; `plane: "x" | "y" |
  "z"` with `at`, or `{normal, point}`). Its geometry is reflected: clay
  blobs flip (a blob's `flip` reads it mirrored), sculpt offsets, paint,
  mask channels and decals mirror; symmetric primitives and eyes just turn,
  so the copy keeps a positive matrix. Kit pieces, hair and terrain (whose
  geometry cannot be reflected itself) mirror through their matrix and are
  listed in `mirrored_by_negative_scale`. The copy keeps its own pivot (the
  reflected one); `.L`/`.R` (and `_L`/`_R`, `Left`/`Right`, also inside a
  name) swap in its name, the bone it rides, blob bones and rig bones and
  clips; materials and colors carry over. Children and the kitbash parts on
  the object (and on its children) are mirrored with it onto the other
  side's bones; stencil letters stay readable. Sources with negative or
  non-uniform scale work: a part mirrored once by negative scale comes
  back with a normal matrix and reflected geometry.
- `linked: true` keeps the copy the reflection of its source: later blob
  edits, sculpt, paint, materials, moves, `align` and new detail passes on
  the source show on the copy, until `mirror_object {target: copy, action:
  "unlink"}`. The plane is kept in the source's root space (the rig), so
  moving the whole mech keeps both halves. `replace: true` first deletes an
  object with the copy's name (and the parts on it): redo the right leg
  from the left one in one call.
- `update_object align {x_axis, y_axis, z_axis, forward, up}`: two world
  directions for two of the object's axes (`forward` is +Z, `up` +Y); the
  first given (or `primary`) exactly, the second as close as it can. It
  turns about the pivot and keeps the scale; a negative axis is handled.
- `update_object frame {parent, offset, rotate, roll, at, point, attach}`
  puts an object in the frame of another object (its pivot and axes) or a
  bone. A bone's frame, at rest: origin `at` of the way from head to tail,
  +Y along the bone, +Z toward the rig's front as far as the bone allows,
  +X = Y x Z. `roll` turns about that Y, then `rotate`; `point` (a local
  point or a blob name) lands on the origin instead of the pivot; `attach`
  hangs the object on the bone.
- `update_object grip {bone, point, roll, rotate, offset, at}`: frame with
  `attach`, `at` 0.5: snap a prop so its handle blob sits in a hand bone
  with a given roll. Replies give `world_axes_at_rest`.
- `pose_bone {bone, rotate | translate | head | tail | aim, pivot, mirror,
  carry, blobs}`: pose in place. It moves a bone's rest pose (and the bones
  under it) and carries what rides them: props and armor on those bones,
  the rig clay's blobs assigned to them (plus `blobs` by name), weight
  strokes, kitbash groups, and the ends of two-bone parts on those bones.
  Clips keep their keys. `mirror: true` does the mirrored move on the
  other side. Sculpt and paint grids of the rig clay stay put (noted).
- Mirrored blobs pose per side. A blob with `mirror: true` stores one side
  and reflects it across x = 0, so a move that is not its own mirror (a
  head tilt `rotate: [-4, 0, 9]`, or `upper_arm.L` alone) cannot move it
  as one blob: `pose_bone` first writes it out as its two sides (the blob
  keeps its name, bone and place in the list; its copy, `.L`/`.R` swapped
  in the name and the bone, comes right after it, `mirror` off, `flip`
  on), then moves each side by its own bone, the `.R` side by the `.R`
  bone when the rig has one. Symmetric moves (`mirror: true` on a side
  bone, a nod or lift on the centre line) keep the blob one blob. The
  reply lists `split_blobs: [{from, into: [kept, copy], why}]`. Masks
  (`by_blobs`), weight strokes (by bone) and `blobs` names still mean the
  side they named; one undo gives the single mirrored blob back.
  `edit_clay {split: ["lid.L"]}` does the same split by hand (then
  `update` one side only: one eye more closed, one brow raised).
- `approximate_blobs: [{name, why}]` lists what a turn can only follow
  approximately: extrudes and polygon-profile sweeps, whose profile roll
  is set by the path, not stored. Round tubes (`profile: {circle}`,
  `field: segments`), capsules, spheres and ellipsoids move exactly.

Reproduce `pose_check/` (the mech's scene is not in the repository: pass a
copy; the copy is never overwritten, the result is saved next to it):

```
CLIDE3D_MCP_PORT=7892 cargo run --release &
python3 pose_check/build_pose.py /abs/copy/mech.json
```

It keys a knee brace into `fire`, renders 8 frames of it as the scene was
(`fire_before.png`, `legs_before.png`, `arm_before.png`: the one-bone ammo
hose leaves the cannon, the piston rods leave the shins), then places the
ammo hose again (chest -> left hand: two-bone), makes the leg pistons
two-bone, aims the left forearm down and hangs the autocannon straight
down with `align` (`pose_bone` carries the arm armor, hand, cannon, their
parts and the hose's end), swings the right arm out, rebuilds the right
thigh, shin and track with linked `mirror_object` (replacing the old
negative-scale copies and their parts), and renders the same views after
(`*_after.png`) and a film frame at the brace (`film_fire_frame4.png`).

### Simulated fire and smoke

`fire` creates a saved `Fire` object with a deterministic voxel cache. It runs
as an atomic background job: `job_status` reports progress or cancels, and
cancellation leaves the scene unchanged. No extra dependencies are required.

```json
{"preset":"torch","name":"TorchFire","parent":"torch",
 "translation":[0,0.255,0.175],
 "settings":{"resolution":24,"frames":24,"fps":24,"seed":17}}
```

Presets: `torch`, `candle`, `campfire`, `brazier`, `bonfire`,
`burning_debris`, `smoke_column`, `embers`. `target` re-simulates an existing
Fire; a supplied preset resets its settings before overrides. `settings`
accepts `radius` (emitter meters), `fuel`, `smoke`, `intensity`, `light_gain`, `temperature`
(artist Kelvin multiplier), `wind` (world-like local velocity), `seed`,
`resolution` (12–128 across), `warmup` (seconds), `fps`, `start`, `frames`
(1–96), and `points` (local sphere centers). New fires use a tight emitter-local box spanning ±2.5 radii horizontally
and −0.5 to 9.5 radii vertically, expanding to fit custom points. The box
follows the object's transform; `tight_domain:false` retains the legacy
±4 / −0.5..15.5 box (also used for old saved scenes). The f32 cache is
limited to 256 MiB: 64 cells supports 32 frames, 96 supports nine, and
128 supports four. Working grids are bounded by the 128-cell cap. `emitter` samples up
to 256 mesh vertices, parents the fire to that mesh, and measures the points
relative to `translation`; only points within 3 radii are accepted. Choose
radius/translation to enclose the emitting surface. This is a sampled surface
emitter; the fluid does not collide with scene meshes.

The default grid is 32 × 64 × 32. Campfire, brazier, bonfire and debris
presets use three emitter lobes; supply `points:[]` for a single source.
The `log` kit piece supplies charred wood with an independent emissive crack
texture (`lit:false` disables the glow). Scale and rotate unit cylinders
under campfire emitters and add a bed of small glowing coals.

Hero close-ups use 64–128 cells; cheap presets still default to 32. Independent
solver slabs use up to eight scoped standard-library workers at 48 cells and
above, with deterministic results. Negligible sources and empty fluid cells
skip combustion/noise work. The solver uses fixed 120 Hz steps, trilinear semi-Lagrangian advection with limited MacCormack scalar correction,
thermal buoyancy, deterministic turbulence, curl confinement, and ten Jacobi
pressure iterations. Fuel burns into heat, smoke and soot; heat cools and
smoke dissipates. Film CPU and GPU apply the same two-octave advected curl
lookup displacement, driven by local thermal/density energy and increasing
with height. A filled luminous root blends into reaction sheets at the tips;
height-dependent fuel burnout and cooling shorten the flame. Temperature/height color falloff
and soot absorption produce white-yellow bases, orange bodies and red tips.
Use ACES highlight compression and restrained exposure/bloom for close-ups. Candle
sources reduce turbulence and add a blue base. See `fire_check/README.md`
for the 512-pixel hero sequence and before/after pictures.
This is a bounded visual effects model, with open grid
boundaries and a compact combustion approximation. Semi-Lagrangian advection
is dissipative and does not conserve mass exactly. Increase resolution for
small tongues. `playback frame` chooses the nearest preceding cached frame,
clamping outside the range; simulation fps determines cache timing.

Film CPU and GPU march the same blackbody emission/smoke coefficient grids,
with Beer–Lambert absorption, Henyey–Greenstein scattering (g = 0.25), and
smoke attenuation on light/shadow rays. Four emission-weighted spherical
lights approximate each fire's illumination; their power and placement come
from that frame's hot voxels. `light_gain` is an artist multiplier for this
light estimate; preset gains are calibrated for the bright flame root so
the room can read without washing out nearby surfaces. Set it to 1 for the
unscaled emission integral. Environment and existing `fog` remain active.
This is single scattering with deterministic quadrature, not a multiple
scattering combustion reference renderer. Overlapping volumes sum their
coefficients. The viewport uses inexpensive textured cross cards, with an
alpha cutout, to show the cached flame and embers.

To replace a kit's solid flame, create/update it with `lit:false` and
`light:false`, then add the fire above. For a brazier use local
`translation:[0,1.22,0]`; for a single candle use `[0,0.2,0]`. Multiple candle
wicks can use separate small Fire objects. Existing kit scenes keep their
original flame mesh until converted.

`export_glb` replaces Fire with four triangles in cross cards and embeds a
64 px per tile, two-view, up-to-16-frame color+alpha atlas, also used as its
emissive texture. The material uses core glTF `BLEND`. The node's
`extras.maya_fire` records atlas size, tile size, sampled frames, playback
fps, duration and UV step. Columns are frames, rows are views, image origin
is top left. UVs start on frame zero; the game must advance U by `uv_step[0]`
and stop at the final frame, or implement a crossfade loop. glTF has no core
UV animation, so an engine script/shader must consume these extras. Color
and emission share the atlas (emissiveStrength = 3); the preview cards use
an alpha cutout. FBX currently exports the preview frame.

Run the visual suite and inspect its saved images, atlases, editable scenes
and GLBs in `fire_check/`:

```sh
cargo test --release fire_film_checks -- --ignored --nocapture
cargo test --release gpu_matches_cpu_fire_smoke -- --nocapture
```

The fluid method follows [Fedkiw, Stam and Jensen's Visual Simulation of
Smoke](https://www-graphics.stanford.edu/~henrik/papers/smoke/).

### Battle damage and vehicle wear

`damage` combines geometry and material in one call, on Clay, Kit pieces or
primitives. `wear` accepts the same preset/amount controls. Old scenes have no
new wear unless it is applied. Keep the existing paint and stencils, add a
preset, then place a handful of hits:

```json
{"target":"Torso","preset":"battle_worn","amount":0.65,"stamps":[
  {"kind":"bullet_hit","point":[0.2,5.4,1.2],"normal":[0,0,1],"caliber_mm":7.62,"seed":12},
  {"kind":"blast","point":[-0.7,5.6,1.2],"size":0.35,"seed":4},
  {"kind":"dent","point":[0.8,5.2,1.2],"size":0.2},
  {"kind":"gouge","points":[[-0.2,5.1,1.2],[0.1,5.3,1.2]],"size":0.035},
  {"kind":"rust_streak","point":[0.5,5.8,1.2],"length":0.3,"seed":9}
]}
```

Points default to world coordinates in the rest pose; use `space:"local"`
for object coordinates. They snap onto surface triangles; supply an outward
`normal` when stamping near a corner. All sizes/depths are meters: `size` is
the full halo width. A bullet's default halo is four times its `caliber_mm`
(5.56–25). `points` places multiple dabs, or a continuous path for
`gouge`/`scrape`. `count` with `region:{center:[x,y,z],radii:[x,y,z]}` samples
surface area deterministically using `seed`. A region must contain triangle
centers; keep it larger than a mesh cell.

- `bullet_hit`: recessed crater, petal lip, bright steel, burnt ring and
  ragged chipped paint; `dent`: smooth bowl with chips where it bent.
- `scorch`: soft soot without droplets, char/blister relief and a restrained
  straw-to-blue metal ring; `melted_lip:true` raises a softened rim. `blast`
  adds a crater and shrapnel pits.
- `gouge`/`scrape`: grooves, curled lips and fine parallel metal scratches.
- `rust_streak`, `oil_leak`, `water_stain`: project gravity onto the surface,
  follow bumps and fade downhill in world space. Oil is dark and glossy.
  Start on a vertical/sloping face; a level face cannot drain without a slope.
- `bend`/`tear`: supply `hinge:[[x,y,z],[x,y,z]]`, `angle` (-85–85 degrees)
  and `size` for a folded plate region. The selected side is normal cross
  hinge direction. Reverse the hinge endpoints to select the other side.
  Tear cuts a jagged groove at the seam; sufficient `depth` opens a thin plate.

Presets preserve base paint: `battle_worn` adds noise-shaped light chips,
primer, bare metal, cavity grime and handling polish; `desert_dusty` adds
settled dust and sun fade; `muddy` builds mud/splatter near world ground;
`burnt_wreck` adds char and exposed edges; `factory_new` removes these added
wear layers. `amount` is 0–1. Stack battle wear and dust in two calls, using
`targets` to apply a preset to many parts. Repeating a preset replaces its
amount; `clear:true` removes saved damage stamps while retaining older decals.
A material supports 16 layers and an object 64 decals/damage path segments.

New directional and height masks can set `world:true`; legacy masks stay
local. World coefficients are compiled at mesh build, including rotated,
mirrored and bone-attached rigid parts, and used by clay bakes. This avoids
covering a rotated gun in ground mud because its local Y is near zero.
`paint` strokes also accept `mask:{blobs:["armor"],above:0,below:2}`. Single
point dabs snap to the surface by default (`snap:false` opts out). Paint a
mask with `channel:"oil"`, then use a smart layer's painted mask to set
`roughness`, `metallic` and color together.

Film CPU/GPU use the same damage profiles. Geometry, color, normal and ORM
reach clay bakes and textured glTF exports. Unbaked damage exports quantize
per-triangle roughness to 1/8 and metallic to 1/4, because core glTF has no
per-vertex PBR values. Bake hero assets for exact maps. Stamps locally refine
clay and kit triangles to resolve small craters in one call, with a budget
of 200,000 added triangles per mesh. Excessive batches can need a finer
base mesh; the object’s overall resolution stays unchanged. See [closeup_check](../closeup_check/README.md) for close-up material controls and
photographic inspection, and [damage_check](../damage_check/README.md)
for reproducible panel and mech comparisons.

### Photographic film camera and looks

`render_film {"look":"trailer"}` sets a 45 mm f/4 camera, warm sunlight and backlight,
cool fill and shadows, a clouded HDR sky, desaturated AgX grading,
heterogeneous dust, subtle glare, vignetting, grain and sharpening. Other presets: `modern_warfare` (same
as trailer), `desert_heat`, `night_ops`, `cinematic_teal_orange`, `neutral`
(the original rendering defaults). Sky fill can be adjusted independently of
the backlight with `environment:{preset:"golden_hour",intensity:0.25,rotate:0}`
(or any existing sky preset). `storm` is a procedural clouded sky whose
HDR pixels also supply matching diffuse illumination. Rotation turns both
the map and its sun. Explicit options override preset fields recursively, including `fog:false`, `bloom:false`, `grain:0`, or
`camera:{aperture:null}` for a pinhole. Materials and scene files stay unchanged.

```json
{
  "look":"trailer", "aspect":1.7778, "samples":256,
  "camera":{
    "focal_length_mm":85, "sensor_size_mm":[36,24],
    "position":[4,2,8], "target":[0,1.5,0],
    "dof":{"f_stop":2.8,"focus_object":"Cockpit","blades":7,"blade_rotation":15},
    "distortion":-0.02,"chromatic_aberration":0.001
  },
  "fog":{"density":0.025,"height":0.5,"falloff":3,"noise":0.7,"noise_scale":0.35,"seed":17},
  "bloom":{"strength":0.08,"threshold":1.5,"streaks":0.15,"dirt":0.12},
  "white_balance":6000,
  "grade":{"look":"modern_warfare","saturation":0.75,"contrast":1.05},
  "grain":0.01,"grain_seed":42,"sharpen":0.12,"vignette":0.2
}
```

Camera distances use meters. `focal_length_mm` and sensor height determine
vertical FOV; `fov` overrides that projection. `sensor_size_mm:[width,height]`
defaults to full frame [36,24] when specified; its ratio supplies the output
aspect unless `aspect` overrides it. With no sensor option, the original
square output and 24 mm sensor height remain. `sensor_height_mm` is an
alternative whose width follows output aspect. A focal length/FOV change
dollies to preserve framing unless `position` is supplied. Aperture is
`camera.aperture` or `camera.dof.f_stop` (0.7..64); the sensor and focal
length set the physical entrance-pupil diameter. Focus uses exactly one of
`focus_distance` (distance along the view axis), `focus_point` (world point),
or `focus_object` (name/id, center of evaluated mesh bounds), defaulting to
the framed target. DOF needs a perspective view. `blades:0` uses a round
aperture; 3..16 uses uniformly sampled polygonal bokeh.

Exposure can use `camera.ev` (EV100, 12 = multiplier 1, 11 = twice as
bright), or `camera.iso` plus `camera.shutter_seconds`. The physical
multiplier is `4096 * (ISO/100) * shutter_seconds / f_stop²`, with ISO 100,
1/60 second and f/8 as omitted values. These controls opt into physical
exposure; ordinary aperture changes retain the current brightness.
The existing top-level `exposure` multiplies either result. `white_balance`
is the source temperature to neutralize (2000..12000 K), or RGB gains.

For animation, pass `frame` and `motion_blur:true` (eight evenly stratified
shutter times over ±0.25 frame), or
`motion_blur:{open:-0.5,close:0.5,steps:12}`. Offsets are animation frames;
`close` must exceed `open`, with 2..16 steps. Evaluated rigs, skinned meshes,
parented objects, carried lights and fire caches follow each time. Optional
`camera_end_position` and `camera_end_target` interpolate a camera move
from the configured camera at shutter open to these world points at close.
Framing remains fixed. Samples and time budget are split between strata
(at least one sample each); increase steps for fast motion to avoid
strobing. HDR radiance is averaged before final post. This is bounded
shutter quadrature, so it costs additional scene builds rather than using
a continuous-time BVH. `shutter_seconds` controls exposure independently.

`fog` retains its original homogeneous and height-falloff behavior when
`noise:0`. Noise 0..1 adds smooth, seeded 3D density variation; `noise_scale`
is inverse meters, `seed` an integer 0..16777215. Extinction affects camera,
light and indirect paths; single scattering uses light visibility for sun
and local-light shafts and aerial perspective. Fire and smoke volumes
keep their own emission and extinction and compose with the fog. Scattering
adds no light in an unlit volume. Fog transmittance uses the original exact
height integral at noise 0, otherwise matching 24..96-step CPU/GPU midpoint
quadrature. Avoid very fine noise over long paths, which this bounded
quadrature can undersample. Fog is single scattering; dense smoke is better
represented by the existing bounded fire `smoke_column` volumes.

CPU and GPU use the same HDR post pipeline: guided à-trous denoising
(`denoise:true`, default), exposure and white balance,
radial lens distortion/chromatic separation, energy-conserving bloom/glare, vignette,
`tonemap` (`aces`, `agx`, `filmic`), grade, sharpening and deterministic grain.
Glare scatters a fraction (`strength:0..1`) of energy above `threshold`
into a normalized multiscale halo, optional aperture-shaped `streaks`, and
procedural lens `dirt`; it removes that energy from the core, including at
image boundaries. It approximates lens scattering and aperture diffraction
with normalized kernels rather than simulating wave optics. Tone and
`grade` operate before display encoding. `grade.look` selects only the color
look, without camera or atmosphere changes; its optional RGB `lift`
(-0.5..0.5), `gamma` (0.1..5), `gain` (0..5), `saturation` (0..2), and
`contrast` (0.1..3, around 18% grey) refine it. Contrast uses joined power
curves that preserve black, white and smooth shadow/highlight gradients; it
does not subtract a fixed amount of shadow light. `grain` (0..0.15, subtle
0.008..0.015) and `grain_seed` give reproducible film texture. `sharpen`
(0..1) restores local detail after denoising. For sky fog, denoising uses
view direction and medium distance guides; silhouettes retain surface guides.
The filter uses reflected image boundaries and a fixed local luminance-noise
estimate so dark sampling outliers can blend with their lit neighbours.

See [look_check](../look_check/README.md) for reproducible before/after mech
shots, telephoto cockpit DOF, night operations, and a grading contact sheet.

### Compact replies, shared atlases and material maintenance

Tool text replies default to about 8 KB. `get_scene` and `open_scene`
return counts and top-level objects; `get_scene {all:true, offset:0,
limit:24}` pages through children. Use `parent` (id/name or null), `target`,
or a name `filter` glob. `detail:true` / `verbose:true` returns full
properties. `get_clay` defaults to blob names and materials and accepts
`offset` / `limit`; request detail for complete blob definitions.

`set_material {list:true}` lists names and their object/blob/part users,
with `filter`, `offset`, and `limit`. `unused:true` finds unused scene
materials; `delete_unused:true` deletes them (optionally filtered).
`{replace:"damage_*", with:"tan_paint"}` merges matching assignments and
removes the old definitions. `{name:"tan_paint", rename:"sand_paint"}`
renames one material and its assignments; use replace when the destination
already exists. Material/color edits refresh shading on existing clay
geometry and preserve skin weights. Unrelated materials leave cached
meshes alone. Geometry-dependent tool calls wait for the refreshed shading;
material edit replies are immediate.

`export_glb` packs baked parts into shared color/normal/ORM atlases by
default. Objects, pivots, skinning, animations, and LODs remain separate.
`texel_density:512` targets 512 pixels per world meter; packing reduces the
target when needed to fit at most two 4096² texture sets (six images,
96 Mi pixels). Tiles have four-pixel gutters; normal maps are renormalized
after resizing. `atlas:false` retains each part's original maps. Alpha
cards and tiled UVs retain their original textures. The density calculation
uses the mean axis scale for nonuniform transforms. The export checker
shares decoded atlases across materials.

`job_status` retains results for 24 hours, including after connection
timeouts, repeated reads, and later jobs. Pending jobs report running,
queued, or awaiting-mesh states; listings accept `offset` / `limit`.
Linked mirror targets inherit kitbash/detail passes once: explicit mirror
placement skips sources that already have linked copies. Flexible kitbash
spans route outside target surfaces and sag between authored points
(`route:false` keeps the authored route). Pistons report intersections
because their rods must stay straight. `kitbash {action:"reattach", target,
from:[...], to:[...], bones:["chest","hand.L"]}` reattaches an existing
span without replacing its part settings; omitted bone names are inferred.

Reproduce timings, atlas export checks, and the per-part/atlas render
comparison in [speed_check](../speed_check/README.md).
