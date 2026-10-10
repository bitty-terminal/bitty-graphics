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
//! # Host seam for Core binding (graphics#14)
//!
//! This section freezes the public surface a Core-owned trait binds to.
//! The trait itself is Core-owned and lands with the Core-side rewire
//! (after 0.0.23), following the `storage_backends.rs` pattern: Core
//! defines the trait, the composition root injects this crate's
//! implementation. What this crate guarantees, starting now, is that the
//! entry points, bounds, and error mapping below do not drift.
//!
//! ## Stable entry points (exact signatures)
//!
//! ```text
//! decode_kitty_payload(format: KittyTransmitFormat, width: Option<u32>,
//!     height: Option<u32>, payload: &[u8])
//!     -> Result<KittyDecodedImage, KittyDecodeError>
//! decode_kitty_payload_owned(format: KittyTransmitFormat, width: Option<u32>,
//!     height: Option<u32>, payload: Box<[u8]>)
//!     -> Result<KittyDecodedImage, KittyDecodeError>
//! rasterize(image: &KittyPlacedImage, rect: RectPx) -> Option<Vec<u8>>
//! rasterize_clipped(image: &KittyPlacedImage, full: RectPx, visible: RectPx)
//!     -> Option<Vec<u8>>
//! KittyRasterCache::get_or_rasterize(&mut self, key: KittyRasterKey,
//!     rasterize: impl FnOnce() -> Option<Vec<u8>>) -> Option<Vec<u8>>
//! KittyFrameBudget::admit(&mut self, need: usize) -> bool
//! ```
//!
//! ## Bounds and error contract the trait binds to
//!
//! | Cap | Value | Bound in code |
//! |---|---|---|
//! | Side | 8192 px/side | [`KITTY_DECODE_MAX_DIMENSION`] |
//! | Area | 4096 x 4096 px | [`KITTY_DECODE_MAX_PIXELS`] |
//! | Decoded bytes | 64 MiB RGBA | [`KITTY_DECODE_MAX_BYTES`] |
//! | Frame blits | 32 | [`KITTY_PRESENT_MAX_BLITS_PER_FRAME`] |
//! | Frame bytes | 64 MiB | [`KITTY_PRESENT_MAX_BYTES_PER_FRAME`] |
//! | Cache entries | 128 | [`KITTY_RASTER_CACHE_MAX_ENTRIES`] |
//! | Cache bytes | 64 MiB | [`KITTY_RASTER_CACHE_MAX_BYTES`] |
//!
//! Every rejection returns [`KittyDecodeError`] with the variant shapes and
//! display strings pinned by the conformance suite (`tests/seam_conformance.rs`):
//! `EmptyPayload`, `MissingDimensions`, `ZeroDimension`,
//! `DimensionsTooLarge { width, height, cap }`,
//! `TooManyPixels { pixels, cap }`, `DecodedTooLarge { bytes, cap }`,
//! `LengthMismatch { expected, actual }`, `MalformedPng(String)`.
//! Bounds run before any pixel buffer is allocated.
//!
//! ## Binding map (read before writing the Core trait)
//!
//! 1. **Raster names.** Core's parallel copy spells the entry points
//!    `rasterize_kitty` / `rasterize_kitty_clipped` over Core-owned
//!    carriers; this seam spells them [`rasterize`] /
//!    [`rasterize_clipped`]. Same behavior, same fail-closed `None`
//!    contract. The trait binds this behavior under either name; no alias
//!    is added here so the surface stays single-spelled.
//! 2. **Format parameter.** This seam takes the mapped
//!    [`KittyTransmitFormat`]; unknown wire `f` values never reach it
//!    ([`KittyTransmitFormat::from_f`] returns `None` and the caller
//!    rejects before calling). Core's parallel copy instead takes the wire
//!    `u32` and maps unknown values to `MalformedPng`. The trait must pin
//!    one spelling for unknown `f`.
//! 3. **Carriers.** [`KittyPlacedImage`] (pub `id`/`width`/`height`/`rgba`/
//!    `compressed_len`), [`KittyImageId`], [`RectPx`], [`CellMetrics`], and
//!    [`ExtentPx`] share their shapes with the Core-owned twins; the only
//!    structural delta is [`KittyDecodedImage`], which exposes accessors
//!    (`width()`, `height()`, `rgba()`, `into_rgba()`) where Core's copy
//!    uses pub fields. Convert at the composition root.
//!
//! ## Core-owned responsibilities (not this crate)
//!
//! Trait definition and composition-root wiring, the pre-allocation and
//! pre-upload validators on both sides of every call, aggregate budget
//! charging with eviction/refusal, placement policy, and dropping Core's
//! PNG edge (`image` feature plus `decode_png`) once the rewire lands.
//!
//! ## Explicit gaps
//!
//! - `KittyRasterCache` and `KittyFrameBudget` have no Core counterpart;
//!   adopting them is additive (no Core behavior to preserve).
//! - Unknown-`f` mapping differs (see binding map item 2); the trait must
//!   choose, this crate does not guess.
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
