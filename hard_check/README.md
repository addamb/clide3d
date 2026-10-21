# Forged armor visual checks

Comparisons show **before on the left, after on the right** (768 pixels per
view). Before uses shared field normals and surface nets; after uses exact
planar primitives or Hermite feature placement with split crease normals.
Both retain the manifold tetrahedral fallback for ambiguous field faces.
Each pair has identical forms, materials, sampling, camera and lighting.

## Overviews

- [Great helm](horned_helmet_after.png): smooth dome and curled, ridged horns,
  layered brow/cheek plates, central crest, mounting collars, visible rivets
  and engraved brass trim.
- [Fitted breastplate and pauldron](breastplate_after.png): the same human
  field and skeleton as `create_human`, a sculpted shell with center ridge,
  outward rims, raised brass ornament and overlapping shoulder plates.
  Conservative fitting covers anatomical protrusions without reproducing
  their contours. The breastplate follows `spine`; shoulder pieces follow
  `upperarm.L`.
- [Sword](longsword_after.png): a fullered section with a thin flat cutting
  edge, overlapping tapered tip, ornate guard, leather grip and lathed pommel.
- [Shield](shield_after.png): raised boss, rolled rim, 32 attached rivets,
  radial engraving and brass inlay.
- [Layered pauldron](pauldron_after.png): an open dome, overlapping lames,
  rolled trim, rivets and surface-following crest.

Steel and aged brass use metallic GGX materials, restrained cavity/dirt/edge
wear, roughness variation and `metal_scratched` detail. No material system
changes or external/generated artwork are required.

## Macro evidence

| Comparison | Inspect |
| --- | --- |
| [Crossguard corner](crossguard_corner.png) | Flat faces and a straight, sharp silhouette instead of a rounded, uneven corner |
| [Sword edge](sword_edge.png) | Thin cutting plane and distinct face normals |
| [Plate rim](plate_rim.png) | Actual wall, stepped lames and rolled trim |
| [Rivet](rivet.png) | Attached head, rounded crown and sharp flange |
| [Engraved groove](engraved_groove.png) | Constant-depth cut and defined side walls |
| [Helmet trim](helm_trim.png) | Rivets, layered cheek plate and engraved band |
| [Curled horn](curled_horn.png) | Continuous curvature and transported ridges |
| [Fitted shell](fitted_shell.png) | Broad forged contours and raised center ridge |

Mandatory feature studies use 0.1–0.3 mm cells. Sword/guard/rivet/groove
studies isolate the same local construction from distant geometry so those
cells fit the existing limits. Plate and helmet rim studies resolve windows
of the original field; window caps are outside camera framing. The human
render crops geometry outside the torso framing. Local study JSONs retain
editable source geometry; windowed studies are reproduced by the Rust check.

Overview cells are 0.8–1 mm for props and 2.5 mm for the fitted human shell.
The overview and macro images serve different purposes: a full-length sword
cannot reveal a submillimeter edge at the same scale as the guard study.

## Reproduce

```sh
cargo test --release hard_film_checks -- --ignored --nocapture
HARD_CHECK=breastplate cargo test --release hard_film_checks -- --ignored --nocapture
```

`src/hard_showcase.json` contains the recipes. `src/hard_checks.rs` projects
ornament onto the fitted surfaces and renders through the `render_film`
backend. `HARD_PHASE=after` can refresh one phase when matching cached source
JSONs/images already exist; source differences require a full pair.

[Mesh timings](TIMINGS.md) report the actual fine meshes, including macro
studies. The available sandbox has no Metal adapter, so `device:auto` used
the CPU tracer. All final views reached 64 samples. Timings include concurrent
compilation activity and are measurements, not a controlled speedup claim.

Validation includes closed/winding checks, rotated planar edges, thin blade
edges, crease normals, conservative body clearance, rigid plate attachment,
smooth spline tangents, solid lathe axes and a tightly twisted sweep's
manifold fallback. Release build, tests and clippy are run separately.
