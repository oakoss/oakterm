//! Visual bell: a brief background flash on the pane that rang.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long one ring keeps a pane flashed (idea 36's `visual_duration`).
pub(crate) const FLASH_DURATION: Duration = Duration::from_millis(100);

/// Share of the contrast color mixed into the pane background.
const FLASH_MIX: u16 = 64; // of 255, about 25%

/// Panes currently flashing, each with the instant its flash ends.
#[derive(Debug, Default)]
pub(crate) struct BellFlash {
    until: HashMap<u32, Instant>,
}

impl BellFlash {
    pub(crate) fn ring(&mut self, pane_id: u32, now: Instant) {
        self.until.insert(pane_id, now + FLASH_DURATION);
    }

    #[must_use]
    pub(crate) fn flashing(&self, now: Instant) -> Vec<u32> {
        let mut panes: Vec<u32> = self
            .until
            .iter()
            .filter(|&(_, &end)| now < end)
            .map(|(&pane_id, _)| pane_id)
            .collect();
        panes.sort_unstable();
        panes
    }

    /// Drop flashes that have ended; true when any did, so the caller
    /// repaints to clear them.
    pub(crate) fn expire(&mut self, now: Instant) -> bool {
        let before = self.until.len();
        self.until.retain(|_, end| now < *end);
        self.until.len() != before
    }

    #[must_use]
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.until.values().copied().min()
    }
}

#[must_use]
pub(crate) fn target(rang: Option<u32>, focused: u32) -> u32 {
    rang.unwrap_or(focused)
}

/// [`flash_rgb`] over a cell color packed by `render_grid::pack_bg_color`
/// (`0xAABBGGRR`), keeping its alpha.
#[must_use]
pub(crate) fn flash_packed(packed: u32) -> u32 {
    let [r, g, b, a] = packed.to_le_bytes();
    let [r, g, b] = flash_rgb([r, g, b]);
    u32::from_le_bytes([r, g, b, a])
}

/// The flash color for a background: lightened when dark, darkened when
/// light, so it reads against any theme.
#[must_use]
pub(crate) fn flash_rgb(bg: [u8; 3]) -> [u8; 3] {
    let luma = (299 * u32::from(bg[0]) + 587 * u32::from(bg[1]) + 114 * u32::from(bg[2])) / 1000;
    let target: u16 = if luma < 128 { 255 } else { 0 };
    bg.map(|c| {
        let c = u16::from(c);
        let mixed = (c * (255 - FLASH_MIX) + target * FLASH_MIX) / 255;
        u8::try_from(mixed).unwrap_or(u8::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::{BellFlash, FLASH_DURATION, flash_packed, flash_rgb, target};
    use crate::render_grid::pack_bg_color;
    use std::time::{Duration, Instant};

    #[test]
    fn ring_flashes_only_that_pane_until_the_duration_ends() {
        let now = Instant::now();
        let mut flash = BellFlash::default();
        flash.ring(3, now);
        assert_eq!(flash.flashing(now), vec![3]);
        assert_eq!(flash.flashing(now + Duration::from_millis(99)), vec![3]);
        assert_eq!(flash.flashing(now + FLASH_DURATION), Vec::<u32>::new());
    }

    #[test]
    fn a_second_ring_restarts_the_flash() {
        let now = Instant::now();
        let mut flash = BellFlash::default();
        flash.ring(1, now);
        let later = now + FLASH_DURATION / 2;
        flash.ring(1, later);
        assert_eq!(flash.flashing(now + FLASH_DURATION), vec![1]);
        assert_eq!(flash.next_deadline(), Some(later + FLASH_DURATION));
    }

    #[test]
    fn expire_reports_only_when_a_flash_ended() {
        let now = Instant::now();
        let mut flash = BellFlash::default();
        assert!(!flash.expire(now));
        flash.ring(1, now);
        flash.ring(2, now + Duration::from_millis(50));
        assert_eq!(flash.next_deadline(), Some(now + FLASH_DURATION));
        assert!(!flash.expire(now));
        assert!(flash.expire(now + FLASH_DURATION));
        assert_eq!(flash.flashing(now + FLASH_DURATION), vec![2]);
        assert_eq!(
            flash.next_deadline(),
            Some(now + Duration::from_millis(50) + FLASH_DURATION)
        );
        assert!(flash.expire(now + Duration::from_secs(1)));
        assert_eq!(flash.next_deadline(), None);
    }

    #[test]
    fn flash_lightens_dark_backgrounds_and_darkens_light_ones() {
        assert_eq!(flash_rgb([0, 0, 0]), [64, 64, 64]);
        assert_eq!(flash_rgb([255, 255, 255]), [191, 191, 191]);
        let dark = [30, 30, 46];
        let lit = flash_rgb(dark);
        assert!(lit.iter().zip(dark).all(|(f, b)| *f > b));
        let light = [239, 241, 245];
        let dimmed = flash_rgb(light);
        assert!(dimmed.iter().zip(light).all(|(f, b)| *f < b));
        assert!(flash_rgb([127, 127, 127])[0] > 127);
        assert!(flash_rgb([128, 128, 128])[0] < 128);
    }

    #[test]
    fn a_bell_flashes_the_pane_that_rang_else_the_focused_one() {
        assert_eq!(target(Some(7), 3), 7);
        assert_eq!(target(None, 3), 3);
    }

    #[test]
    fn flash_packed_matches_flash_rgb_on_packed_colors() {
        let rgb = [30, 140, 220];
        assert_eq!(
            flash_packed(pack_bg_color(rgb)),
            pack_bg_color(flash_rgb(rgb))
        );
    }
}
