# Photographic film acceptance pictures

The copied source `mech.json` is unchanged (SHA-256
`95b8c2b51b50d07454ae40c42039fea3c7e3f64cbba3c9a4f727b06c9ae197af`).
Fine geometry, materials, decals, terrain and scattered rocks are preserved.
Large scene copies and the native geometry cache are local and git-ignored.

| Shot | Control | Finished |
| --- | --- | --- |
| Low three-quarter | [Pinhole daylight](trailer_before.png) | [Trailer](trailer_after.png) |
| 200 mm cockpit/shoulder | [Same lighting, pinhole](telephoto_before.png) | [f/2.8, focus on LS-M1](telephoto_after.png) |
| Night operations | [Moonlight](night_ops_before.png) | [Night grade and local light](night_ops_after.png) |
| Low backlight | [Same lighting without dust](dust_shafts_control.png) | [Shadowed light shafts](dust_shafts.png) |

[Labelled contact sheet](before_after_contact_sheet.png) contains these four
pairs. [Grading contact sheet](grading_contact_sheet.png) finishes the same
traced HDR buffer with neutral, modern_warfare, desert_heat, night_ops and
cinematic_teal_orange looks.

The accepted round-one images are retained byte-for-byte as
[trailer_round1.png](trailer_round1.png) and
[night_ops_round1.png](night_ops_round1.png). Their compositions and lighting
recipes remain in the current set, re-rendered through the corrected post.

## Round-two corrections

The original sensor macro had three contributing problems:

- Linear affine contrast at 1.16 subtracted 0.0288, crushing a whole range
  of positive shadow values to identical black. Joined power curves now
  preserve black, white and the 18% grey pivot, with continuous slopes.
- The denoiser's brightness tolerance approached zero for dark sampling
  outliers. They rejected lit neighbours and survived as blotchy speckles.
  A fixed local, geometry-guided luminance-noise estimate allows blending.
- Dropping filter taps at image boundaries changed the kernel at each
  wavelet stride, introducing steps into otherwise smooth gradients.
  Reflected boundaries retain a consistent kernel.

Radiance, denoising, bloom, tone mapping and grade remain floating point;
only final display encoding converts to 8 bits. Regression tests exercise
shadow and green ramps through denoise + trailer tone/grade with grain
and sharpening disabled, dark sampling outliers, and depth discontinuities.
The original macro is retained as
[sensor_regression_round1.png](sensor_regression_round1.png), with the same
macro re-rendered as [sensor_regression_after.png](sensor_regression_after.png).
A further [surface control](sensor_surface_control.png) disables denoising,
bloom, sharpening, grain and contrast grading. The remaining concentric
edges are the model's separate lathe/carve/sphere surfaces and materials;
the bright green sphere has constant `lens_glow` emission 4. Its flat lit
interior and jagged material boundary are present without those processing
stages. Those authored surfaces/materials are preserved, rather than blurred
across their boundaries. The smooth-ramp regressions isolate the actual
processing defects from this source geometry.

The telephoto uses a real **200 mm full-frame lens, f/2.8, seven blades**,
about 26 m from the stencil at `[0.3, 6.15, 0.8]`. The camera targets the
cockpit and shoulder rather than the sensor. The soldier is staged near
camera in `telephoto_scene.json`: his root moves from `[3.3,0,1.2]` to
`[14.95,0,17.87]`, carrying his head and eyes. This puts a physically defocused
foreground silhouette into the tight shoulder crop. The source remains
untouched; open this staged scene to reproduce either telephoto image.
The pinhole control uses the same scene, lighting, fog and finishing.
The mounted missile pod is close to the stencil's focal plane, so its blur
is physically modest at this working distance; distant background and the
near soldier have much larger circles of confusion. There is no screen-space
DOF filter or artificial enlargement of the aperture.

The new dust shot uses a low sun behind/beside the mech, 0.35-degree source,
forward scattering, warm scattering albedo below one, seeded density
variation, and exponential height falloff over 3.5 m. Dust is denser near
the ground, thinner above the shoulders; warm sunlight and cool weak fill
provide depth. Shafts are the existing single-scattering transport with
geometry visibility and Beer–Lambert attenuation on camera and light paths.
The no-fog control isolates their cause. No overlay or emissive fog is used.

## Outline halo correction

The post-grade unsharp mask added its high-frequency residual without an
upper bound. A dark silhouette against constant haze therefore acquired a
bright rim above the background value, with a corresponding dark undershoot.
Sharpening now clamps each channel to the original 3×3 neighborhood's range,
using immutable input and reflected boundaries. This retains sharpening of
gradual detail while preventing new local extremes. The trailer's `sharpen`
value remains 0.12. Denoising, floating-point grading, physical DOF, fog
transport and glare are unchanged.

[Halo comparison](halo_before_after.png) shows the original unbounded post
on the left and the corrected post on the right. Both shots are re-traced
at 960×540, 512 samples with their original camera/lighting/fog recipes.
The pre-fix renders and recipes are retained as
[trailer_halo_before.png](trailer_halo_before.png) /
[trailer_halo_before.json](trailer_halo_before.json) and
[dust_shafts_halo_before.png](dust_shafts_halo_before.png) /
[dust_shafts_halo_before.json](dust_shafts_halo_before.json).
The current `trailer_after` and `dust_shafts` files contain the corrected post.

The silhouette regression runs hard and softened dark circular objects
against bright haze through denoise and the trailer finishing pipeline,
at both preset and maximum sharpening. With grain and vignette disabled to
isolate ringing, every output channel must stay between the finished dark
and haze controls. It fails on the old code even at 0.12 sharpening.
Separate checks preserve nonzero sharpening of gradual detail, constant
images and single-pixel dimensions. The round-two ramp, dark-outlier,
depth-edge and glare-energy regressions remain in place.

The native geometry cache is now version 2 and retains texture sets, imported
material values and face shading data. Its former untextured-only assertion
failed on the source scene's eye meshes during regeneration; a textured-cache
round-trip regression covers this harness fix.

## Reproduce

Each adjacent JSON records exact `render_film` options, the source scene,
resolved preset values, samples, dimensions and trace time. Open its `scene`
file before passing `render_film` to MCP. For 16:9 output, use the stored
`aspect`; the full-frame gate height used by the lens model is 24 mm.

```sh
SCENE="$PWD/look_check/mech.json" LOOK_SIZE=960 LOOK_SAMPLES=512 \
  cargo test --release film_look_check -- --ignored --nocapture
cargo test --release film_look_contact_sheets -- --ignored --nocapture
```

`LOOK_CASE=telephoto_before,telephoto_after` restricts shots; `LOOK_OUTPUT`
selects a folder. `LOOK_RECIPES` accepts a JSON map from shot names to complete
render options for camera/lighting experiments. The harness generates the
staged telephoto scene automatically. To repeat the sensor regression, map
`telephoto_after` to the `render_film` options in `sensor_regression_after.json`
and rename the resulting image/metadata before regenerating the normal set.

A Metal adapter is required for fast regeneration. The first fine geometry
build can take nine minutes; the source-fingerprinted `geometry.cache`
then reuses exact native vertices, shader materials and decals. Delete that
cache after meshing or vertex-layout changes. Fog uses bounded quadrature
and single scattering; lens glare uses normalized kernels. These remain
approximations, not a multiple-scattering or wave-optics reference renderer.

Validation: `cargo build --release`, `cargo test --release` (245 passed,
29 ignored visual/benchmark harnesses), and
`cargo clippy --release --all-targets -- -D warnings` pass. Metal parity
checks executed for materials, fire/smoke, displacement, polygonal DOF,
heterogeneous fog and environment lighting. Existing tests also verify
unlit/occluded fog adds no light and glare preserves energy at image edges.
