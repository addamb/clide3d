# Environment kit checks

Pictures from the environment kit branch, rendered with `render_film`
(`device: "auto"`, the GPU) through the editor's MCP tools.

- `crypt_iso.png`: an 8 x 8 m crypt from the `iso` camera, 1600 x 900,
  256 samples (about 50 s on an M2 Max): flagstone floor, walls with an
  arched doorway and a corridor beyond, corners, two pillars, rubble,
  a sarcophagus, scattered rocks, bones and candles, three wall torches,
  a brazier, chains, light mist, a hero and a ghoul. 21 lights, about a
  million triangles.
- `crypt_close.png`, `brazier.png`: closer iso views of the same room.
- `viewport.png`: the editor viewport on the same room (`set_camera
  preset: iso`), shading with the object lights.
- `kit_sheet.png`: the other pieces (flagstone, bricks and cobble floors,
  stairs, pointed and flat doorways, square pillar, vault, chain, bones,
  candles, a standing torch, a layered rock) under studio light.
- `terrain.png`: a terrain check (cracks, a sculpted mound, painted moss,
  scattered stones, a boulder) under the golden_hour sky.
- `build_crypt.py` builds the crypt through MCP calls (`kit`, `scatter`,
  `create_human`, `edit_clay`, `animate`) and saves `crypt_mcp.json`;
  `build_crypt.py render` renders `crypt_iso.png`. `build_terrain.py`
  builds and renders the terrain check. Both expect the editor running
  with `CLIDE3D_MCP_PORT=7891`.

`OUT=env_check cargo test --release env_check -- --ignored` renders a
similar room headless (no editor, no characters) from `src/envkit/check.rs`.
