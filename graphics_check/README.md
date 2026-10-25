# Viewport graphics checks

Before and after pictures for the real-time viewport (`src/render.rs`), drawn
headlessly through `render::render_image`, the path agent screenshots and
`render_animation` use. They are drawn on the window's kind of target
(`Bgra8Unorm`, where the shader gamma-encodes by hand).

## Stage 1: key-light shadow maps

### What changed

- The studio key light casts shadows onto every mesh. A depth-only pass
  renders a 2048² shadow map (4096² with `quality: "high"`) along the key.
  `fs_mesh` filters it with 16 rotated Vogel-disk taps, each a hardware 2x2
  comparison (PCF). `soft: true` adds a blocker search and widens the
  penumbra away from the caster (PCSS), sized by the key's angular radius.
  Only the key term is shadowed: the fill, the ring light, object lights and
  ambient are unchanged.
- The map is an orthographic view along the key. It is fitted to the
  casters' world bounds, its width is quantised to powers of √2 and its
  origin is snapped to whole texels. Orbiting never changes it and panning
  does not make edges crawl (`studio_orbit.png`). In the window, the fit is
  clamped around the persp camera's target, so a large scene keeps detail.
- Glass (transmission > 0.3) and bright emitters (emission > 1.5) cast no
  shadow, as in `render_film`. A shadow catcher on the grid plane y = 0
  shows shadows in floorless scenes. It is pushed back in depth further
  than the meshes are, so a real floor at y = 0 hides it. It turns off when
  objects reach below the grid.
- The viewport key moved from (0.4, 0.8, 0.5) to render_film's key
  (0.45, 0.75, 0.55). This changes highlights and the reflection spot a
  little: about 1.4/255 mean absolute difference on the fixture. With
  shadows off and the old key, the output matches the previous renderer to
  within 1/255 (`shadows_off_matches_the_renderer_before_shadows`). On this
  machine, the `*_before.png` pictures below are byte-identical to captures
  from commit 9ed2b47.
- Window panes share one shadow map, which the first pane's `prepare`
  draws. Screenshots have their own map. Each map is redrawn only when the
  light fit, the casters (instance transforms and materials) or a mesh
  upload change. Scenes with more than 8M fill triangles
  (`SHADOW_TRIANGLE_BUDGET`) draw without shadows. `set_display` reports
  this in `shadows.last_frame`, and the Lighting menu shows it too.
- Controls: Lighting menu > Shadows / Ground Shadow / Soft Shadows (PCSS).
  The MCP `set_display` tool takes `shadows: true | false | { enabled, soft,
  quality, strength, softness, ground, key_direction }`, and its reply
  echoes the settings. Masks (silhouettes) never change.

### Pictures

| File | What |
| --- | --- |
| `fixture_before.png` / `fixture_after.png` / `fixture_before_after.png` | The shared test fixture (`render::test_support::Fixture`): floor, cube, ball, cylinder, clay capsule. |
| `studio_before.png` / `studio_after.png` / `studio_before_after.png` | A floorless agent-style shot with the grid: the catcher shows the shadows. The glass ball (left) casts none. |
| `studio_soft.png` | The studio shot with `soft: true` (PCSS). |
| `studio_orbit.png` | Four orbit positions around the same target: the shadows stay put. |
| `baseline/fixture_{srgb,linear}.png` | 128x96 fixture renders from the renderer before this stage (commit 9ed2b47), on both targets. A test compares against them; do not regenerate them from newer code. |

"Before" pictures are drawn by the new renderer with shadows off and the old
key (`graphics_checks::legacy()`). The baseline test above proves that this
reproduces the old renderer.

### Reproduce

```sh
cargo test --release graphics_sheets -- --ignored --nocapture
# OUT=dir writes elsewhere; GRAPHICS_SIZE=640x480 changes the size (480x360).
```

The automated checks (non-ignored, on both the sRGB and manual-gamma
targets) are in `src/graphics_checks.rs`, `src/render.rs` (light fit, WGSL
validation and Globals layout) and `src/app/tools.rs` (set_display parsing):

```sh
cargo test --release -- graphics_checks render::tests set_display_shadows
```

## Stage 3: HDR finishing, bloom, tonemap and exposure

### What changed

- Shaded panes draw through an HDR pipeline (`src/render/post.rs`). In
  `prepare`, on egui's encoder, the fills and the ground shadow catcher
  render linear radiance into a half-float MSAA target (pass A,
  premultiplied over nothing). The grid and wireframes render into their
  own MSAA target in the pane's format, tested against pass A's depth
  (pass B). Then light above the bloom threshold goes down a mip chain and
  back up (the bloom). In `paint`, one draw composites it all into egui's
  pass. For each sample, the scene is exposed, gives up the light that
  went into the halo and gains the halo, is tonemapped and graded, and
  then gets the grid and wires on top. The results are averaged, as the
  direct path encodes and blends each sample. So edges, the selection
  outline and grid lines are as crisp as before, in their own colours,
  never tonemapped.
- This stage owns exposure and tonemapping: `fs_mesh` only gamma-encodes
  for the direct path, and the new `fs_mesh_hdr` returns linear light.
  The tonemaps (ACES, AgX, Filmic) and the grade are ports of
  render_film's (`film::look`, `film::post`). A GPU probe checks them
  against `Look::finish` on a 1001-step HDR ramp and 64 saturated colours
  (within 1/255, `tonemaps_and_grades_match_render_film`). Exposure uses
  render_film's units: `set_display.post.exposure` is in stops, and film's
  camera `ev = 12 - exposure`.
- Bloom is render_film's `bloom: true`: strength 0.08, threshold 1 after
  exposure, and halo radii of 0.6%, 1.8% and 5% of the shorter side,
  weighted 0.5, 0.3 and 0.2 and mapped onto mip levels. The halo's size is
  therefore a share of the picture. It conserves energy: it moves light,
  never adds any (within 2% in `bloom_moves_light_without_making_any`).
  Only emitters (the glow preset, torches, lit windows, panel lights) and
  the hottest highlights pass the threshold. Over the background the halo
  adds to the light the background stands for, so background with no halo
  stays exactly the clear colour or gradient.
- The default look is ACES with bloom on. ACES rolls off highlights and
  makes mid-tones a little darker than the old clip (see
  `post_before_after.png`). `tonemap: "none"` with bloom off reproduces
  the direct path within 1/255 (`plain_hdr_matches_the_direct_path`).
- Masks (silhouettes), pure wireframe and `post.enabled: false` still draw
  straight into the pane, unchanged.
- Glass now blends premultiplied in linear light rather than in the
  target's gamma space: on the window's target this changes a few glass
  pixels by up to 10/255 (mean 0.05/255, `glass_blends_in_linear_light_through_hdr`).
- The window and `render_image` (agent screenshots, render_views,
  render_animation) share one code path. A test drives the real
  `prepare`/`paint` into an egui-like pass and compares the result with
  `render_image` (within 1/255).
- A pane partly off the window: egui clamps its callback viewport to the
  window, which used to squash the view into what was left. `prepare` now
  fits the projection to the clamped viewport (`crop`), so the visible part
  shows what the whole pane would show, in the same place. This applies to
  both the HDR and the direct paths. Vignette and the halo's background
  gradient still use the visible part's extent.
- Memory: about 76 bytes per pane pixel (scene 4x MSAA half-float, its
  resolve, 4x depth, 4x overlay, bloom chain): about 280 MB for a
  2560x1440 single pane. Targets are made when a pane's size changes, and
  given back a few frames after a pane stops drawing (leaving the four
  view) or turns HDR off.
- Cost: a 1280x720 `render_image` of the studio shot (readback included)
  took 3.3 ms direct and 5.6 ms with HDR on this machine (Apple GPU, mean
  of 10).
- Controls: Lighting > Display in the viewport menu offers HDR on or off,
  Bloom, the tonemap (ACES, AgX, Filmic, None) and an exposure slider in
  stops. The MCP `set_display` tool takes `post: true | false | {enabled,
  look, tonemap, exposure, bloom, vignette, white_balance, grade, reset}`
  with render_film's meanings, and its reply echoes `post`. A look preset's
  environment, lights, fog, lens effects, grain and sharpening stay
  render_film only.

### Pictures

| File | What |
| --- | --- |
| `post_before.png` / `post_after.png` / `post_before_after.png` | The fixture with the cube selected and the grid: direct path (HDR off) and the default HDR look. |
| `looks.png` | Top: ACES, AgX, Filmic. Bottom: None (clip), trailer, night_ops (with their grades and vignettes). |
| `glow_before.png` / `glow_after.png` / `glow_before_after.png` | A stone corner lit by emitters: two torches, a glow-preset ball, a lit window and a ceiling panel. HDR off, then on. |

Stage 1's pictures above, and its baseline test, stay on the direct path
(`post.enabled = false`), so they reproduce as before.

### Reproduce

```sh
cargo test --release hdr_sheets -- --ignored --nocapture
# OUT=dir writes elsewhere; GRAPHICS_SIZE=640x480 changes the size (480x360).
```

The automated checks run on both the sRGB and the manual-gamma targets.
They are in `src/graphics_checks.rs` (HDR against the direct path, glass,
background, masks, halo, energy, wires and grid, window against
render_image), `src/render/post.rs` (WGSL validation and uniform layout,
bloom levels, tonemap and grade parity with render_film), `src/render.rs`
(`crop`) and `src/app/tools.rs` (`set_display_post_parse_and_echo`):

```sh
cargo test --release -- graphics_checks render:: set_display
```

## Stage 2: environment lighting and ambient occlusion

### What changed

- The viewport is lit by render_film's environments and lights instead of
  its own made-up studio. The default, `lighting: "studio"`, is
  render_film's default: the `studio` environment and its key, fill and
  rim lights (`film::LIGHTS`). A viewport-only catchlight stays, and the
  studio still dims when objects carry lights, as before. The other presets
  (`overcast`, `sunset`, `golden_hour`, `forest`, `night`, `storm`, HDR
  files, look presets) are the same environment maps the tracer uses.
- `src/view_light.rs` bakes each environment once on the CPU (tens of
  milliseconds, cached by its arguments):
  - Diffuse light is SH9 irradiance (Ramamoorthi and Hanrahan), with every
    texel weighted by its exact solid angle as `Environment::map` does.
  - Reflections are an Rgba16Float equirect mip chain: level k is
    GGX-prefiltered at roughness k/5 with 64 Hammersley samples
    (filtered importance sampling), plus a 32x32 split-sum BRDF table.
  - An environment's sun disc is not baked in. It becomes a directional
    light carrying the preset's power (radiance x 2π(1 − cos size)), as
    render_film's suns are. Up to four suns are shaded directly, brightest
    first; any further suns are folded into the irradiance.
  - Rust and WGSL `Globals` grew once, to 1200 bytes, with a `size_of`
    assert. They gain SH, suns, environment and AO parameters and the
    inverse view-projection.
- Shading follows film's units: a sun's irradiance E gives
  `albedo/π · (1 − F) · E · n·l`, with the GGX highlight carrying its own
  cosine. Ambient light is `albedo/π · (1 − F) · E_SH(n)` plus the
  prefiltered environment times `(f0·A + B)`. There is no tonemap in the
  shader: Stage 3's composite owns exposure and tonemapping.
- **Ambient occlusion** is screen-space and recomputed each frame (no
  temporal state, so pictures repeat exactly):
  - A half-size pre-pass draws world normals and depth, skipping glass and
    keeping hair-card cut-outs.
  - 12 hemisphere taps are reconstructed through the inverse view-projection,
    so ortho views work. They are turned by a fixed 4x4 pattern.
  - A depth-aware 4x4 blur averages the pattern away.
  - Fills upsample it with a 4-tap depth-weighted filter (`textureLoad`
    only). It darkens ambient light only, as `min(baked AO, SSAO)` so baked
    meshes are not darkened twice.
- Why a pre-pass, not Stage 3's pass A: AO is read inside the pass that
  shades the fills, so pass A's depth arrives too late. Applying it in the
  composite would darken direct light too, or need an extra ambient MRT,
  and the direct path (HDR off) has no pass A at all.
- One triangle count and one gate (`EXTRA_PASS_TRIANGLE_BUDGET`, 8M fill
  triangles; `SHADOW_TRIANGLE_BUDGET` is the same constant) decide both
  extra passes. Scenes above it draw without shadows or AO, and the
  `set_display` reply says why (`ambient_occlusion.last_frame`).
- GPU environments are cached per baked probe (`Arc<EnvProbe>`, up to 8,
  least recently used goes). Each texture is written once and then only
  read, so the window and `render_image` (screenshots, `render_animation`)
  never overwrite each other's environment
  (`environments_never_overwrite_each_other`). AO targets are per slot;
  `render_image` holds the renderer through its submit, so its slot's
  targets are safe to reuse.
- Shadows: the studio (and classic) lighting shadows `shadows.key_direction`
  (render_film's key by default), as before. Any other lighting shadows
  its brightest sun, for example the sunset sun with `lights: "none"`. An
  overcast sky with no lights has no sun, so it draws no shadows (reason:
  "the lighting has no sun").
- Masks, the background and the grid and wires are unchanged.
- `lighting: "classic"` keeps the old lighting (no environment, no AO). The
  Stage 1 baseline test and the Stage 1 and 3 pictures use it, and they
  still reproduce byte for byte on this machine.
- Controls:
  - The Lighting menu has Studio, the environment presets, Classic and Last
    Film Look, plus an Ambient Occlusion checkbox.
  - `set_display` takes `lighting` (a string as above, `"film"`, or
    `{environment, lights, extra_lights, look}` as render_film takes them)
    and `ambient_occlusion` (`true`/`false` or `{enabled, radius, strength}`;
    defaults on, 0.5, 1). Its reply echoes both.
  - `render_views` takes the same two for one call.
- Cost: a 1280x720 `render_image` of the studio shot (HDR, readback
  included, mean of 10) takes 5.8 ms classic, 6.2 ms with environment
  lighting and 6.3 ms with AO as well.
- Memory: AO adds 17 bytes per half-size pixel, about 9 MB for a 1920x1080
  pane. A preset environment takes about 0.35 MB, shared by every pane.

### Pictures

| File | What |
| --- | --- |
| `ibl_fixture_before.png` / `ibl_fixture_after.png` / `ibl_fixture_before_after.png` | The fixture through the default HDR finishing: the renderer before this stage (classic lighting, no AO), then render_film's studio with AO. |
| `ibl_studio_*.png` | The same for the floorless studio shot with the grid. |
| `ao_before.png` / `ao_after.png` / `ao_before_after.png` | The new studio lighting without and with ambient occlusion: contact darkening under the ball, at the cylinder's and capsule's feet and along the cube. |
| `environments.png` | Pairs of viewport and render_film (64 samples), two pairs per row: studio, overcast, sunset, golden_hour, forest, night, storm. Each uses render_film's `environment` argument, so its studio lights too. A chrome ball shows the reflections. `environments.json` has the settings. |
| `film_match.png` | The energy check's matte ball under overcast (top) and sunset (bottom) with `lights: "none"`: viewport (ACES) and render_film (CPU, 256 samples). |
| `lighting.json` | Cameras, lighting and AO settings of the pictures above. |

### Validation

Measured on this machine (Apple GPU, both the sRGB and manual-gamma
targets):

- `viewport_matches_film_energy`: the viewport's linear light over a lone
  matte ball (exposure 0.5, no tonemap) against render_film's denoised
  linear radiance (CPU, 256 samples), by luminance over the top, middle,
  bottom, left and right of the ball. It is within 2.0% everywhere: at
  most 1.7% under overcast and 2.0% under sunset, on the sun's far side.
  The test allows 5% for overcast and 6% for sunset.
- `ao_darkens_creases_but_not_open_floor`:
  - Under overcast with no lights (ambient only), the floor at the cube's
    foot drops by 11/255 and just in front of the ball's contact by
    28/255 (the test asks for at least 8).
  - In the studio, where the suns light the floor too, the drops are 3 and
    7 (the test asks for at least 2).
  - Open floor and the ball's top change by at most 2/255, and fewer than
    0.5% of pixels get brighter.
- `metal_reflects_the_environment`: a chrome ball's mean is blue-white
  under overcast, more than 1.5x sunset's, near black at night, and its top
  is over 2x its bottom (sky over ground).
- `reflection_blur_tracks_roughness`: a storm sky's reflected detail
  (mean neighbour difference) is 38, 10 and 4.7 at roughness 0.1, 0.5 and
  0.9.
- `view_light` (CPU only):
  - SH9 irradiance against brute-force quadrature at 14 normals: within
    0.4 to 1.7% of the peak, and 3.3% for sunset's narrow horizon glow
    (tolerance 3%, sunset 4%). Rotated environments turn with it.
  - Mip 0 against `Environment::at` (sun excluded) at 500 directions:
    within 2e-3 relative, including a rotated map.
  - Each mip level's mean is within 2% of mip 0's.
  - The sunset sun gives exactly (4.0, 0.9, 0.25), and intensity scales it.
  - The studio suns are `film::LIGHTS`. The BRDF table is bounded, with
    scale + bias = 1 for a smooth surface seen head on.
  - Also checked: f16 rounding, and that parse accepts render_film's
    lighting arguments and rejects bad ones.
- Also: WGSL validation of both shaders (the Globals span equals
  `size_of`), every preset, AO setting and odd size drawing without
  validation errors, repeatable renders (within 1/255), masks unchanged by
  lighting and AO, the over-budget gate turning AO off, and the window
  path (prepare and paint, AO in a pane slot) matching `render_image`
  within 1/255.

### Reproduce

```sh
cargo test --release lighting_sheets -- --ignored --nocapture
# OUT=dir writes elsewhere; GRAPHICS_SIZE=640x480 changes the size (480x360).
cargo test --release -- view_light graphics_checks render:: set_display
```

## Stage 5D: variance-guided film denoiser

### What changed

- `render_film`'s denoiser (`src/film/denoise.rs`) is guided by each
  pixel's measured noise, as in SVGF. Both tracers now also sum, per
  pixel, each sample's lighting luminance (its colour over its own
  first-hit albedo) and its square. They go in the two `accum` lanes that
  were 0 before (`trace.wgsl` `main`, read back in `gpu.rs`), so the GPU
  buffers did not grow. The CPU (`View::pass`) computes the same with the
  same luminance weights, the same non-finite colour clamp and the same
  0.5 albedo for a bad (NaN) guide. The CPU now also adopts that GPU guard;
  before, it summed the bad guide. From these sums,
  `var = max(0, Σl²/n − (Σl/n)²) / n` is the variance of the pixel's mean
  lighting.
- Each à-trous pass first smooths the variance over the same surface
  (3x3 Gaussian, normal and depth tests). The brightness weight then
  allows `3·σ + 0.1·(local noise of both pixels)`. The variance carries
  into the next pass with the squared weights. So clean pixels (lit
  flats, sharp shadows every sample agrees on) keep their neighbours out,
  and noisy ones (soft shadows, glossy bounces, hair) are smoothed harder.
  The normal^128, depth and reflected-border rules are unchanged.
- The old filter scaled its brightness width only by the sample count, and
  wide enough to work at 16 samples. That flattened soft contact shadows
  and shading (the "before" pictures). Below 4 samples there is no usable
  variance, and the classic width is used.
- An approximation, documented in `denoise.rs`: the filtered lighting is
  the mean colour over the mean albedo, not the mean of each sample's
  ratio. They differ only where the first hit's albedo changes inside a
  pixel.
- Every pass (and the noise and variance pre-passes) writes each pixel
  from the previous pass's picture, in rows on all cores
  (`clay::par_chunks`). The output is bit-identical for any thread count
  (`denoise_is_thread_count_independent`).
  - At 1600x900, five passes: 2.19 s on one thread, 0.27 s on all cores
    (`denoise_timing`, ignored).
  - The classic path on one thread, which is numerically the previous
    filter, takes about 2.0 s.
- Controls: `render_film` takes `denoiser: "variance"` (the default) or
  `"classic"` (the previous filter). `denoise: true | false` is unchanged
  and still defaults to true.
- No existing test expectation changed. The tracked `fire_check/` pictures
  are rendered at 32 px with `denoise` off, so this stage does not change
  them.

### Pictures

Each `*_before_after.png` has four tiles: the raw samples, classic
(before), variance-guided (after), and a 2048-sample reference without
denoising. Before and after come from the same trace, so their samples
are identical. All are 320x240, traced on the CPU.

| File | Samples | PSNR vs reference: raw / classic / variance |
| --- | --- | --- |
| `denoise_fixture_16spp_*` | 16 | 38.2 / 32.1 / 40.5 dB |
| `denoise_fixture_64spp_*` | 64 | 44.9 / 40.6 / 47.9 dB |
| `denoise_closeup_16spp_*`: scaly, colour-textured ball under 2000 hair strands, on a floor, golden hour | 16 | 31.0 / 30.6 / 34.1 dB |
| `denoise_texture_16spp_*`: the same ball filling the frame | 16 | 28.0 / 29.2 / 29.7 dB |

Edges kept are the mean gradient over the reference's strongest 5%
gradients, as a fraction of the reference's. For classic / variance:

| Picture | Classic | Variance |
| --- | --- | --- |
| fixture, 16 spp | 0.98 | 1.03 |
| fixture, 64 spp | 0.97 | 1.00 |
| close-up | 0.97 | 0.99 |
| texture | 1.00 | 1.01 |

Texture detail kept is the correlation of high-pass luma with the
reference's, on a hair-free band of the ball:

| Picture | Raw | Classic | Variance |
| --- | --- | --- | --- |
| texture | 0.86 | 0.911 | 0.911 |
| close-up | 0.83 | 0.909 | 0.908 |

### Tests

- `graphics_checks::variance_denoiser_beats_classic_at_low_samples`: 16
  samples against 1024, smaller pictures.
  - Fixture (96x72): 35.4 dB against classic 30.1 and raw 35.1.
  - Close-up (128x96): 33.8 dB against 29.0 and 30.8. It must beat
    classic by at least 1 dB and beat the raw samples.
  - Edges kept must be at least classic's and above 0.95.
  - The frame-filling texture shot is about even in PSNR (29.64 against
    29.63). It asserts no worse than classic minus 0.1 dB, at least 1 dB
    better than raw, and detail kept within 0.02 of classic's (0.779
    against 0.791) and clearly above raw's (0.678).
  - So at this small size the new filter does not sharpen texture detail
    beyond the old one, and keeps essentially all of it.
- `film::gpu::tests::gpu_matches_cpu_lighting_moments`: the CPU and GPU
  moment lanes agree over a 256-sample picture with skin, gloss and a
  floor.
  - Mean demodulated lighting: CPU 1.7257, GPU 1.7261 (2% allowed).
  - Mean per-sample spread: 0.4554 against 0.4574 (3% allowed).
  - The GPU must really run (`assert_honours`).
- `film::denoise::tests`:
  - The three previous tests pass unchanged, with a variance of 0 (the
    classic path). The gradient and depth-edge tests also run with a
    measured variance.
  - New: `clean_lighting_edges_stay_sharp_with_variance` and
    `denoise_is_thread_count_independent`.

### Reproduce

```sh
cargo test --release denoise_sheets -- --ignored --nocapture   # OUT=dir writes elsewhere
cargo test --release -- denoise variance_denoiser gpu_matches_cpu_lighting_moments
cargo test --release denoise_timing -- --ignored --nocapture
```

## Stage 5E: physical sun and sky

### What changed

- A new environment, `physical`, is a physical sun and sky
  (`src/film/sky.rs`). It is single scattering of sunlight through a
  spherical atmosphere (Nishita): a 6360 km planet under 60 km of air.
  - Rayleigh scattering by air (β = 5.8, 13.5, 33.1 per 10⁶ m, 8 km scale
    height).
  - Mie scattering by haze (21 per 10⁶ m at turbidity 2.5, 1.2 km,
    Cornette-Shanks phase with g = 0.76).
  - Ozone absorption (a tent around 25 km).
  - Each texel of a 512x256 equirect marches its view ray in 32 steps, and
    the sunlight reaching each step in 8 more. Steps grow quadratically,
    because the air thins with height. All of it runs in f64, in rows on all
    cores.
  - Below the horizon is a diffuse ground (`ground_albedo`) lit by the sun
    and the baked sky. It fades into the horizon haze over the first 5
    degrees.
- The sun is not painted into the map: it is far smaller than a texel. It
  becomes the environment's sun disc, 0.53 degrees across. Its irradiance
  is the same sunlight times the transmittance along its path, so the sun
  and the sky redden and dim together. The disc goes through the existing
  disc sampling (`sun_chance`, MIS), as the presets' suns do. Below the
  horizon there is no disc, only a twilight sky.
- **The absolute brightness is a chosen constant, not physics.** `SOLAR`
  (6.2075) is the sunlight above the atmosphere. It is set so a 30 degree
  sun, with the default haze and ground, lights the ground as brightly as
  `golden_hour` does (horizontal irradiance luminance 2.5907 for both). So
  render_film's exposure and look presets suit it as they are. The test
  that pins it, `physical_sky_matches_golden_hour_irradiance_at_30deg`, is
  a calibration check, not a physics check. Sun-to-sky ratios and colours
  do come out of the scattering. Multiple scattering is left out, so the
  sky is dimmer than a real one; no comparison against a measured sky was
  made. At 30
  degrees, the sky gives 12% of the ground's light.
- One code path for both tracers and the viewport. The bake goes through
  `Environment::map` and the disc through `Environment::with_sun`, which
  the presets now share. The CPU tracer, the GPU tracer (the same uploaded
  map and disc, with no WGSL change) and the viewport's IBL
  (`view_light::EnvProbe`: SH9 irradiance, GGX mips, and the disc as the
  shadow-casting directional light) all read the same data. `rotate` and
  `intensity` work as they do for the presets. Bakes are cached by their
  settings (8 at a time). A bake takes 45 ms on this machine.
- The default sun azimuth is render_film's key (0.45, 0.75, 0.55), which is
  39.29 degrees from +Z toward +X. A default physical sun therefore stands
  where the film key and the viewport's studio key do.
- The named presets are unchanged, and so are their maps, suns and every
  pinned picture. One shared tail was factored out (`with_sun`), which
  computes the same values.
- Controls:
  - `render_film` and `set_display lighting` take `environment:
    "physical"` (30 degrees), or the shortcuts `sky_noon` (60),
    `sky_afternoon` (30), `sky_golden` (8) and `sky_sunset` (1.5).
  - The object form is `{preset: "physical" (or a shortcut), sun_elevation
    (-5..90), sun_azimuth (degrees), turbidity (1..10, default 2.5),
    ground_albedo (0..1, default 0.3), intensity, rotate}`.
  - Errors name the field and its range. The sky settings on any other
    preset are an error.
  - The viewport's Lighting menu lists the four shortcuts.
  - Fog does not follow the sky (aerial perspective is a follow-up): set
    the fog `color` to match.
- One fix outside the sky: the viewport's probe cache now keeps the
  default studio entry when it evicts. Before, with more than 16
  environments baked in one process, the cache could drop studio, which
  `ViewLighting::studio_default` still holds, and bake a second copy. The
  new sky tests made this happen in the suite
  (`studio_lighting_uses_film_lights`).

### Pictures

| File | What |
| --- | --- |
| `sky_before.png` / `sky_after.png` / `sky_before_after.png` | render_film (GPU, 128 samples, `lights: "none"`, environment background). Before is `golden_hour`; after is a physical sun at golden_hour's sun position (elevation 16.9, azimuth 45). |
| `sky_elevation_sheet.png` | render_film as the physical sun goes down at azimuth 45: 90, 60 and 30 degrees (top), then 10, 3 and 0 (bottom). The sky deepens to a dark blue overhead as the low sky and the sun redden. At 0 degrees, the sun sits on the horizon and the scene is very dark. |
| `sky_viewport.png` | Viewport and render_film pairs, two pairs per row: `sky_noon`, `sky_afternoon` (top), `sky_golden`, `sky_sunset` (bottom). The viewport draws its own background, not the sky. |
| `sky.json` | Cameras, settings and the bake time. |

### Validation

Measured on this machine (Apple M2 Max):

- `film::sky::tests::physical_sky_sun_warms_and_dims_toward_horizon`: the
  sun's irradiance at 60, 30, 10 and 2 degrees is (5.63, 5.14, 4.42),
  (5.25, 4.48, 3.46), (3.89, 2.52, 1.21) and (1.16, 0.30, 0.02). Blue over
  red falls 0.78, 0.66, 0.31, 0.02, and brightness falls with it. At -2
  degrees there is no disc.
- `film::sky::tests::physical_sky_warms_toward_a_low_sun`: the sky 5
  degrees up toward the sun warms monotonically, with blue over red 1.74,
  0.91, 0.33 and 0.06. The zenith stays blue (blue over red at least 1.5)
  at every elevation. A black ground gives no ground light.
- `film::lighting::tests::physical_sky_matches_golden_hour_irradiance_at_30deg`
  (calibration): luminance 2.5907 against 2.5907. It allows 5%.
- `film::lighting::tests::physical_sky_samples_its_sun_and_map_like_a_preset`:
  the disc is picked at its `sun_chance`. Each sample's pdf is `pdf()`'s,
  and the map's pdf integrates to its share. `rotate` and `intensity` turn
  and scale the sun and the map as they do a preset's. Sky settings on
  `golden_hour` are refused, and a sun below the horizon leaves a lit
  twilight with no disc.
- `film::gpu::tests::gpu_matches_cpu_under_a_physical_sky` (GPU asserted to
  have traced): a ball on a floor, 1024 samples. The mean pixel difference
  is 0.60/255 at 60 degrees and 0.41/255 at 20 degrees with azimuth 120 and
  turbidity 4 (`MATCH` allows 2.0).
- `graphics_checks::physical_sun_moves_shadows`: the sun at azimuth 126
  and then 306 (either side of the camera's line of sight, elevation 35).
  The floor where each sun puts the ball's shadow measures:

  | Renderer | Under its own sun | Under the other sun |
  | --- | --- | --- |
  | render_film (CPU) | 32.7 | 140 |
  | Viewport, sRGB target | 30 | 140 |
  | Viewport, manual-gamma target | 35.7 | 138.6 |

  The viewport shadows the sky's sun, on both targets.
- `graphics_checks::viewport_matches_film_energy` now also runs
  `sky_afternoon`, `sky_golden` and `sky_sunset`. The viewport's linear
  light over a matte ball is within 1.2% of render_film's in every band,
  on both targets (the test allows 3%).
- `view_light::tests::physical_sky_lights_the_viewport`: the sky's sun is
  the viewport's first (shadowing) directional light, with the irradiance
  the tracers sample. SH9 irradiance is within 2.27% of the peak of brute
  force. Every shortcut name lights the viewport.
  `lighting_pipelines_build_and_draw_without_errors` draws `sky_golden` too.
- `film::sky::tests::{named_presets_and_parsing, bakes_repeat_exactly}`:
  defaults, ranges and the key azimuth. A fresh bake equals the cached
  one exactly; each row is computed on its own, so the thread count
  cannot change it.
- Seen but not fixed here: with the sun at 8 degrees in front of the
  camera, a patch of floor reads about 25% darker in the viewport than in
  render_film. The ball matches within 1.2% under the same sky, and the
  same unshadowed patch matches within about 3.5% under `sunset` and
  `overcast`.
  This is probably the viewport's
  analytic gloss at grazing light; this stage did not investigate it.

### Reproduce

```sh
cargo test --release sky_sheets -- --ignored --nocapture   # OUT=dir writes elsewhere; GRAPHICS_SIZE=WxH
cargo test --release -- sky physical viewport_matches_film_energy lighting_pipelines
```

## Stage 4: repeated geometry

### Measured first

The scene: 9 identical towers on a 3 m grid (`instancing_checks::tower_scene`),
each the hard-surface tower fixture with 48 floor slabs, a ring of 40
posts per floor (an array of a path array), 8 columns and a blended dome,
`detail_cell` 0.02. It is rendered through render_film's path (items from
`scatter::draws`, `film::Meshes::new`, `render_views_with_look`, device
auto) at 320x240, 16 samples, on an Apple M2 Max.

Before this stage (commit bf7ce5e), the GPU tracer traced the scene with
no fallback, regular and fine:
`device: {used: gpu, fallback_reason: null}`. Every buffer fits one Metal
binding by 5-8x. The largest is the fine mesh's vertices at 536 MB; the
triangles take 494 MB and the ray tree 176 MB. Without sharing, the first
buffer to pass the 4 GiB binding limit is the vertices, at about 68
identical fine towers. So the film part chosen here is plan 5-C step 1:
shared vertices go up once. The two-level BVH (4-D) and the extra
"hi" bindings (5-C2) are not built: they are not needed for this scene,
and they would change the tracer's Params/Item/bindings and stack. The
GPU layout is unchanged: 19 bindings, the same Item and Params, and a
32-entry stack.

### What changed

- **Identical clay shares one build** (`mesh.rs`). Before building a
  clay object, the mesh library looks for another object with the same
  build key (clay, palette, library, fallback material). It finds them
  by a hash of the key, then checks they are equal. It takes that
  object's meshes, or waits for its build. Rigged clay and cloth never
  share. Each object keeps its own entry pointing at the shared meshes,
  so baking, posing or editing one object affects only that object. A
  bake belongs only to the object baked. An edit gives the object its
  own build; editing it back makes it share again, with no build.
  Fine meshes are shared by rest mesh: one fine build, and every object
  showing that rest mesh gets it. render_film waits on that same build.
- **The viewport draws them as one mesh.** `MeshLib::key_of` gives every
  object that draws the same mesh the key of the lowest such id. The mesh
  is uploaded once and drawn in one instanced call. If that object is
  deleted, the next lowest id takes over in the same sync. The renderer
  and shaders are unchanged.
- **Film uploads shared vertices once** (`film/gpu.rs`). Vertices are in
  object space and each item has its own matrix, so items sharing an
  `Arc<Mesh>` share one vertex block. Per-item smart and imported-alpha
  data stay per item, relative to the shared base. The pre-flight
  `BufferPlan` counts vertices the same way. The triangles and ray tree
  are still flattened per item, as before.
- **`instance_array` MCP tool** (`envkit/scatter.rs`, `clay.rs`). It turns
  an array blob into a hidden one-copy clay plus a scatter of its copies,
  both under the object. Copies then draw, render and export through the
  existing scatter instancing. Rotations compose with `level: "leaf"`.
  The copy meshes at the object's cell size, as a one-copy array. As a
  lone box or extrude it would get the dense exact planar mesh: about
  17x the triangles for one column. The rest of the object keeps its cell size too.
  The tool refuses in these cases:
  - the blob is not an added, unmirrored array;
  - the object is rigged, hollowed or a linked mirror copy;
  - unless `force` is set, another blob blends with, carves into or
    covers any innermost copy's box;
  - unless `force` is set, the clay has sculpt, paint, masks or decals.

  The scatter carries `placed_by: "array"`, and the scatter tool will not
  place it again. `get_scene`'s budget now counts shared meshes once in
  `unique_triangles`. It adds `shared_meshes`, `arrays` (copies, nested,
  instanceable) and `hints`.
- **Exports share meshes by content** (`gltf.rs`, `fbx.rs`). glTF nodes
  and FBX Models that come from the same mesh, tint and material point at
  one mesh or one Geometry. This covers identical clay objects as well as
  scatter copies. `export_glb instancing: "ext"` writes one node per
  scatter source with EXT_mesh_gpu_instancing TRANSLATION, ROTATION and
  SCALE. The extension is listed in `extensionsUsed` only. `glb_read`
  expands it, so `check_export` renders every copy.
- Every change is bit-exact. Pictures, meshes and pins are unchanged:
  `instancing_separate.png` and `instancing_shared.png` are
  byte-identical. Exported files differ only in sharing.

### Numbers (`instancing_perf.json`)

`separate` turns sharing off. It reproduces the run made before this
stage: the same 9 builds, 8.9 s / 17.0 s, triangle counts, buffer sizes
and export sizes. `instanced` additionally runs `instance_array` on every
tower's posts and columns. Its floors are refused because the dome
blends with the top slab.

| | separate (before) | shared | instanced |
|---|---|---|---|
| clay builds, regular / fine | 9 / 9 | 1 / 1 | 3 / 3 |
| regular build, s | 8.98 | 0.96 | 0.23 |
| fine build, s | 16.97 | 1.84 | 0.46 |
| fine triangles traced | 10,786,932 | 10,786,932 | 12,286,404 |
| fine unique triangles | 10,786,932 | 1,198,548 | 877,552 |
| film device (regular and fine) | GPU, no fallback | GPU, no fallback | GPU, no fallback |
| fine ray-tree build, s | 2.61 | 2.47 | 2.85 |
| fine GPU vertices, MB | 535.6 | 59.5 | 36.9 |
| fine GPU triangles + nodes, MB | 494 + 176 | 494 + 176 | 562 + 204 |
| glb (regular mesh), MB | 198.1 | 22.0 | 14.2 |
| fbx (regular mesh), MB | 337.3 | 37.5 | 23.9 |
| glb meshes / fbx geometries | 9 / 9 | 1 / 1 | 3 / 3 |

Instanced copies are separate closed shapes, so they draw about 13% more
triangles than the hard union they came from. Because film still
flattens instances, the traced triangle count rises by the same amount.

### Tests

These are in `src/instancing_checks.rs`. GPU tests skip without an
adapter.

- `duplicates_share_one_build`: 1 build for 9 objects. An edit rebuilds
  one object, and editing it back shares again without a build.
  Deleting the shared key's owner re-elects in the same sync.
- `shared_meshes_stay_per_object_for_bakes_and_rigs` checks that a bake
  stays on its own object and that rigged clay never shares.
- `background_builds_and_fine_meshes_are_shared` checks the window's
  background sync: 1 regular and 1 fine build for 5 objects.
- `shared_meshes_render_and_upload_like_separate_ones`: identical CPU
  pixels with and without sharing. Vertices are counted once.
- `gpu_matches_cpu_on_shared_meshes`: it asserts the GPU traced, the GPU
  is within 2.0 MAD of the CPU, and shared vs separate GPU renders are
  within 1/255.
- `viewport_draws_shared_meshes_once_and_the_same` runs on both targets.
  It checks one fill key against five, shaded output within 1/255, and
  identical masks.
- `identical_objects_export_one_mesh` (glTF and FBX) and
  `gpu_instancing_extension_round_trips` (instance transforms read back
  within 1e-5).
- `instance_array_checks_what_would_change` and
  `instance_array_meshes_one_copy_and_looks_the_same`: drawn triangles
  are within 10% and the silhouette within 1/255 MAD.

### Reproduce

```sh
cargo test --release tower_report -- --ignored --nocapture
# TOWERS=n FLOORS=n POSTS=n SIZE=WxH SAMPLES=n FINE=0 ONLY=shared OUT=dir
cargo test --release instancing_checks
```
