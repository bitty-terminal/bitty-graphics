//! `bitty-graphics`: bounded graphics decode and raster mechanics.
//!
//! Candidate extension crate holding the decode-side relocation from Core
//! `bitty-rich` (`bitty` CTX-0003 / Issue #2, `bitty` W-141 move plan): the
//! whole `kitty_decode` bounded Kitty payload decoder plus the
//! texture-preparation mechanics subset of `kitty_place` (`kitty_raster`:
//! nearest-neighbor scaling, the per-frame blit budget, and the bounded
//! raster cache). Pure relocation, no behavior change; only mechanical
//! adaptation to the new crate home.
//!
//! # Scope (owner decisions, not relitigated here)
//!
//! - Rect geometry stays in Core: the pure rect helpers
//!   (`placement_full_rect`, `placement_rect`, `placement_rect_for`,
//!   `placement_full_rect_for`, `viewport_extent`, `cell_span`) and the
//!   whole placement-policy half (`KittyAction`, `KittyImageLayer`,
//!   `KittyPlacement`, eviction, origin confinement, scroll anchoring,
//!   alternate-screen suppression, paint order) remain Core-owned. Only
//!   pixel scaling, per-frame budgeting, and raster caching move.
//! - Config-file background decode is out of scope: Core `background.rs`
//!   stays untouched (no `image` facade edge travels with this move).
//!
//! # Trust boundary (P0-AC-003 / P0-AC-004 stay satisfiable)
//!
//! This crate is processing, not policy. The enforced check order around
//! every decode is:
//!
//! 1. **Core pre-check**: the caller validates declared size, dimensions,
//!    stride, and buffer length with checked arithmetic against the
//!    contract ceilings *before* submitting bytes here.
//! 2. **Extension refusal**: this crate re-validates before allocating and
//!    returns exact `width * height * 4` RGBA8 bytes or a typed failure;
//!    over-declaration is refused before any large allocation occurs
//!    (P0-AC-003), and `rasterize`/`rasterize_clipped` likewise validate
//!    before allocating.
//! 3. **Core re-validation**: the caller re-validates returned dimensions,
//!    stride, and buffer length before upload, charges the bytes against
//!    the aggregate budget, and owns eviction/refusal (P0-AC-004).
//!
//! A repository split never retires a Core check: Core keeps its
//! pre-allocation and pre-upload validators regardless of the copies that
//! live here.
//!
//! # Dependencies (one-way only)
//!
//! This crate depends on the `png` codec plus its own geometry carriers
//! only. It never depends on `bitty-rich` or `bitty-runtime` (no cycle);
//! Core never depends on extension internals.

#![forbid(unsafe_code)]

pub mod geometry;
pub mod kitty_decode;
pub mod kitty_raster;

pub use geometry::{CellMetrics, ExtentPx, RectPx};
pub use kitty_decode::{
    KITTY_DECODE_MAX_BYTES, KITTY_DECODE_MAX_DIMENSION, KITTY_DECODE_MAX_PIXELS, KITTY_FORMAT_PNG,
    KITTY_FORMAT_RGB, KITTY_FORMAT_RGBA, KittyDecodeError, KittyDecodedImage, KittyTransmitFormat,
    decode_kitty_payload, decode_kitty_payload_owned,
};
pub use kitty_raster::{
    KITTY_PRESENT_MAX_BLITS_PER_FRAME, KITTY_PRESENT_MAX_BYTES_PER_FRAME,
    KITTY_RASTER_CACHE_MAX_BYTES, KITTY_RASTER_CACHE_MAX_ENTRIES, KittyFrameBudget, KittyImageId,
    KittyPlacedImage, KittyRasterCache, KittyRasterKey, KittyRasterStats, rasterize,
    rasterize_clipped,
};
