# Texture and hair checks

Film renders of the elf8 scene (64 samples, studio light), made with
`src/texture_check.rs` (`SCENE=... OUT=... cargo test --release
texture_check::texture_check -- --exact --ignored`).

- `before/`: the code before this branch.
- `after/`: this branch. The groom was tuned as an agent would
  (`clump 0.5, clump_levels 3, volume 0.014, frizz 0.0018, flyaways 0.02`).
  The cloth and leather views were rendered before the final preset change
  (linen tile 5 cm -> 12 cm, leather preset leather_smooth -> leather_rough),
  so they understate the final cloth and boot detail. `before/hair.png` uses
  an older, closer camera than `after/hair.png`.
- `after/cards_*.png`: the game card textures (over gray).
- `detail_sheet.png`: every library texture on a sphere, in `LIBRARY`
  order (`texture_check::detail_sheet`).
