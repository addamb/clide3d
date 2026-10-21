# Adaptive detail checks

Pictures from `src/detail_checks.rs` (film tracer, 900-1000 px, 64-96
samples). Timings are wall-clock on a 12-core M2 Max shared with other
jobs (load averages are in `bake_timings.txt`), so treat them as rough.

## Human: one clay, 0.25 mm face and hands, 1 mm body

`cargo test --release detail_human -- --ignored --nocapture`

The same `create_human` body and the same strokes (pores and forehead
lines on the face, crow's feet, pores and knuckle creases on the hands,
soft bumps on the torso):

| | before | after |
| --- | --- | --- |
| Detail | one 1.1 mm grid (the finest a 1.7 m body allowed) | 1 mm + face and hand regions at 0.25 mm |
| Fine mesh | 12.0 M triangles, uniform, 37 s | 8.5 M triangles, adaptive, 28 s |
| Pictures | `human_before_{full,face,hand}.png` | `human_after_{full,face,hand}.png` |

Before, the creases and pores blur into the 1.1 mm grid; after, forehead
lines, crow's feet and knuckle creases are crisp, the lip line stays
open, and there is no head/neck seam because the head is in the body
clay. The adaptive mesh is closed and manifold (see
`adaptive::clay_tests`).

## Dragon: finer scales all over

`SCENE=.../dragon3/dragon.json cargo test --release detail_dragon -- --ignored --nocapture`

Before: the agent's dragon as saved (body clay on a 4.6 mm grid, 20.5 M
fine triangles with the head). After: seven detail regions (body, neck,
tail, four legs) at 2 mm, then finer overlapping scales (1-1.4 cm)
sculpted over the spine, flanks and legs; wings stay at 4.6 mm (19.8 M
fine triangles). `dragon_*_shape.png` turn the material's painted scale
pattern off so the sculpted geometry itself shows: the upper flank, which
only had the large scales, now has rows of small domed scales, and the legs
carry a finer scale texture over the old one.

## Kit stone wall: render-time displacement

`cargo test --release detail_wall -- --ignored --nocapture`

The same wall (`damage: 0.6`) with the stone material's detail map as
shading relief only (before) and with `detail_displace: 0.018` (after;
mortar 0.009). The film diced it to 1.8-3.0 M triangles for each camera
(4 s -> 11 s per view). Chips and pits break the stones' outlines and cast
their own shadows (`wall_*_closeup.png`, `wall_*_grazing.png`).

## Bakes

`BAKE_TIMES=1 cargo test --release bake_timing -- --ignored --nocapture`
(`CACHED=1`: the fine mesh already built; `EXACT=1`: every texel from the
field; `SCENE=elf.json BODY=Elf`: the elf body from the friction log).

| Body, 60k triangles | 4K | 8K |
| --- | --- | --- |
| Elf body, original code | 440.6 s | - |
| Elf body, now (from scratch) | 15.3 s | 32.3 s |
| Elf body, now, `exact: true` | 73.5 s | - |
| Sculpted one-clay human (from scratch) | 29.0 s | 40.7 s |
| Same, fine mesh already built | 16.0 s | 28.4 s |

Most of the old time went to rebuilding the whole clay field for every
weathered texel and solving the field at every texel; bakes now cast short
rays at the fine mesh. `bake_human_4k_face.png` is the 60k-triangle game
mesh with its 4K maps: the pores and forehead lines are in the normal map.
`bake_human_{color,normal}_4k.png` are the maps (downscaled); every texel
is filled from the nearest island, so no mip level bleeds background.
