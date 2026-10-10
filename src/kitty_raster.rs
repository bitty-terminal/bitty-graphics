//! Kitty raster mechanics: scaling, per-frame budget, raster cache.
//!
//! Relocated mechanics subset of Core `bitty-rich::kitty_place` (`bitty`
//! CTX-0003 / Issue #2, `bitty` W-141 move plan). Pure relocation, no
//! behavior change; only the Core-internal references below were
//! mechanically adapted.
//!
//! What moved here: [`rasterize`], [`rasterize_clipped`],
//! [`KittyRasterKey`]/[`KittyRasterStats`]/[`KittyRasterCache`] plus
//! methods, [`KittyFrameBudget`] plus methods, and the
//! `KITTY_PRESENT_*` / `KITTY_RASTER_CACHE_*` constants. The bitmap
//! carrier ([`KittyPlacedImage`] with its [`KittyImageId`] handle) moves
//! as the supporting input type; identity authority stays in Core.
//!
//! What stays in Core: the placement-policy half (`KittyAction`,
//! `KittyImageLayer`, `KittyPlacement`, `KittyPlacementError`, the
//! `KITTY_PLACE_MAX_*` cap aliases, eviction, origin confinement, scroll
//! anchoring, alternate-screen suppression, paint order) and every pure
//! rect-geometry helper (`placement_full_rect`, `placement_rect`,
//! `placement_rect_for`, `placement_full_rect_for`, `viewport_extent`,
//! `cell_span`).
//!
//! Bounds (threat T-01/T-02): every length is validated with checked
//! arithmetic **before** any buffer is allocated or grown, including the
//! nearest-neighbor output (`rect_w * rect_h * 4`, itself bounded because
//! the caller clamps the rect to the viewport first). This module is
//! processing, not policy: Core pre-checks declared sizes before
//! submitting work here and re-validates returned bytes before upload.

use std::collections::{HashMap, VecDeque};

use crate::geometry::{CellMetrics, RectPx};
use crate::kitty_decode::KITTY_DECODE_MAX_BYTES;

// ---------------------------------------------------------------------------
// Identifiers
// ---------------------------------------------------------------------------

/// Stable handle for a stored decoded image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KittyImageId(pub u64);

impl KittyImageId {
    /// Numeric value for diagnostics only.
    #[must_use]
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Bitmap carrier (input type for the raster mechanics)
// ---------------------------------------------------------------------------

/// Stored decoded image: owned RGBA8 pixels in row-major order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyPlacedImage {
    /// Stable handle.
    pub id: KittyImageId,
    /// Decoded pixel width.
    pub width: u32,
    /// Decoded pixel height.
    pub height: u32,
    /// RGBA8 bytes, exactly `width * height * 4` long.
    pub rgba: Vec<u8>,
    /// Wire payload length (diagnostics; the bytes themselves are decoded).
    pub compressed_len: usize,
}

// ---------------------------------------------------------------------------
// Rasterize (nearest-neighbor scale to the rect extent)
// ---------------------------------------------------------------------------

/// Scales stored RGBA to the exact `rect` pixel extent (nearest neighbor).
///
/// Returns `None` (paints nothing) when `rect` is empty, when the output
/// byte size fails checked validation against the 64 MiB cap, or when the
/// source bitmap fails validation. No allocation occurs before validation.
///
/// The output is straight-alpha RGBA8, row-major, exactly
/// `rect.width * rect.height * 4` bytes — the shape the present layer
/// composites.
///
/// # Host-trait binding contract (graphics#14)
///
/// Frozen seam: signature, nearest-neighbor behavior, 64 MiB output cap,
/// and fail-closed `None` contract are what a Core-owned trait binds to.
/// Core's parallel copy spells this `rasterize_kitty` over Core-owned
/// carriers with identical shapes; the trait binds this behavior under
/// either name.
#[must_use]
pub fn rasterize(image: &KittyPlacedImage, rect: RectPx) -> Option<Vec<u8>> {
    rasterize_clipped(image, rect, rect)
}

/// Scales the visible window of a placement (nearest neighbor, #1334).
///
/// `full` is the unclamped placement extent (the image scales into this,
/// exactly like [`rasterize`] would); `visible` is the viewport-clipped
/// sub-rectangle to emit (`visible` must lie inside `full`). The output
/// is bit-identical to scaling the whole image into `full` and then
/// cropping `visible` — but only the visible bytes are ever allocated,
/// so a placement overflowing the viewport paints its visible part at
/// true scale instead of squeezing the whole image into it.
///
/// Returns `None` (paints nothing) when `visible` is empty or outside
/// `full`, when the visible byte size fails checked validation against
/// the 64 MiB cap, or when the source bitmap fails validation. No
/// allocation occurs before validation.
///
/// The output is straight-alpha RGBA8, row-major, exactly
/// `visible.width * visible.height * 4` bytes.
///
/// # Host-trait binding contract (graphics#14)
///
/// Frozen seam alongside [`rasterize`]: Core's parallel copy spells this
/// `rasterize_kitty_clipped` over Core-owned carriers with identical
/// shapes. Bit-identical to scaling into `full` and cropping `visible`.
#[must_use]
pub fn rasterize_clipped(
    image: &KittyPlacedImage,
    full: RectPx,
    visible: RectPx,
) -> Option<Vec<u8>> {
    if visible.width == 0 || visible.height == 0 || full.width == 0 || full.height == 0 {
        return None;
    }
    // `visible` must lie inside `full` (same origin space); anything else
    // is a caller bug and fails closed. `i64` differences of `i32`
    // coordinates never overflow; non-negative after the origin guard,
    // so the `as u64` casts are exact.
    if visible.x < full.x || visible.y < full.y {
        return None;
    }
    let offset_x = (i64::from(visible.x) - i64::from(full.x)) as u64;
    let offset_y = (i64::from(visible.y) - i64::from(full.y)) as u64;
    if offset_x + u64::from(visible.width) > u64::from(full.width)
        || offset_y + u64::from(visible.height) > u64::from(full.height)
    {
        return None;
    }
    let out_len = (u64::from(visible.width) * u64::from(visible.height))
        .checked_mul(4)
        .filter(|&n| n <= KITTY_DECODE_MAX_BYTES as u64)?;
    // `out_len` fits `usize` on every supported target: it is at most
    // 64 MiB while `usize` is at least 32 bits.
    let out_len = out_len as usize;
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let expected_src = (u64::from(image.width) * u64::from(image.height))
        .checked_mul(4)
        .filter(|&n| n <= KITTY_DECODE_MAX_BYTES as u64)?;
    if image.rgba.len() as u64 != expected_src {
        return None;
    }
    let mut out = vec![0_u8; out_len];
    let (sw, sh) = (u64::from(image.width), u64::from(image.height));
    let (fw, fh) = (u64::from(full.width), u64::from(full.height));
    let (vw, vh) = (u64::from(visible.width), u64::from(visible.height));
    for dy in 0..vh {
        // Nearest neighbor into the full extent, then the visible window:
        // `sy = (offset_y + dy) * sh / fh` — division in u64, exact for
        // the bounded ranges here. Bit-identical to scaling into `full`
        // and cropping `visible`.
        let sy = ((offset_y + dy) * sh / fh) as usize;
        for dx in 0..vw {
            let sx = ((offset_x + dx) * sw / fw) as usize;
            let s = (sy * image.width as usize + sx) * 4;
            let d = (dy as usize * visible.width as usize + dx as usize) * 4;
            out[d..d + 4].copy_from_slice(&image.rgba[s..s + 4]);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Per-frame blit budget + raster cache (CTX-0252 F2)
// ---------------------------------------------------------------------------

/// Maximum image blits composited in one present frame.
///
/// Core retains up to 128 placements (Core policy cap
/// `KITTY_PLACE_MAX_ITEMS`) and every visible one rasterizes to its
/// viewport-clamped rect; without a frame cap the pathological transient
/// is 128 x 64 MiB of scaled bytes per frame. The budget sheds
/// deterministically in paint order (ascending `z`, stable): the first 32
/// visible placements paint and the rest are skipped for that frame only
/// (retained, repainted when earlier placements hide or the budget grows).
/// Ordinary frames carry a handful of images and never touch the cap.
pub const KITTY_PRESENT_MAX_BLITS_PER_FRAME: usize = 32;

/// Maximum scaled blit bytes composited in one present frame (64 MiB).
///
/// Mirrors [`KITTY_DECODE_MAX_BYTES`]: any single viewport-clamped blit the
/// store admits also fits the frame, so the byte cap only sheds
/// pathological multiplicity, never a lone image. Checked **before**
/// rasterizing, so refused bytes are never allocated.
pub const KITTY_PRESENT_MAX_BYTES_PER_FRAME: usize = 64 * 1024 * 1024;

/// Maximum cached raster entries (one per placement cap).
///
/// Mechanical adaptation: mirrors the Core 128-placement policy cap
/// (`bitty-rich::kitty_place::KITTY_PLACE_MAX_ITEMS`, RFC IMG-8 parity).
/// Authority lives in the Core contract; this literal only pins parity on
/// the extension side.
pub const KITTY_RASTER_CACHE_MAX_ENTRIES: usize = 128;

/// Maximum cached raster bytes (one max image worth of scaled output).
pub const KITTY_RASTER_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Per-frame blit budget: deterministic shed for pathological placement counts.
///
/// Created fresh each frame. [`KittyFrameBudget::admit`] returns `true` and
/// accounts `need` bytes while both the blit count and the byte total stay
/// within [`KITTY_PRESENT_MAX_BLITS_PER_FRAME`] /
/// [`KITTY_PRESENT_MAX_BYTES_PER_FRAME`], `false` otherwise (the caller skips
/// that placement for this frame only). Skip-and-continue in paint order
/// keeps small placements painting even when a huge one is shed.
///
/// # Host-trait binding contract (graphics#14)
///
/// Frozen seam: the 32-blit / 64 MiB caps and the admit-before-rasterize
/// discipline are what a Core-owned trait binds to. Core holds no
/// counterpart budget, so adopting this type is additive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyFrameBudget {
    blits: usize,
    used_bytes: usize,
}

impl KittyFrameBudget {
    /// An empty budget for one frame.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a blit of `need` bytes fits; accounts it on success.
    ///
    /// `need` is the checked `rect.width * rect.height * 4` for the
    /// candidate rect. Callers compute it before rasterizing so refused
    /// bytes are never allocated. A single blit larger than the whole byte
    /// cap never fits and is skipped every frame (fail-safe for absurd
    /// viewports; the grid still presents).
    pub fn admit(&mut self, need: usize) -> bool {
        if self.blits >= KITTY_PRESENT_MAX_BLITS_PER_FRAME {
            return false;
        }
        let next = self.used_bytes.saturating_add(need);
        if next > KITTY_PRESENT_MAX_BYTES_PER_FRAME {
            return false;
        }
        self.blits += 1;
        self.used_bytes = next;
        true
    }

    /// Blits admitted so far this frame.
    #[must_use]
    pub fn blits(&self) -> usize {
        self.blits
    }

    /// Scaled bytes admitted so far this frame.
    #[must_use]
    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }
}

/// Cache key for one rasterized placement blit.
///
/// Identity (`placement`, `image`) plus everything that shapes the output:
/// the clamped destination `rect` (position and extent), the source bitmap
/// `src` dimensions (guards image-id reuse), and the frame context — the
/// `scrollback` sequence (content position), `cell` metrics, and `viewport`
/// grid size. Scroll or geometry changes therefore miss instead of painting
/// stale pixels; identical frames hit and skip re-rasterizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KittyRasterKey {
    /// Placement being painted.
    pub placement: u64,
    /// Image the placement binds.
    pub image: u64,
    /// Clamped destination rect (position + extent).
    pub rect: RectPx,
    /// Source bitmap width.
    pub src_w: u32,
    /// Source bitmap height.
    pub src_h: u32,
    /// `State::scrollback_len()` this frame (content sequence).
    pub scrollback: usize,
    /// Cell metrics this frame (geometry).
    pub cell: CellMetrics,
    /// Viewport grid width this frame (geometry).
    pub viewport_cols: u16,
    /// Viewport grid height this frame (geometry).
    pub viewport_rows: u16,
}

/// Snapshot of [`KittyRasterCache`] counters (headless-observable).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KittyRasterStats {
    /// Lookups served without rasterizing.
    pub hits: u64,
    /// Lookups that rasterized (including first fills).
    pub misses: u64,
    /// Entries currently cached.
    pub entries: usize,
    /// Scaled bytes currently cached.
    pub bytes: usize,
}

/// Bounded per-placement raster cache: scaled blits keyed by [`KittyRasterKey`].
///
/// The present loop used to rasterize every visible placement every frame
/// (one nearest-neighbor scale per blit); this cache keeps the scaled bytes
/// so static frames pay the scale once. Bounded to
/// [`KITTY_RASTER_CACHE_MAX_ENTRIES`] entries /
/// [`KITTY_RASTER_CACHE_MAX_BYTES`] bytes, oldest evicted first (FIFO,
/// deterministic for fixed insertion order). Entries are immutable scaled
/// bytes: source bitmaps never mutate under an image id, and every context
/// input rides in the key, so a hit can never paint stale pixels.
/// Structural resets (Core policy-layer clear, alternate-screen entry)
/// clear the cache explicitly via [`KittyRasterCache::clear`]; evicted
/// placements simply stop being looked up (their entries age out under the
/// caps and are never served, because lookups are driven by the live
/// placement list).
///
/// # Host-trait binding contract (graphics#14)
///
/// Frozen seam: entry/byte caps, FIFO eviction, hit/miss accounting, and
/// the never-cache-`None` rule are what a Core-owned trait binds to. Core
/// holds no counterpart cache, so adopting this type is additive.
#[derive(Debug, Clone, Default)]
pub struct KittyRasterCache {
    entries: HashMap<KittyRasterKey, Vec<u8>>,
    order: VecDeque<KittyRasterKey>,
    bytes: usize,
    hits: u64,
    misses: u64,
}

impl KittyRasterCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Entries currently cached.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is cached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Scaled bytes currently cached.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Cumulative cache hits.
    #[must_use]
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cumulative cache misses (each rasterized at most once).
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Counter snapshot.
    #[must_use]
    pub fn stats(&self) -> KittyRasterStats {
        KittyRasterStats {
            hits: self.hits,
            misses: self.misses,
            entries: self.entries.len(),
            bytes: self.bytes,
        }
    }

    /// Cached scaled bytes for `key`, if present (cloned).
    #[must_use]
    pub fn get(&self, key: &KittyRasterKey) -> Option<Vec<u8>> {
        self.entries.get(key).cloned()
    }

    /// Returns cached bytes on hit; on miss runs `rasterize`, caches the
    /// output on success, and returns it. Failures (`None`) are never
    /// cached and count as misses without poisoning the key.
    pub fn get_or_rasterize(
        &mut self,
        key: KittyRasterKey,
        rasterize: impl FnOnce() -> Option<Vec<u8>>,
    ) -> Option<Vec<u8>> {
        if let Some(hit) = self.entries.get(&key) {
            self.hits = self.hits.wrapping_add(1);
            return Some(hit.clone());
        }
        self.misses = self.misses.wrapping_add(1);
        let bytes = rasterize()?;
        self.insert(key, bytes.clone());
        Some(bytes)
    }

    /// Inserts scaled bytes, evicting oldest first to hold the entry and
    /// byte caps. Every [`rasterize`] output fits the byte cap (at most
    /// [`KITTY_DECODE_MAX_BYTES`]), so the loop always terminates with room;
    /// a lone over-cap insert would still store alone rather than thrash.
    fn insert(&mut self, key: KittyRasterKey, bytes: Vec<u8>) {
        if let Some(old) = self.entries.get(&key) {
            self.bytes = self.bytes.saturating_sub(old.len());
        } else {
            self.order.push_back(key);
        }
        while self.entries.len() >= KITTY_RASTER_CACHE_MAX_ENTRIES
            || self.bytes.saturating_add(bytes.len()) > KITTY_RASTER_CACHE_MAX_BYTES
        {
            if let Some(evicted) = self.order.pop_front() {
                if let Some(removed) = self.entries.remove(&evicted) {
                    self.bytes = self.bytes.saturating_sub(removed.len());
                }
                if evicted == key {
                    break;
                }
            } else {
                break;
            }
        }
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.entries.insert(key, bytes);
    }

    /// Drops all cached entries without resetting hit/miss counters.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const METRICS: CellMetrics = CellMetrics {
        width: 8,
        height: 16,
    };

    fn tiny_red() -> (u32, u32, Vec<u8>) {
        // 2x2 opaque red.
        (2, 2, [0xFF, 0x00, 0x00, 0xFF].repeat(4))
    }

    /// Direct bitmap fixture. Mechanical adaptation: Core builds this via
    /// `KittyImageLayer::store` (policy, stays in Core); here the carrier
    /// is constructed directly with identical bytes.
    fn stored_red_image() -> KittyPlacedImage {
        let (width, height, rgba) = tiny_red();
        KittyPlacedImage {
            id: KittyImageId(1),
            width,
            height,
            rgba,
            compressed_len: 16,
        }
    }

    #[test]
    fn rasterize_clipped_matches_crop_of_full_raster() {
        // 4x4 px: top half red, bottom half blue. Visible = bottom-right
        // 2x2 window of the 4x4 full extent: must equal the matching crop
        // of the full raster, never a re-scaled whole image.
        let rgba = {
            let mut bytes = Vec::new();
            for y in 0..4 {
                let color = if y < 2 {
                    [0xFF, 0, 0, 0xFF]
                } else {
                    [0, 0, 0xFF, 0xFF]
                };
                for _ in 0..4 {
                    bytes.extend_from_slice(&color);
                }
            }
            bytes
        };
        let image = KittyPlacedImage {
            id: KittyImageId(1),
            width: 4,
            height: 4,
            rgba,
            compressed_len: 64,
        };
        let full = RectPx::new(0, 0, 4, 4);
        let visible = RectPx::new(2, 2, 2, 2);
        let clipped = rasterize_clipped(&image, full, visible).expect("clipped must rasterize");
        assert_eq!(clipped.len(), 2 * 2 * 4);
        // Bottom-right of the source is all blue.
        assert!(clipped.chunks_exact(4).all(|px| px == [0, 0, 0xFF, 0xFF]));
        // Identity (visible == full) matches the legacy entry point.
        assert_eq!(
            rasterize_clipped(&image, full, full),
            rasterize(&image, full)
        );
        // A re-scaled whole image would mix red into the window.
        let squeezed = rasterize(&image, visible).expect("legacy entry still works");
        assert!(
            squeezed.chunks_exact(4).any(|px| px == [0xFF, 0, 0, 0xFF]),
            "legacy re-scale of the window mixes source halves (the #1334 squeeze)"
        );
    }

    #[test]
    fn rasterize_clipped_fails_closed() {
        let image = stored_red_image();
        let full = RectPx::new(0, 0, 2, 2);
        // Empty visible paints nothing.
        assert_eq!(
            rasterize_clipped(&image, full, RectPx::new(0, 0, 0, 2)),
            None
        );
        // Visible outside full is a caller bug: nothing paints.
        assert_eq!(
            rasterize_clipped(&image, full, RectPx::new(0, 2, 2, 2)),
            None
        );
        assert_eq!(
            rasterize_clipped(&image, full, RectPx::new(1, 1, 2, 2)),
            None
        );
        // Visible bytes over the 64 MiB cap: refused before allocation.
        let big = RectPx::new(0, 0, 9000, 9000);
        assert_eq!(rasterize_clipped(&image, big, big), None);
    }

    #[test]
    fn rasterize_scales_nearest_neighbor() {
        // 2x1: red then green. Scale to 4x2: each source pixel doubles.
        let image = KittyPlacedImage {
            id: KittyImageId(1),
            width: 2,
            height: 1,
            rgba: vec![0xFF, 0, 0, 0xFF, 0, 0xFF, 0, 0xFF],
            compressed_len: 8,
        };
        let out = rasterize(&image, RectPx::new(0, 0, 4, 2)).unwrap();
        assert_eq!(out.len(), 4 * 2 * 4);
        // Row 0: RR GG; row 1 repeats.
        assert_eq!(&out[0..8], &[0xFF, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF]);
        assert_eq!(&out[8..16], &[0, 0xFF, 0, 0xFF, 0, 0xFF, 0, 0xFF]);
        assert_eq!(&out[16..24], &[0xFF, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF]);
        assert_eq!(&out[24..32], &[0, 0xFF, 0, 0xFF, 0, 0xFF, 0, 0xFF]);
    }

    #[test]
    fn rasterize_identity_for_matching_extent() {
        let image = stored_red_image();
        let out = rasterize(&image, RectPx::new(5, 5, 2, 2)).unwrap();
        assert_eq!(out, image.rgba);
    }

    #[test]
    fn rasterize_empty_rect_paints_nothing() {
        let image = stored_red_image();
        assert_eq!(rasterize(&image, RectPx::new(0, 0, 0, 10)), None);
        assert_eq!(rasterize(&image, RectPx::new(0, 0, 10, 0)), None);
    }

    #[test]
    fn frame_budget_admits_within_caps_and_sheds_beyond() {
        let mut budget = KittyFrameBudget::new();
        assert!(budget.admit(1024));
        assert!(budget.admit(2048));
        assert_eq!(budget.blits(), 2);
        assert_eq!(budget.used_bytes(), 3072);
        // Byte cap: exactly the cap admits once, one more byte sheds.
        let mut full = KittyFrameBudget::new();
        assert!(full.admit(KITTY_PRESENT_MAX_BYTES_PER_FRAME));
        assert_eq!(full.blits(), 1);
        assert!(!full.admit(1));
        // A single blit larger than the whole cap never fits.
        let mut huge = KittyFrameBudget::new();
        assert!(!huge.admit(KITTY_PRESENT_MAX_BYTES_PER_FRAME + 1));
        assert_eq!(huge.blits(), 0);
        assert_eq!(huge.used_bytes(), 0);
        // Count cap over 128 pathological candidates: 32 paint, rest shed.
        // Mechanical adaptation: 128 mirrors the Core placement policy cap
        // (authority lives in Core); expectations are unchanged.
        let mut many = KittyFrameBudget::new();
        let mut admitted = 0;
        for _ in 0..128 {
            if many.admit(4) {
                admitted += 1;
            }
        }
        assert_eq!(admitted, KITTY_PRESENT_MAX_BLITS_PER_FRAME);
        assert_eq!(many.blits(), KITTY_PRESENT_MAX_BLITS_PER_FRAME);
        assert_eq!(KITTY_PRESENT_MAX_BLITS_PER_FRAME, 32);
        assert_eq!(KITTY_PRESENT_MAX_BYTES_PER_FRAME, 64 * 1024 * 1024);
    }

    fn raster_key_fixture() -> KittyRasterKey {
        KittyRasterKey {
            placement: 7,
            image: 3,
            rect: RectPx::new(0, 0, 2, 2),
            src_w: 2,
            src_h: 2,
            scrollback: 100,
            cell: METRICS,
            viewport_cols: 80,
            viewport_rows: 24,
        }
    }

    #[test]
    fn raster_cache_hits_without_rerasterizing() {
        let mut cache = KittyRasterCache::new();
        assert!(cache.is_empty());
        let key = raster_key_fixture();
        let mut calls = 0;
        let first = cache
            .get_or_rasterize(key, || {
                calls += 1;
                Some(vec![1, 2, 3, 4])
            })
            .unwrap();
        let second = cache
            .get_or_rasterize(key, || {
                calls += 1;
                Some(vec![9, 9, 9, 9])
            })
            .unwrap();
        assert_eq!(first, vec![1, 2, 3, 4]);
        assert_eq!(second, vec![1, 2, 3, 4]);
        assert_eq!(calls, 1, "second identical frame must not re-rasterize");
        assert_eq!(
            cache.stats(),
            KittyRasterStats {
                hits: 1,
                misses: 1,
                entries: 1,
                bytes: 4,
            }
        );
        assert_eq!(cache.get(&key).unwrap(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn raster_cache_misses_on_scroll_geometry_and_identity_change() {
        let mut cache = KittyRasterCache::new();
        let base = raster_key_fixture();
        let mut calls = 0;
        let mut raster = |cache: &mut KittyRasterCache, key: KittyRasterKey| {
            calls += 1;
            cache
                .get_or_rasterize(key, || Some(vec![calls as u8; 4]))
                .unwrap()
        };
        let first = raster(&mut cache, base);
        // Scroll sequence change (content moved): miss, fresh bytes.
        let scrolled = KittyRasterKey {
            scrollback: 103,
            ..base
        };
        let second = raster(&mut cache, scrolled);
        assert_ne!(first, second);
        // Geometry change (cell metrics): miss.
        let resized = KittyRasterKey {
            cell: CellMetrics {
                width: 9,
                height: 19,
            },
            ..base
        };
        raster(&mut cache, resized);
        // Viewport change: miss.
        let reflowed = KittyRasterKey {
            viewport_cols: 100,
            ..base
        };
        raster(&mut cache, reflowed);
        // Identity change (another placement, same geometry): miss.
        let other = KittyRasterKey {
            placement: 8,
            ..base
        };
        raster(&mut cache, other);
        assert_eq!(calls, 5);
        assert_eq!(cache.hits(), 0);
        assert_eq!(cache.misses(), 5);
        assert_eq!(cache.len(), 5);
        // The original key still hits with its original bytes (no stale).
        let again = cache.get(&base).unwrap();
        assert_eq!(again, first);
    }

    #[test]
    fn raster_cache_failures_are_not_cached() {
        let mut cache = KittyRasterCache::new();
        let key = raster_key_fixture();
        let mut calls = 0;
        for _ in 0..2 {
            assert_eq!(
                cache.get_or_rasterize(key, || {
                    calls += 1;
                    None
                }),
                None
            );
        }
        assert_eq!(calls, 2, "failures must re-run, never poison the key");
        assert_eq!(cache.misses(), 2);
        assert!(cache.is_empty());
        // Recovery caches normally afterwards.
        assert_eq!(
            cache.get_or_rasterize(key, || Some(vec![5; 4])),
            Some(vec![5; 4])
        );
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn raster_cache_evicts_oldest_within_caps() {
        let mut cache = KittyRasterCache::new();
        let base = raster_key_fixture();
        let total = KITTY_RASTER_CACHE_MAX_ENTRIES + 5;
        for i in 0..total {
            let key = KittyRasterKey {
                placement: 1000 + i as u64,
                ..base
            };
            cache
                .get_or_rasterize(key, || Some(vec![i as u8; 16]))
                .unwrap();
        }
        assert_eq!(cache.len(), KITTY_RASTER_CACHE_MAX_ENTRIES);
        assert!(cache.bytes() <= KITTY_RASTER_CACHE_MAX_BYTES);
        // Oldest five aged out; the newest survived.
        let first = KittyRasterKey {
            placement: 1000,
            ..base
        };
        assert_eq!(cache.get(&first), None);
        let last = KittyRasterKey {
            placement: 1000 + total as u64 - 1,
            ..base
        };
        assert!(cache.get(&last).is_some());
    }

    #[test]
    fn raster_cache_clear_drops_entries_keeps_counters() {
        let mut cache = KittyRasterCache::new();
        cache
            .get_or_rasterize(raster_key_fixture(), || Some(vec![1; 8]))
            .unwrap();
        assert_eq!(cache.len(), 1);
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
        assert_eq!(cache.misses(), 1, "counters survive clear");
        assert_eq!(cache.hits(), 0);
    }
}
