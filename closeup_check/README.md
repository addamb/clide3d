# Close-up paint and damage, round 3

## Round 3: broken, millimetre-scale edge wear

The wide dashes came from the **mask composition**, not a low-resolution
texture. The 180 mm handling field was combined with `Max`, overriding the
25 mm cluster and 8 mm fine noise. Its high values filled the entire
curvature-positive bevel. A final contrast of 24 clipped the interpolated
curvature into a straight, constant-width boundary. The sharp-clay curvature
already comes from the continuous SDF; replacing face normals or increasing
render resolution would not fix that composition.

Every flake boundary now follows 4 mm, three-octave noise. A 110 mm handling
field only modulates density; it cannot replace that noise. Higher coverage
thresholds leave gaps, while lower final contrast softens the submillimetre
transition. The existing curvature gate keeps flat panels clear. World-up
exposure favors upper edges, and inverse occlusion suppresses sheltered edges.
The nested layers are a lightly rubbed version of the authored paint, dull
red-brown primer, then smaller dark neutral steel spots with metallic response
and 0.42 roughness. Uniform object scaling compensates the mask frequencies,
so enlarging a model does not enlarge the flakes. Nonuniform scaling uses the
same mean-axis approximation as the existing paint relief.

The regression `closeup_bevel_wear_width_and_ragged_boundaries` samples the
continuous surface of a 1 m box with a 40 mm rounded bevel at 0.5 mm intervals.
It evaluates the production curvature and production per-hit masks. Width is
surface arc length. It checks equivalent covered width, maximum connected
steel width, variation across cross-sections, gaps along the bevel apex,
primer containment, occlusion suppression and absence of flat-panel chips.
The final measurements are recorded in `edge_wear_metrics.json`:

| Wear amount | Mean equivalent steel width | Largest connected steel width | Width standard deviation | Apex gaps |
| --- | --- | --- | --- | --- |
| 0.65 (mech) | 0.94 mm | 6.50 mm | 1.76 mm | 96.3% |
| 1.00 | 2.60 mm | 8.00 mm | 3.13 mm | 90.6% |

These are coverage measurements on that test bevel, not a promise that every
possible geometry/noise realization has the same maximum. A separate regression
checks millimetre mask scale when an asset is enlarged 6.5 times. Existing tests
continue to check that the actual missile-pod flat face stays clear.

```sh
CLOSEUP_RECORD_METRICS=1 cargo test --release closeup_bevel -- --nocapture
CLOSEUP_SAMPLES=192 cargo test --release closeup_paint_sheet -- --ignored --nocapture
CLOSEUP_SAMPLES=192 CLOSEUP_SCENE=/absolute/path/to/mech3/mech.json \
  cargo test --release closeup_mech -- --ignored --nocapture
```

The round-three outputs replace `mech_after_closeup.png`, `mech_after_detail.png`
and `paint_sheet.png`, with matching per-render recipes and individual paint
panels. The shoulder detail camera is also the mid-distance acceptance view.
The source scene and round-two damage shader are retained.

### Final inspection and validation

The final [torso close-up](mech_after_closeup.png),
[shoulder / mid-distance detail](mech_after_detail.png) and
[paint sheet](paint_sheet.png) were inspected at full resolution. The long,
flat dark dashes have disappeared from the missile doors, shoulder rims and
chest plate. At the shoulder camera distance the warm tan panels dominate;
the wear reads as fine chips, with occasional short clusters and clean gaps,
without the earlier camouflage pattern. At the closest distance the wear
still has a procedural stippled quality. Exposure is inferred from curvature,
world-up and occlusion, rather than semantic hand/foot/tool contact history;
this change does not claim photographic realism for the entire asset.

Release build and Clippy (`--all-targets`) pass without warnings. The final
release suite passes **291 tests**, with 38 opt-in checks ignored. The separate
CPU/Metal smart-material comparison passes for all ten finishes/configurations;
battle-worn paint differs by **0.71/255** per channel on average, and the combined
battle-worn/desert-dust stack by **0.70/255**. The ten paint panels and four matched
mech views were rerendered at **192 samples**. The damage sheet and damage shader
are unchanged from round two. Camera/lighting recipes match between each before
and after pair. The original scene SHA-256 remains
`1f70224c34c979b33bc0215ce42f1b74a01754c711516509622b3330793e0c7e`.

## Round 2 background and retained work

All pictures are native Clide3D film renders. No generated imagery, external
renderer, new crate, or downloaded texture is used.

```sh
CLOSEUP_SAMPLES=192 cargo test --release closeup_sheets -- --ignored --nocapture
CLOSEUP_SAMPLES=192 CLOSEUP_SCENE=/absolute/path/to/mech3/mech.json \
  cargo test --release closeup_mech -- --ignored --nocapture
```

`damage_sheet.png` includes all eleven stamp types plus an oblique projectile.
`paint_sheet.png` includes all ten vehicle finishes. Each panel uses a 50 mm
close-up and raking light; the pale stripe tests retention of sparse paint.
Each image has an adjacent JSON recipe with camera, lighting, samples and time.

`mech_before.json` is an unmodified source copy; `mech_after.json` is the revised
scene. Both scene files are ignored by Git. The original scene is never written.
Before and after images now use identical reconstructed cameras and lighting:
50 mm f/2.8 on the torso, and 70 mm f/4 on the shoulder. The original film camera
recipes were unavailable, so these views match the round-one reconstructions,
not the original film images exactly. The before views use the original scene's
material stacks with the current renderer; they are fresh renders, not copies
of the original pictures. The off-camera soldier remains in the saved scenes
but is omitted from render-only meshes.

## Changes and causes

- **Soot resolution:** film evaluates procedural damage at every hit point on
  both CPU and GPU, without a decal mask texture or nearest-neighbour sampler.
  The former opacity included a 49-cell square value-noise field: the saved
  torso's 0.682 m burn exposed roughly **14 mm cells**. Radial angular jitter
  also made the opaque burnt area a petalled blot. Soot now uses a continuous
  exponential cross-section with a soft uphill fade, widening with airflow.
  Fine cells affect only blister relief/ash inside a smaller burnt center.
  Increasing mesh, detail-region or bake resolution cannot fix that old field.
- **Authored color:** vehicle finish presets no longer supply an implicit
  `base_color`, and resolving a named library material no longer backfills a
  preset color. They retain authored blob/object colors. An explicitly supplied
  `base_color` still requests recoloring; `keep_paint` defaults to true and
  preserves painted markings above that substrate. `keep_paint:false` removes
  sparse paint without changing the substrate choice. The mech recipe rebuilds
  both legacy `tan_paint` and `armor_khaki` finish stacks without the old opaque
  grey-khaki layer, exposing the original warm tan blob colors.
- **Panel finish:** broad tone variation, soft handling smudges and directional
  wipe masks affect color and roughness. Fine 4 mm paint tiles have very low
  normal strength and zero color grain. Battle wear adds broader ageing,
  cavity grime and a stronger part-relative lower-edge gradient; desert dust
  collects on upward faces and in cavities. Relief stays faint in highlights.
- **Grooves:** a continuous metal center has slowly varying width, chipped
  paint/primer on both sides and dark burr edges. Lower relief and normal
  strength prevent over-steep normals from alternating black and chalk-white.
  Fine parallel scoring is subordinate to the main cut.
- **Oil:** a narrow source widens downhill, with brown translucent coverage,
  glossy wet film, a rough dusty fringe and a terminal drop. All segments use
  global path progress, so width and opacity do not restart at every segment.
- **Contact wear:** broad handling clusters can form coherent worn-through
  patches with a primer margin. Fine noise no longer multiplies every patch
  into isolated dots. Curvature still gates wear away from flat panel centers;
  these are geometry-driven contact candidates rather than a semantic model
  of where a crew actually stands or grips.

The mech recipe reconnects legacy damage material copies to shared bases,
rebuilds the finish and reapplies wear/dust while retaining sparse paint and
decals. Nearby offset soot stamps are re-centered on their bullet impacts in
the editable copy and evaluated before the craters, so soot does not paint over
the hole. Unrelated burns retain their placement. The source asset is unchanged.

## Verification

Regression checks cover soot opacity continuity at submillimetre intervals,
continuous bright groove coverage, downstream oil spreading, deterministic
profiles, paint roughness variation, authored color and explicit override rules,
marking preservation, relative grime, and fast/exact baking with embedded glTF
maps. Existing checks also cover displaced damage geometry, normal/ORM baking,
GLB round trips and CPU/GPU material agreement.

## Photographic comparison

The paint/dust reference is the public-domain DVIDS photograph
[an Abrams crewman cleaning a V-pack at FOB Mackenzie, Iraq, June 2, 2005](https://www.dvidshub.net/image/6732/soldier-uses-air-wand-remove-dirt-tanks-v).
The downloaded reference was inspected directly, alongside the supplied concept
`mech3/ref/front.jpg`. The real tank's skirt shows uneven warm dusty panels,
subdued broad staining and much less regular edge wear. It is a paint/dust
reference, not evidence for the appearance of ballistic holes or scorches.

The final torso render was inspected at full resolution. The former square
soot cells and petalled ink blot are gone; the soot now rises softly from the
impacts, without a separate dark ring below them. The warm desert tan is restored.
The scratch has a continuous bright core with dark sides. Contact wear forms
larger interrupted patches with visible paint/primer transitions rather than a
peppered outline. Broad ageing and smudges are subtle; the head-on matte panels
still read cleaner than a heavily weathered real vehicle.

The result remains visibly rendered. The repeated panel construction, oversized
regular crater rims and some long straight worn patches still make it less
convincing than the photographed Abrams. These are improved procedural materials,
not a claim of photographic or Call of Duty-level equivalence. Contact placement
is inferred from curvature/noise rather than authored crew-use histories.


The final damage and paint sheets and all four matched mech views were inspected.
The isolated oil panel now has a much narrower brown film and soft dusty fringe,
but its single ribbon and small terminal drip remain stylized. The paint sheet
shows retained markings and stronger broad shading on battle-worn paint; the
plain military finish remains deliberately restrained. The matched before views
show how much larger the original paint relief was.

Round-two checks: release build and Clippy (`--all-targets`) pass without warnings;
289 release tests pass (37 opt-in checks ignored). Both final CPU/Metal material
checks pass, including continuous runoff (mean pixel difference 0.46/255). Both
opt-in render checks pass: 26 individual renders and two sheets at 192 samples.
Recipe equality verifies identical before/after camera and lighting parameters.
The source scene SHA-256 remains
`1f70224c34c979b33bc0215ce42f1b74a01754c711516509622b3330793e0c7e`.
