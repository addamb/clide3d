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
undo / redo · ⌘D duplicate · ⌫ delete · ⌘S save · ⌘O open.

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

Tools: `get_scene`, `create_object`, `update_object`, `delete_object`,
`duplicate_object`, `select_object` (multi), `group_objects`, `parent_objects`,
`set_pivot`, `set_camera`, `frame_view`, `screenshot`, `set_material`, `decal`
(PNG of the viewport or whole window), `run_mel`, `set_display`, `undo`, `redo`,
`new_scene`, `open_scene`, `save_scene`, for faces `face_detail` and
`face_hair` (see Faces below), and for environments `kit`,
`scatter` and `edit_terrain` (see Environments below). Open/save only accept absolute `.json`

`new_scene`, `open_scene`, `save_scene`, for environments `kit`,
`scatter` and `edit_terrain` (see Environments below), for hard-surface
detail `kitbash` and `detail_pass` (see Kitbash below), and for symmetry and
posing `mirror_object` and `pose_bone` (see Mirror, align and pose below). Open/save only accept absolute `.json`
paths and never overwrite a file that is not a scene.

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
- `set_reference`: load the image to copy (absolute `.png`/`.jpg`). It finds
  the subject, returns its main colors (palette), and overlays it on a
  viewport pane.
- `render_views`: one contact sheet of the model from several cameras,
  rendered off screen.
- `compare_reference`: lines the model up with the image from one view and
  returns how well the outlines overlap (IoU), which regions are missing or
  extra, a pixel-to-world mapping, and reference | model | overlap images.

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
  `polished_metal`, `gloss`. `set_material` defines new ones or changes a
  preset for this scene (roughness, metallic, clearcoat, sheen, subsurface,
  pores, mottling, skin_specular, detail) and lists what each object uses. The viewport shades them (GGX highlights, a soft
  studio, a weak light near the camera for catchlights).
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
  `paint` and `sculpt` strokes take `snap: true` to do the same. These
  tools (and `render_views` / `render_film` unless given a `frame`) always
  work in the rest pose, so points stay right while a clip plays.
- Clothing: a blob with `op: "cover"` dresses everything before it: the
  surface grown by `offset`, kept only inside the blob's own shape (a torso
  box = a fitted tunic, an arm capsule = a sleeve). It fits exactly, has a
  crisp edge and its own color and material, and moves with the rig.
- `color_blend` on a blob sets how wide its color fades into earlier blobs;
  0 gives a crisp color edge (a sleeve end) while the shape blends smoothly.

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
beads left and right in turn (degrees). Paths have 2–128
points and at most 512 beads; long paths increase spacing at that limit.
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
  `"overcast"`, `"sunset"`, `"forest"`, `"golden_hour"`, and `"night"`.
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
mesh answer once it is built.

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
| `sweep` | `profile`, `points`, `taper` (1), `twist` (0 degrees total), `path_smooth` (true) | Trim, strap, ridged horn; transported section frame |
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
