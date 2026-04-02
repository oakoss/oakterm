//! Glyph atlas — rectangle packing with LRU eviction.
//!
//! Dual atlas: `R8Unorm` for grayscale text, `Rgba8UnormSrgb` for color emoji.
//! Uses `etagere::BucketedAtlasAllocator` for packing. LRU eviction when full.

use etagere::{AllocId, BucketedAtlasAllocator, Size};
use std::collections::HashSet;

/// Initial atlas texture size.
const INITIAL_SIZE: u32 = 512;

/// Maximum atlas dimension (capped to avoid unbounded VRAM growth).
const MAX_SIZE: u32 = 4096;

/// A region allocated in the atlas texture.
#[derive(Debug, Clone, Copy)]
pub struct AtlasRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Glyph bearing offsets, carried from the rasterizer for positioning.
    pub placement: crate::shaper::GlyphPlacement,
    alloc_id: AllocId,
}

/// Key for looking up a cached glyph in the atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphCacheKey {
    pub font_id: u32,
    pub glyph_id: u32,
    pub size_tenths: u32, // font size * 10, to avoid floating point in keys
}

/// A single atlas plane (one texture).
pub struct AtlasPlane {
    allocator: BucketedAtlasAllocator,
    width: u32,
    height: u32,
    lru: lru::LruCache<GlyphCacheKey, AtlasRegion>,
    in_use: HashSet<GlyphCacheKey>,
}

impl AtlasPlane {
    /// Create a new atlas plane with the initial size.
    #[must_use]
    pub fn new() -> Self {
        Self::with_size(INITIAL_SIZE, INITIAL_SIZE)
    }

    /// Create a new atlas plane with a specific size.
    #[must_use]
    pub fn with_size(width: u32, height: u32) -> Self {
        let allocator = BucketedAtlasAllocator::new(Size::new(
            width.try_into().unwrap_or(i32::MAX),
            height.try_into().unwrap_or(i32::MAX),
        ));
        Self {
            allocator,
            width,
            height,
            // Unbounded: etagere is the real capacity constraint, not the LRU.
            lru: lru::LruCache::unbounded(),
            in_use: HashSet::new(),
        }
    }

    /// Look up a cached glyph. Promotes to most-recently-used in O(1).
    pub fn get(&mut self, key: &GlyphCacheKey) -> Option<AtlasRegion> {
        self.lru.get(key).copied()
    }

    /// Allocate space for a glyph and insert it into the cache.
    /// Returns `None` if the atlas is full and eviction couldn't free space.
    #[allow(clippy::cast_sign_loss)] // etagere coords are always non-negative
    pub fn insert(
        &mut self,
        key: GlyphCacheKey,
        width: u32,
        height: u32,
        placement: crate::shaper::GlyphPlacement,
    ) -> Option<AtlasRegion> {
        // Try direct allocation.
        if let Some(region) = self.try_allocate(width, height) {
            let atlas_region = AtlasRegion {
                x: region.rectangle.min.x as u32,
                y: region.rectangle.min.y as u32,
                width,
                height,
                placement,
                alloc_id: region.id,
            };
            self.lru.push(key, atlas_region);
            return Some(atlas_region);
        }

        // Evict LRU entries until we have space.
        let mut skipped = Vec::new();
        while let Some((evict_key, evict_region)) = self.lru.pop_lru() {
            if self.in_use.contains(&evict_key) {
                skipped.push((evict_key, evict_region));
                continue;
            }

            self.allocator.deallocate(evict_region.alloc_id);

            if let Some(region) = self.try_allocate(width, height) {
                // Re-insert skipped in-use entries.
                for (k, v) in skipped {
                    self.lru.push(k, v);
                }
                let atlas_region = AtlasRegion {
                    x: region.rectangle.min.x as u32,
                    y: region.rectangle.min.y as u32,
                    width,
                    height,
                    placement,
                    alloc_id: region.id,
                };
                self.lru.push(key, atlas_region);
                return Some(atlas_region);
            }
        }

        // All entries were in-use or eviction didn't free enough space.
        // Re-insert skipped entries.
        for (k, v) in skipped {
            self.lru.push(k, v);
        }

        // Grow the atlas and retry.
        if let Some(region) = self.grow_and_retry(key, width, height, placement) {
            return Some(region);
        }
        None
    }

    /// Mark a glyph as in-use for the current frame (prevents eviction).
    pub fn mark_in_use(&mut self, key: &GlyphCacheKey) {
        self.in_use.insert(*key);
    }

    /// Clear the in-use set at the end of each frame.
    pub fn clear_in_use(&mut self) {
        self.in_use.clear();
    }

    /// Number of cached glyphs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lru.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lru.is_empty()
    }

    /// Atlas texture dimensions.
    #[must_use]
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Clear all cached glyphs (e.g., on font size change or DPI change).
    pub fn clear(&mut self) {
        self.lru.clear();
        self.in_use.clear();
        self.allocator = BucketedAtlasAllocator::new(Size::new(
            self.width.try_into().unwrap_or(i32::MAX),
            self.height.try_into().unwrap_or(i32::MAX),
        ));
    }

    /// Double the atlas dimensions (up to `MAX_SIZE`) and retry allocation.
    /// `etagere::grow()` preserves all existing allocations.
    #[allow(clippy::cast_sign_loss)]
    fn grow_and_retry(
        &mut self,
        key: GlyphCacheKey,
        width: u32,
        height: u32,
        placement: crate::shaper::GlyphPlacement,
    ) -> Option<AtlasRegion> {
        loop {
            let new_w = (self.width * 2).min(MAX_SIZE);
            let new_h = (self.height * 2).min(MAX_SIZE);
            if new_w == self.width && new_h == self.height {
                return None; // Already at max size.
            }
            self.allocator.grow(Size::new(
                new_w.try_into().unwrap_or(i32::MAX),
                new_h.try_into().unwrap_or(i32::MAX),
            ));
            self.width = new_w;
            self.height = new_h;

            if let Some(region) = self.try_allocate(width, height) {
                let atlas_region = AtlasRegion {
                    x: region.rectangle.min.x as u32,
                    y: region.rectangle.min.y as u32,
                    width,
                    height,
                    placement,
                    alloc_id: region.id,
                };
                self.lru.push(key, atlas_region);
                return Some(atlas_region);
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)] // atlas coords fit in u32
    fn try_allocate(&mut self, width: u32, height: u32) -> Option<etagere::Allocation> {
        let w: i32 = width.try_into().ok()?;
        let h: i32 = height.try_into().ok()?;
        self.allocator.allocate(Size::new(w, h))
    }
}

impl Default for AtlasPlane {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shaper::GlyphPlacement;

    fn key(glyph_id: u32) -> GlyphCacheKey {
        GlyphCacheKey {
            font_id: 0,
            glyph_id,
            size_tenths: 140,
        }
    }

    const P_DEFAULT: GlyphPlacement = GlyphPlacement { top: 14, left: 0 };

    #[test]
    fn allocate_and_lookup() {
        let mut atlas = AtlasPlane::new();
        let p = GlyphPlacement { top: 18, left: 1 };
        let region = atlas.insert(key(65), 10, 20, p).expect("should allocate");
        assert_eq!(region.width, 10);
        assert_eq!(region.height, 20);
        assert_eq!(region.placement.top, 18);
        assert_eq!(region.placement.left, 1);

        let cached = atlas.get(&key(65)).expect("should be cached");
        assert_eq!(cached.x, region.x);
        assert_eq!(cached.y, region.y);
        assert_eq!(cached.placement.top, 18);
        assert_eq!(cached.placement.left, 1);
    }

    #[test]
    fn miss_returns_none() {
        let mut atlas = AtlasPlane::new();
        assert!(atlas.get(&key(999)).is_none());
    }

    #[test]
    fn eviction_frees_space() {
        // Small atlas that can only fit a few glyphs.
        let mut atlas = AtlasPlane::with_size(32, 32);

        // Fill the atlas.
        for i in 0..4 {
            atlas.insert(key(i), 16, 16, P_DEFAULT);
        }
        assert_eq!(atlas.len(), 4);

        // Next allocation should evict the LRU entry.
        let region = atlas.insert(key(100), 16, 16, P_DEFAULT);
        assert!(region.is_some(), "should evict and allocate");
        assert!(atlas.get(&key(0)).is_none(), "oldest should be evicted");
    }

    #[test]
    fn in_use_grows_atlas() {
        let mut atlas = AtlasPlane::with_size(32, 32);

        for i in 0..4 {
            atlas.insert(key(i), 16, 16, P_DEFAULT);
        }

        // Mark all as in-use.
        for i in 0..4 {
            atlas.mark_in_use(&key(i));
        }

        // Should grow the atlas since eviction can't free in-use glyphs.
        let region = atlas.insert(key(100), 16, 16, P_DEFAULT);
        assert!(region.is_some(), "should grow atlas when all in-use");
        assert_eq!(atlas.size(), (64, 64));

        // Clear in-use, eviction still works at the new size.
        atlas.clear_in_use();
        let region = atlas.insert(key(200), 16, 16, P_DEFAULT);
        assert!(region.is_some(), "allocation works at new size");
    }

    #[test]
    fn clear_resets_atlas() {
        let mut atlas = AtlasPlane::new();
        atlas.insert(key(1), 10, 10, GlyphPlacement { top: 8, left: 0 });
        atlas.insert(key(2), 10, 10, GlyphPlacement { top: 8, left: 0 });
        assert_eq!(atlas.len(), 2);

        atlas.clear();
        assert!(atlas.is_empty());
        assert!(atlas.get(&key(1)).is_none());
    }

    #[test]
    fn oversized_glyph_grows_to_fit() {
        let mut atlas = AtlasPlane::with_size(32, 32);
        // Glyph larger than initial atlas — should grow to accommodate.
        let region = atlas.insert(key(1), 64, 64, GlyphPlacement { top: 60, left: 0 });
        assert!(region.is_some(), "should grow atlas to fit large glyph");
        assert!(atlas.size().0 >= 64);
    }

    #[test]
    fn truly_oversized_glyph_returns_none() {
        let mut atlas = AtlasPlane::with_size(32, 32);
        // Glyph larger than MAX_SIZE — can't grow enough.
        let region = atlas.insert(
            key(1),
            MAX_SIZE + 1,
            MAX_SIZE + 1,
            GlyphPlacement { top: 0, left: 0 },
        );
        assert!(region.is_none());
    }

    #[test]
    fn grow_on_full() {
        let mut atlas = AtlasPlane::with_size(32, 32);
        // Fill atlas and mark all in-use so eviction can't free space.
        for i in 0..4 {
            atlas.insert(key(i), 16, 16, P_DEFAULT);
            atlas.mark_in_use(&key(i));
        }
        // Insert should grow the atlas instead of failing.
        let region = atlas.insert(key(100), 16, 16, P_DEFAULT);
        assert!(region.is_some(), "should grow atlas and allocate");
        assert_eq!(atlas.size(), (64, 64));
    }

    #[test]
    fn grow_capped_at_max() {
        let mut atlas = AtlasPlane::with_size(MAX_SIZE, MAX_SIZE);
        // Fill and mark in-use.
        let region = atlas.insert(key(0), 16, 16, P_DEFAULT);
        assert!(region.is_some());
        atlas.mark_in_use(&key(0));
        // Fill remaining space with a huge glyph that won't fit.
        let region = atlas.insert(key(1), MAX_SIZE, MAX_SIZE, P_DEFAULT);
        assert!(region.is_none(), "can't grow past max");
    }

    #[test]
    fn existing_allocations_valid_after_grow() {
        let mut atlas = AtlasPlane::with_size(32, 32);
        let r1 = atlas.insert(key(1), 16, 16, P_DEFAULT).unwrap();
        atlas.mark_in_use(&key(1));
        // Force growth by filling and inserting when all in-use.
        for i in 2..=4 {
            atlas.insert(key(i), 16, 16, P_DEFAULT);
            atlas.mark_in_use(&key(i));
        }
        let _new = atlas.insert(key(100), 16, 16, P_DEFAULT).unwrap();
        // Original allocation should still be accessible with same coords.
        let cached = atlas.get(&key(1)).unwrap();
        assert_eq!(cached.x, r1.x);
        assert_eq!(cached.y, r1.y);
    }
}
