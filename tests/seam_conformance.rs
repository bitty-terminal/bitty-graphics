//! Seam conformance suite for graphics#14 (CTX-0007).
//!
//! Pins the public contract a Core-owned trait binds to. Every test
//! exercises only the public surface (`bitty_graphics::...` re-exports),
//! the way a host would call it: if any test here fails, the seam drifted
//! and the Core-side rewire cannot trust the documented signatures, caps,
//! or error mapping.

use bitty_graphics::{
    CellMetrics, ExtentPx, KITTY_DECODE_MAX_BYTES, KITTY_DECODE_MAX_DIMENSION,
    KITTY_DECODE_MAX_PIXELS, KITTY_FORMAT_PNG, KITTY_FORMAT_RGB, KITTY_FORMAT_RGBA,
    KITTY_PRESENT_MAX_BLITS_PER_FRAME, KITTY_PRESENT_MAX_BYTES_PER_FRAME,
    KITTY_RASTER_CACHE_MAX_BYTES, KITTY_RASTER_CACHE_MAX_ENTRIES, KittyDecodeError,
    KittyDecodedImage, KittyFrameBudget, KittyImageId, KittyPlacedImage, KittyRasterCache,
    KittyRasterKey, KittyRasterStats, KittyTransmitFormat, RectPx, decode_kitty_payload,
    decode_kitty_payload_owned, rasterize, rasterize_clipped,
};

/// 1x1 RGBA PNG, single red opaque pixel (same bytes as the unit fixture).
const PNG_1X1_RGBA_RED: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 218, 99, 248, 207, 192, 240, 31, 0,
    5, 0, 1, 255, 86, 199, 47, 13, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

const METRICS: CellMetrics = CellMetrics {
    width: 8,
    height: 16,
};

fn raster_key(placement: u64) -> KittyRasterKey {
    KittyRasterKey {
        placement,
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

fn red_2x2_image() -> KittyPlacedImage {
    KittyPlacedImage {
        id: KittyImageId(1),
        width: 2,
        height: 2,
        rgba: [0xFF, 0x00, 0x00, 0xFF].repeat(4),
        compressed_len: 16,
    }
}

#[test]
fn caps_are_frozen() {
    assert_eq!(KITTY_DECODE_MAX_DIMENSION, 8192);
    assert_eq!(KITTY_DECODE_MAX_PIXELS, 4096 * 4096);
    assert_eq!(KITTY_DECODE_MAX_BYTES, 64 * 1024 * 1024);
    assert_eq!(KITTY_PRESENT_MAX_BLITS_PER_FRAME, 32);
    assert_eq!(KITTY_PRESENT_MAX_BYTES_PER_FRAME, 64 * 1024 * 1024);
    assert_eq!(KITTY_RASTER_CACHE_MAX_ENTRIES, 128);
    assert_eq!(KITTY_RASTER_CACHE_MAX_BYTES, 64 * 1024 * 1024);
    assert_eq!(KITTY_FORMAT_PNG, 100);
    assert_eq!(KITTY_FORMAT_RGB, 24);
    assert_eq!(KITTY_FORMAT_RGBA, 32);
    // Relationships the trait relies on: the area cap at 4 bytes per pixel
    // is exactly the byte cap, and a lone max image fits the frame budget.
    assert_eq!(KITTY_DECODE_MAX_PIXELS * 4, KITTY_DECODE_MAX_BYTES as u64);
    assert_eq!(KITTY_PRESENT_MAX_BYTES_PER_FRAME, KITTY_DECODE_MAX_BYTES);
}

#[test]
fn format_mapping_is_exact() {
    assert_eq!(
        KittyTransmitFormat::from_f(100),
        Some(KittyTransmitFormat::Png)
    );
    assert_eq!(
        KittyTransmitFormat::from_f(24),
        Some(KittyTransmitFormat::Rgb)
    );
    assert_eq!(
        KittyTransmitFormat::from_f(32),
        Some(KittyTransmitFormat::Rgba)
    );
    // Unknown `f` never reaches the seam: the host must reject before
    // calling (Core's parallel copy instead returns `MalformedPng`).
    for unsupported in [0, 1, 23, 25, 31, 33, 99, 101, 255, u32::MAX] {
        assert_eq!(
            KittyTransmitFormat::from_f(unsupported),
            None,
            "f={unsupported}"
        );
    }
    assert_eq!(KittyTransmitFormat::Png.f_value(), 100);
    assert_eq!(KittyTransmitFormat::Rgb.f_value(), 24);
    assert_eq!(KittyTransmitFormat::Rgba.f_value(), 32);
    assert_eq!(KittyTransmitFormat::Png.raw_channels(), None);
    assert_eq!(KittyTransmitFormat::Rgb.raw_channels(), Some(3));
    assert_eq!(KittyTransmitFormat::Rgba.raw_channels(), Some(4));
}

#[test]
fn empty_payload_rejected_first() {
    assert_eq!(
        decode_kitty_payload(KittyTransmitFormat::Png, None, None, &[]),
        Err(KittyDecodeError::EmptyPayload)
    );
    assert_eq!(
        decode_kitty_payload(KittyTransmitFormat::Rgb, Some(1), Some(1), &[]),
        Err(KittyDecodeError::EmptyPayload)
    );
    assert_eq!(
        decode_kitty_payload_owned(
            KittyTransmitFormat::Png,
            None,
            None,
            Vec::new().into_boxed_slice()
        ),
        Err(KittyDecodeError::EmptyPayload)
    );
    assert_eq!(
        decode_kitty_payload_owned(
            KittyTransmitFormat::Rgba,
            Some(1),
            Some(1),
            Vec::new().into_boxed_slice()
        ),
        Err(KittyDecodeError::EmptyPayload)
    );
}

#[test]
fn raw_requires_both_dimensions() {
    let payload = [0_u8; 12];
    for format in [KittyTransmitFormat::Rgb, KittyTransmitFormat::Rgba] {
        assert_eq!(
            decode_kitty_payload(format, None, None, &payload),
            Err(KittyDecodeError::MissingDimensions)
        );
        assert_eq!(
            decode_kitty_payload(format, Some(2), None, &payload),
            Err(KittyDecodeError::MissingDimensions)
        );
        assert_eq!(
            decode_kitty_payload(format, None, Some(2), &payload),
            Err(KittyDecodeError::MissingDimensions)
        );
    }
}

#[test]
fn zero_dimension_rejected() {
    let payload = [0_u8; 4];
    for (w, h) in [(0, 1), (1, 0), (0, 0)] {
        assert_eq!(
            decode_kitty_payload(KittyTransmitFormat::Rgba, Some(w), Some(h), &payload),
            Err(KittyDecodeError::ZeroDimension),
            "{w}x{h}"
        );
    }
    assert_eq!(
        decode_kitty_payload_owned(
            KittyTransmitFormat::Rgba,
            Some(0),
            Some(1),
            vec![0_u8; 4].into_boxed_slice()
        ),
        Err(KittyDecodeError::ZeroDimension)
    );
}

#[test]
fn oversize_side_rejected_before_alloc() {
    // 100_000 x 100_000 would need tens of gigabytes; the side cap fires
    // on a 4-byte payload, proving bounds run before allocation.
    assert_eq!(
        decode_kitty_payload(
            KittyTransmitFormat::Rgb,
            Some(100_000),
            Some(100_000),
            &[0; 4]
        ),
        Err(KittyDecodeError::DimensionsTooLarge {
            width: 100_000,
            height: 100_000,
            cap: KITTY_DECODE_MAX_DIMENSION,
        })
    );
    // The boundary itself is admitted: 8192 x 1 RGBA decodes from an
    // exactly-sized payload, while 8193 x 1 is refused.
    let wide = vec![0x7F_u8; 8192 * 4];
    let img = decode_kitty_payload(KittyTransmitFormat::Rgba, Some(8192), Some(1), &wide).unwrap();
    assert_eq!(img.dimensions(), (8192, 1));
    assert_eq!(
        decode_kitty_payload(KittyTransmitFormat::Rgba, Some(8193), Some(1), &[0; 4]),
        Err(KittyDecodeError::DimensionsTooLarge {
            width: 8193,
            height: 1,
            cap: 8192,
        })
    );
    assert_eq!(
        decode_kitty_payload_owned(
            KittyTransmitFormat::Rgb,
            Some(9000),
            Some(1),
            vec![0_u8; 4].into_boxed_slice()
        ),
        Err(KittyDecodeError::DimensionsTooLarge {
            width: 9000,
            height: 1,
            cap: 8192,
        })
    );
}

#[test]
fn over_area_rejected_before_alloc() {
    // 5000 x 5000 = 25M px > 16_777_216 cap; the area check fires on an
    // 8-byte payload, before the length check could allocate.
    assert_eq!(
        decode_kitty_payload(KittyTransmitFormat::Rgba, Some(5000), Some(5000), &[0; 8]),
        Err(KittyDecodeError::TooManyPixels {
            pixels: 25_000_000,
            cap: KITTY_DECODE_MAX_PIXELS,
        })
    );
    // The area boundary itself passes the area check: 8192 x 2048 is
    // exactly 16_777_216 px, so a short payload falls through to the exact
    // length check with the pinned expected size (67_108_864 bytes).
    assert_eq!(
        decode_kitty_payload(KittyTransmitFormat::Rgba, Some(8192), Some(2048), &[0; 4]),
        Err(KittyDecodeError::LengthMismatch {
            expected: 67_108_864,
            actual: 4,
        })
    );
}

#[test]
fn length_mismatch_reports_exact_sizes() {
    // 2x1 RGB needs exactly 6 bytes.
    assert_eq!(
        decode_kitty_payload(KittyTransmitFormat::Rgb, Some(2), Some(1), &[0; 5]),
        Err(KittyDecodeError::LengthMismatch {
            expected: 6,
            actual: 5
        })
    );
    assert_eq!(
        decode_kitty_payload(KittyTransmitFormat::Rgb, Some(2), Some(1), &[0; 7]),
        Err(KittyDecodeError::LengthMismatch {
            expected: 6,
            actual: 7
        })
    );
}

#[test]
fn error_display_is_pinned() {
    assert_eq!(
        KittyDecodeError::EmptyPayload.to_string(),
        "kitty payload is empty"
    );
    assert_eq!(
        KittyDecodeError::MissingDimensions.to_string(),
        "kitty raw payload needs width and height (s/v)"
    );
    assert_eq!(
        KittyDecodeError::ZeroDimension.to_string(),
        "kitty image dimension is zero"
    );
    assert_eq!(
        KittyDecodeError::DimensionsTooLarge {
            width: 9000,
            height: 1,
            cap: 8192
        }
        .to_string(),
        "kitty image 9000x1 exceeds max dimension of 8192px"
    );
    assert_eq!(
        KittyDecodeError::TooManyPixels {
            pixels: 25_000_000,
            cap: 16_777_216
        }
        .to_string(),
        "kitty image of 25000000 pixels exceeds max of 16777216 pixels"
    );
    assert_eq!(
        KittyDecodeError::DecodedTooLarge {
            bytes: 100,
            cap: 64
        }
        .to_string(),
        "kitty decoded bitmap of 100 bytes exceeds max of 64 bytes"
    );
    assert_eq!(
        KittyDecodeError::LengthMismatch {
            expected: 6,
            actual: 5
        }
        .to_string(),
        "kitty raw payload of 5 bytes does not match 6 expected bytes"
    );
    let malformed = KittyDecodeError::MalformedPng("end of input".to_owned());
    assert_eq!(
        malformed.to_string(),
        "kitty PNG is malformed: end of input"
    );
}

#[test]
fn owned_matches_borrowed() {
    // RGB expansion is byte-identical across entry points.
    let raw_rgb = [10_u8, 20, 30, 40, 50, 60];
    let borrowed =
        decode_kitty_payload(KittyTransmitFormat::Rgb, Some(2), Some(1), &raw_rgb).unwrap();
    let owned = decode_kitty_payload_owned(
        KittyTransmitFormat::Rgb,
        Some(2),
        Some(1),
        raw_rgb.to_vec().into_boxed_slice(),
    )
    .unwrap();
    assert_eq!(borrowed, owned);
    assert_eq!(owned.rgba(), &[10, 20, 30, 0xFF, 40, 50, 60, 0xFF]);
    // RGBA passthrough is byte-identical across entry points.
    let raw_rgba = [1_u8, 2, 3, 4, 5, 6, 7, 8];
    let borrowed =
        decode_kitty_payload(KittyTransmitFormat::Rgba, Some(2), Some(1), &raw_rgba).unwrap();
    let owned = decode_kitty_payload_owned(
        KittyTransmitFormat::Rgba,
        Some(2),
        Some(1),
        raw_rgba.to_vec().into_boxed_slice(),
    )
    .unwrap();
    assert_eq!(borrowed, owned);
    assert_eq!(owned.rgba(), &raw_rgba);
    // PNG decodes identically through both entry points.
    let borrowed =
        decode_kitty_payload(KittyTransmitFormat::Png, None, None, PNG_1X1_RGBA_RED).unwrap();
    let owned = decode_kitty_payload_owned(
        KittyTransmitFormat::Png,
        None,
        None,
        PNG_1X1_RGBA_RED.to_vec().into_boxed_slice(),
    )
    .unwrap();
    assert_eq!(borrowed, owned);
}

#[test]
fn png_decodes_through_seam_and_fails_closed() {
    let img: KittyDecodedImage =
        decode_kitty_payload(KittyTransmitFormat::Png, None, None, PNG_1X1_RGBA_RED).unwrap();
    assert_eq!(img.dimensions(), (1, 1));
    assert_eq!(img.width(), 1);
    assert_eq!(img.height(), 1);
    assert_eq!(img.pixel_count(), 1);
    assert_eq!(img.rgba(), &[0xFF, 0x00, 0x00, 0xFF]);
    // PNG ignores supplied dimensions: IHDR governs.
    let stale = decode_kitty_payload(
        KittyTransmitFormat::Png,
        Some(99),
        Some(99),
        PNG_1X1_RGBA_RED,
    )
    .unwrap();
    assert_eq!(stale, img);
    // Garbage and truncation fail closed, deterministically.
    for garbage in [
        b"not a png at all".as_slice(),
        &[0_u8; 64],
        &PNG_1X1_RGBA_RED[..8],
        &PNG_1X1_RGBA_RED[..33],
    ] {
        let first = decode_kitty_payload(KittyTransmitFormat::Png, None, None, garbage);
        assert!(first.is_err(), "garbage decoded as Ok");
        assert_eq!(
            first,
            decode_kitty_payload(KittyTransmitFormat::Png, None, None, garbage)
        );
        assert!(
            matches!(first, Err(KittyDecodeError::MalformedPng(_))),
            "garbage must map to MalformedPng"
        );
    }
}

#[test]
fn raster_identity_and_nearest_neighbor_scale() {
    let image = red_2x2_image();
    // Matching extent is the identity.
    let out = rasterize(&image, RectPx::new(5, 5, 2, 2)).unwrap();
    assert_eq!(out, image.rgba);
    // 2x1 red-then-green scaled to 4x2 doubles each source pixel.
    let wide = KittyPlacedImage {
        id: KittyImageId(2),
        width: 2,
        height: 1,
        rgba: vec![0xFF, 0, 0, 0xFF, 0, 0xFF, 0, 0xFF],
        compressed_len: 8,
    };
    let out = rasterize(&wide, RectPx::new(0, 0, 4, 2)).unwrap();
    assert_eq!(out.len(), 4 * 2 * 4);
    assert_eq!(&out[0..8], &[0xFF, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF]);
    assert_eq!(&out[8..16], &[0, 0xFF, 0, 0xFF, 0, 0xFF, 0, 0xFF]);
    assert_eq!(&out[16..24], &[0xFF, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF]);
    assert_eq!(&out[24..32], &[0, 0xFF, 0, 0xFF, 0, 0xFF, 0, 0xFF]);
}

#[test]
fn raster_clipped_equals_crop_of_full() {
    // 4x4: top half red, bottom half blue. The bottom-right 2x2 window of
    // the 4x4 full extent must be all blue: only visible bytes allocated,
    // true scale kept, never a re-scaled whole image.
    let mut rgba = Vec::new();
    for y in 0..4 {
        let color = if y < 2 {
            [0xFF, 0, 0, 0xFF]
        } else {
            [0, 0, 0xFF, 0xFF]
        };
        for _ in 0..4 {
            rgba.extend_from_slice(&color);
        }
    }
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
    assert!(clipped.chunks_exact(4).all(|px| px == [0, 0, 0xFF, 0xFF]));
    // Identity (visible == full) matches the plain entry point.
    assert_eq!(
        rasterize_clipped(&image, full, full),
        rasterize(&image, full)
    );
}

#[test]
fn raster_fails_closed() {
    let image = red_2x2_image();
    // Empty rects paint nothing.
    assert_eq!(rasterize(&image, RectPx::new(0, 0, 0, 2)), None);
    assert_eq!(rasterize(&image, RectPx::new(0, 0, 2, 0)), None);
    let full = RectPx::new(0, 0, 2, 2);
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
    // Over-cap output is refused before allocation (9000x9000x4 = 324 MB).
    let big = RectPx::new(0, 0, 9000, 9000);
    assert_eq!(rasterize(&image, big), None);
    assert_eq!(rasterize_clipped(&image, big, big), None);
    // Corrupt carriers fail closed: short pixel buffer, zero dimensions.
    let short = KittyPlacedImage {
        rgba: vec![0xFF; 8],
        ..image.clone()
    };
    assert_eq!(rasterize(&short, full), None);
    let flat = KittyPlacedImage {
        width: 0,
        height: 0,
        rgba: Vec::new(),
        ..image.clone()
    };
    assert_eq!(rasterize(&flat, full), None);
}

#[test]
fn frame_budget_enforces_both_caps() {
    let fresh = KittyFrameBudget::new();
    assert_eq!(fresh.blits(), 0);
    assert_eq!(fresh.used_bytes(), 0);
    let mut budget = KittyFrameBudget::new();
    assert!(budget.admit(1024));
    assert!(budget.admit(2048));
    assert_eq!(budget.blits(), 2);
    assert_eq!(budget.used_bytes(), 3072);
    // Byte cap: exactly the cap admits once, one more byte sheds.
    let mut full = KittyFrameBudget::new();
    assert!(full.admit(KITTY_PRESENT_MAX_BYTES_PER_FRAME));
    assert_eq!(full.blits(), 1);
    assert_eq!(full.used_bytes(), KITTY_PRESENT_MAX_BYTES_PER_FRAME);
    assert!(!full.admit(1));
    // A single blit larger than the whole cap never fits.
    let mut huge = KittyFrameBudget::new();
    assert!(!huge.admit(KITTY_PRESENT_MAX_BYTES_PER_FRAME + 1));
    assert_eq!(huge.blits(), 0);
    assert_eq!(huge.used_bytes(), 0);
    // Count cap: 32 paint out of 128 pathological candidates, rest shed.
    let mut many = KittyFrameBudget::new();
    let mut admitted = 0;
    for _ in 0..128 {
        if many.admit(4) {
            admitted += 1;
        }
    }
    assert_eq!(admitted, KITTY_PRESENT_MAX_BLITS_PER_FRAME);
    assert_eq!(many.blits(), KITTY_PRESENT_MAX_BLITS_PER_FRAME);
}

#[test]
fn cache_serves_hits_without_rerasterizing() {
    let mut cache = KittyRasterCache::new();
    assert!(cache.is_empty());
    let key = raster_key(7);
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
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.bytes(), 4);
    assert_eq!(cache.hits(), 1);
    assert_eq!(cache.misses(), 1);
    assert_eq!(cache.get(&key).unwrap(), vec![1, 2, 3, 4]);
}

#[test]
fn cache_misses_on_context_change() {
    let mut cache = KittyRasterCache::new();
    let base = raster_key(7);
    let mut calls: u8 = 0;
    let mut fill = |cache: &mut KittyRasterCache, key: KittyRasterKey| {
        calls += 1;
        cache
            .get_or_rasterize(key, || Some(vec![calls; 4]))
            .unwrap()
    };
    let first = fill(&mut cache, base);
    // Every key input shapes the output: scroll, geometry, viewport, and
    // identity changes all miss with fresh bytes instead of stale pixels.
    let variants = [
        KittyRasterKey {
            scrollback: 103,
            ..base
        },
        KittyRasterKey {
            cell: CellMetrics {
                width: 9,
                height: 19,
            },
            ..base
        },
        KittyRasterKey {
            viewport_cols: 100,
            ..base
        },
        KittyRasterKey {
            viewport_rows: 40,
            ..base
        },
        KittyRasterKey {
            placement: 8,
            ..base
        },
        KittyRasterKey { image: 4, ..base },
        KittyRasterKey {
            rect: RectPx::new(1, 0, 2, 2),
            ..base
        },
        KittyRasterKey { src_w: 4, ..base },
    ];
    for key in variants {
        let bytes = fill(&mut cache, key);
        assert_ne!(bytes, first, "changed context must not serve stale bytes");
    }
    assert_eq!(cache.hits(), 0);
    assert_eq!(cache.misses(), 9);
    assert_eq!(cache.len(), 9);
    // The original key still hits with its original bytes.
    assert_eq!(cache.get(&base).unwrap(), first);
}

#[test]
fn cache_never_caches_failures() {
    let mut cache = KittyRasterCache::new();
    let key = raster_key(7);
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
    assert_eq!(cache.get(&key), None);
    // Recovery caches normally afterwards.
    assert_eq!(
        cache.get_or_rasterize(key, || Some(vec![5; 4])),
        Some(vec![5; 4])
    );
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.misses(), 3);
}

#[test]
fn cache_evicts_oldest_within_caps() {
    let mut cache = KittyRasterCache::new();
    let total = KITTY_RASTER_CACHE_MAX_ENTRIES + 5;
    for i in 0..total {
        cache
            .get_or_rasterize(raster_key(1000 + i as u64), || Some(vec![i as u8; 16]))
            .unwrap();
    }
    assert_eq!(cache.len(), KITTY_RASTER_CACHE_MAX_ENTRIES);
    assert_eq!(cache.bytes(), KITTY_RASTER_CACHE_MAX_ENTRIES * 16);
    assert!(cache.bytes() <= KITTY_RASTER_CACHE_MAX_BYTES);
    assert_eq!(cache.get(&raster_key(1000)), None);
    assert!(cache.get(&raster_key(1000 + total as u64 - 1)).is_some());
}

#[test]
fn cache_clear_keeps_counters() {
    let mut cache = KittyRasterCache::new();
    cache
        .get_or_rasterize(raster_key(7), || Some(vec![1; 8]))
        .unwrap();
    assert_eq!(cache.len(), 1);
    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(cache.bytes(), 0);
    assert_eq!(cache.misses(), 1, "counters survive clear");
    assert_eq!(cache.hits(), 0);
}

#[test]
fn end_to_end_decode_then_rasterize() {
    // The host round trip through the seam: decode raw RGBA, store the
    // carrier, rasterize the identity rect, get the decoded bytes back.
    let payload = [0x11_u8, 0x22, 0x33, 0xFF, 0x44, 0x55, 0x66, 0xFF];
    let decoded: KittyDecodedImage =
        decode_kitty_payload(KittyTransmitFormat::Rgba, Some(2), Some(1), &payload).unwrap();
    assert_eq!(decoded.dimensions(), (2, 1));
    assert_eq!(decoded.pixel_count(), 2);
    let image = KittyPlacedImage {
        id: KittyImageId(9),
        width: decoded.width(),
        height: decoded.height(),
        rgba: decoded.into_rgba(),
        compressed_len: payload.len(),
    };
    assert_eq!(image.id.as_u64(), 9);
    let rect = RectPx::new(0, 0, 2, 1);
    let blit = rasterize(&image, rect).unwrap();
    assert_eq!(blit, payload);
    assert_eq!(rasterize_clipped(&image, rect, rect).unwrap(), blit);
}

#[test]
fn geometry_carriers_are_stable() {
    assert_eq!(CellMetrics::new(0, 16), None);
    assert_eq!(CellMetrics::new(8, 0), None);
    let metrics = CellMetrics::new(8, 16).unwrap();
    assert_eq!(metrics.extent_for(80, 24), ExtentPx::new(640, 384));
    let rect = RectPx::new(-3, 7, 640, 384);
    assert_eq!((rect.x, rect.y, rect.width, rect.height), (-3, 7, 640, 384));
    assert!(ExtentPx::new(0, 10).is_zero());
    assert!(!ExtentPx::new(640, 384).is_zero());
}
