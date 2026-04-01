//! Glyph atlas — rectangle packing with LRU eviction.
//!
//! Dual atlas: `R8Unorm` for grayscale text, `Rgba8UnormSrgb` for color emoji.
//! Uses `etagere::BucketedAtlasAllocator` for packing. LRU eviction when full.

use etagere::{AllocId, BucketedAtlasAllocator, Size};
use std::collections::HashSet;

/// Initial atlas texture size.
const INITIAL_SIZE: u32 = 256;

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
    fn in_use_prevents_eviction() {
        let mut atlas = AtlasPlane::with_size(32, 32);

        for i in 0..4 {
            atlas.insert(key(i), 16, 16, P_DEFAULT);
        }

        // Mark all as in-use.
        for i in 0..4 {
            atlas.mark_in_use(&key(i));
        }

        // Should fail — can't evict anything.
        let region = atlas.insert(key(100), 16, 16, P_DEFAULT);
        assert!(region.is_none(), "all in-use, can't evict");

        // Clear in-use, now eviction works.
        atlas.clear_in_use();
        let region = atlas.insert(key(100), 16, 16, P_DEFAULT);
        assert!(region.is_some(), "after clearing in-use, eviction works");
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
    fn oversized_glyph_returns_none() {
        let mut atlas = AtlasPlane::with_size(32, 32);
        // Glyph larger than the atlas.
        let region = atlas.insert(key(1), 64, 64, GlyphPlacement { top: 60, left: 0 });
        assert!(region.is_none());
    }
}
