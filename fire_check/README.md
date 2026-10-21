# Fire / smoke visual checks

Native Rust fluid simulations and the Metal film tracer. No generated reference art.

Close-ups are 512 × 512, 64 samples, a 64 × 128 × 64 hero grid,
shadowed smoke scattering, a night environment, ACES tone mapping, 0.6
exposure and restrained bloom. Edge-aware surface denoising removes material
sampling grain; background volume detail is left intact. Cheap scene presets retain 32-cell grids.

- `torch_00.png` through `torch_11.png`: twelve successive cached frames
  at 24 fps. `torch_sequence.png` arranges them left to right, then top to
  bottom in four columns.
- `campfire_05.png`: separate log emitters, charred logs with an independent
  emissive fissure texture, and a glowing ember bed.
- `brazier_05.png`: fire and coals sit inside the visible bowl.
- `candle_05.png`: steady narrow emitter, reduced turbulence, blue base,
  and no flying sparks.
- `smoke_column_05.png`: directional illumination and shadowed curly smoke.
- `crypt.png`: the MCP-built crypt with legacy kit flame meshes and point
  lights replaced by simulated fire volumes (one hero wall torch, cheap
  volumes elsewhere); 512 × 512, 128 samples.
  The original `env_check/crypt_mcp.json` remains unchanged.
- `round2/`: unchanged round-two renders (commit `f00c99b`).
  `*_before_after.png` places round two on the left and round three on the right.
  `before/` retains the original round-one renders. Close-up framing is tighter
  in round three; the crypt uses the original camera in a square image.
  Torch frames 00, 03, 06 and 09 each have an individual
  pair; `torch_before_after.png` is the frame-03 preset comparison.
- `crypt_torch_closeup.png`: 512 px, 128-sample close-up of one hero wall torch in the
  same crypt, illuminating the adjacent stone.
- `*_atlas.png`: two view rows, 12 frame columns, 64 px tiles, top-left
  origin; unused space is transparent. GLB uses soft alpha BLEND cross cards
  and `extras.maya_fire` for engine UV playback.
- `*_cpu.png`, `*_gpu.png`: 32 × 32, eight-sample image parity checks for
  torch, candle, campfire, brazier and smoke. Separate coefficient probes
  compare emission, extinction and scattering under a nonuniform transform.

Fuel/heat transport uses limited MacCormack correction, buoyancy, pressure
projection and vorticity confinement. A filled bright core blends into a
mixture-fraction reaction band higher up.
Height-dependent cooling and fuel burnout end the luminous flame before the
dark smoke. Two advected analytic curl octaves displace volume lookups
according to local emission/density energy, with detail increasing with height.
Overlapping central and eight peripheral sources connect the campfire and
brazier roots while permitting separate tongues above. CPU and WGSL apply the
same formulas. Soot absorbs light and reduces smoke albedo. Scene lights
integrate the core and reaction-band emission, with preset light gains
recalibrated for the brighter root. Bloom is 0.008 for isolated flames and
0.02 for the crypt. The crypt check uses the simulated smoke without an
additional atmospheric fog layer; ACES compresses the hot core to white
while the emitted scene light retains its warm color. Secondary volume
emission uses the same indirect-light clamp as surface emission to suppress
rare bright reflection samples. Primary flame radiance remains unclamped.
Smoke lighting is evaluated every four half-voxel steps; flame coefficients
are sampled at every step. These are artist-controlled bounded fluids,
not a combustion chemistry model.

Reproduce from the repository root with Metal GPU access:

```sh
cargo test --release fire_film_checks -- --ignored --nocapture
cargo test --release fire_crypt_check -- --ignored --nocapture
cargo test --release gpu_matches_cpu -- --nocapture
```

For fast iterations, set `FIRE_CHECK_PREVIEW=1`, `FIRE_CHECK_PRESET=torch`
and `FIRE_CHECK_SIZE=128`. Preview mode writes one image and skips exports;
the default suite requires the full requested sample count and a GPU adapter.
`FIRE_CHECK_CONTACT_ONLY=1` rebuilds the four original torch-frame comparison
pairs from existing images without re-running simulation or tracing.

Editable `.json` scene caches and `.glb` flipbooks are reproducible generated
artifacts, ignored by git. The reusable `log` kit piece supplies the charred log mesh and its emissive
texture. The close-up recipe in `src/fire_checks.rs` places logs/coals for
rendering; logs are included in the editable campfire scene, while the small
coal bed is built by the render recipe. Props are separate from the fluid cache. Images and the recipe are tracked.
