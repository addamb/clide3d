# Shape checks

Each PNG compares the original clay (left) with the changed clay (right),
using the production film path tracer: 384 pixels per view, 48 samples,
4 bounces and denoising. These are intentionally small synthetic scenes
that isolate each feature, rather than finished character artwork.

- `ear_stretch.png`: a cupped ear extended into a connected tapered tip.
- `face_creases.png`: sharp eyelid folds and a lip meeting line.
- `tunic_smooth.png`: a loose cover concealing two bumps and a navel pit.
- `sleeve_folds.png`: soft folds following gravity, restricted to a sleeve.
- `chain_braid.png`: one tapered, twisting bead chain replaces a plain tail.

Regenerate the images and before/after scene JSON snapshots with:

```sh
cargo test --release shape_film_checks -- --ignored --nocapture
```

The JSON snapshots are generated locally and ignored by git. Camera and
scene construction live in `src/shape_checks.rs`. The regular test suite
checks mesh connectivity, V-groove sharpness, pinch width, layer height,
cover smoothing, masks, deterministic noise, and tiled brush equivalence.
