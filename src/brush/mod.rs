//! The shared brush module (docs/SCULPT_PLAN.md 3.1-3.3): what a stroke
//! is and how it becomes dabs, for clay sculpt and clay paint (and, later,
//! the mask, groups and Poly sculpt).
//!
//! - `spec`: the authored `StrokeSpec` (JSON in and out, reads today's
//!   sculpt and paint strokes);
//! - `resolve`: spec -> object-space dabs (frames, snapping, LazyMouse,
//!   spacing, pressure, taper, kinds, symmetry);
//! - `curve`: falloff curves and focal shift;
//! - `pressure`: pressure curves for size and strength;
//! - `path`: path points, spacing, LazyMouse, taper;
//! - `symmetry`: mirror and radial copies;
//! - `rng`: the seeded generator for spray;
//! - `alpha`: alpha images, how strokes name them and how dabs sample them;
//! - `vdm`: VDM stamps (Alpha 3D), captured from clay and kept as alphas;
//! - `preset`: brush presets, built in and saved in the scene.

pub mod alpha;
pub mod curve;
pub mod path;
pub mod preset;
pub mod pressure;
pub mod resolve;
pub mod rng;
pub mod spec;
pub mod symmetry;
pub mod vdm;
