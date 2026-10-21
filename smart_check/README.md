# Smart material checks

Film renders (GPU tracer, studio light) made with `src/smart_check.rs`:

- `material_sheet.png`: every smart preset on the test piece (a plate with
  grooves, a rim and rivets, and a ball with studs and a carved ring), in
  `smart::PRESETS` order: worn_iron, rusted_iron, polished_steel,
  gold_trim, bronze / old_leather, cracked_leather, bone, charred_flesh,
  wet_flesh / demon_skin, dungeon_stone, mossy_stone, grimy_cloth,
  blood_splatter (shown over worn_iron). `piece_*.png` are the cells.
  `OUT=smart_check cargo test --release smart_sheet -- --ignored`
- `decals.png`, `decals_back.png`: a glowing rune, a carved rune,
  scratches, a blood splat and a gold emblem on worn iron
  (`smart_decals`).
- `dragon_*.png`: dragon3 with demon_skin on the body and head
  (`SCENE=.../dragon3/dragon.json ... smart_dragon`).
- `elf_*.png`: elf8 with old_leather boots and belt, grimy_cloth and a
  worn_iron buckle (`SCENE=.../elf8/elf.json ... smart_elf`).
