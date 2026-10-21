# Face checks

Film renders of the face work (`src/face.rs`, `src/face_hair.rs`,
`src/eyeball.rs`, subsurface transmission in `src/film`), made through the
MCP tools of a running editor.

- `baseline_woman.png`: `create_human` before this work (smooth, waxy).
- `woman_*`, `man45_*`, `old_*`: a young woman (23, freckles), a
  45-year-old man (stubble, moles) and an old man (78, age spots), each:
  `key` (front and three-quarter, key + fill + rim studio lights),
  `rim_profile` (profile against a strong grazing light: soft reddish
  terminator, ears and nose wings glowing), `eye_macro` and `lips_macro`.
  The men also ran `face_detail` (fold geometry on a 0.25 mm grid). All have
  `face_hair` (brows, lashes, peach fuzz). GPU film, 256-384 samples.
- `elf8_*`: the elf8 scene (`.../elf8/elf.json`, opened read-only) after
  `set_human skin`, `face_hair` and erasing its painted brows (its painted
  eyeliner and lips are kept);
  CPU film (its long hair has 185k strands). The head was baked at 4K and
  exported with the scene (`export_glb`); the glb is not kept here.

Recipe for one character:

```
create_human {name: "H", human: {sex: 1, age: 45}, skin: {stubble: 0.65, redness: 0.45, moles: 0.25}}
face_detail {target: "H"}
face_hair {target: "H", brows: {density: 1.3}}
render_film {views: [{yaw: 38, pitch: 2, center: [0, 1.58, 0.1], radius: 0.13}], size: 800, samples: 256, device: "gpu"}
```
