//! Animation frames and terminal driven playback.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::ImageData;

/// Default gap between frames in milliseconds.
pub const DEFAULT_GAP: i32 = 40;

/// If playback falls behind by more than this, it restarts timing from now instead of
/// rapidly skipping through frames to catch up.
const MAX_LAG: Duration = Duration::from_secs(1);

/// A single animation frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Fully composed frame pixels.
    pub data: Arc<ImageData>,
    /// Time in milliseconds until the next frame; non-positive frames are gapless.
    pub gap: i32,
}

/// Animation playback state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AnimationState {
    #[default]
    Stopped,
    /// Running, but waiting for more frames at the last frame instead of looping.
    Loading,
    Running,
}

/// Animation state of an image.
#[derive(Debug, Default, Clone)]
pub struct Animation {
    pub state: AnimationState,
    /// Loop setting as sent by the client: 0 unset, 1 infinite, `n` loops `n - 1` times.
    pub loops: u32,
    /// Completed loops.
    pub loops_done: u32,
    /// When the current frame was shown.
    pub shown_at: Option<Instant>,
}

/// Composition mode for blending pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Composition {
    AlphaBlend,
    Overwrite,
}

/// RGBA canvas used for frame composition.
pub struct Canvas<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a mut [u8],
}

/// Source pixels for composition.
pub struct Source<'a> {
    pub width: u32,
    pub height: u32,
    pub pixels: &'a [u8],
}

/// Rectangle for composition, in pixels.
#[derive(Debug, Clone, Copy)]
pub struct CompositionRect {
    pub src_x: u32,
    pub src_y: u32,
    pub dst_x: u32,
    pub dst_y: u32,
    pub width: u32,
    pub height: u32,
}

/// Compose `rect` from `src` onto `dst`, clipping to both bounds.
pub fn compose(dst: &mut Canvas<'_>, src: &Source<'_>, rect: CompositionRect, mode: Composition) {
    let width = rect
        .width
        .min(src.width.saturating_sub(rect.src_x))
        .min(dst.width.saturating_sub(rect.dst_x)) as usize;
    let height = rect
        .height
        .min(src.height.saturating_sub(rect.src_y))
        .min(dst.height.saturating_sub(rect.dst_y)) as usize;

    for row in 0..height {
        let src_start =
            ((rect.src_y as usize + row) * src.width as usize + rect.src_x as usize) * 4;
        let dst_start =
            ((rect.dst_y as usize + row) * dst.width as usize + rect.dst_x as usize) * 4;
        let src_row = &src.pixels[src_start..src_start + width * 4];
        let dst_row = &mut dst.pixels[dst_start..dst_start + width * 4];

        match mode {
            Composition::Overwrite => dst_row.copy_from_slice(src_row),
            Composition::AlphaBlend => {
                for (d, s) in dst_row.chunks_exact_mut(4).zip(src_row.chunks_exact(4)) {
                    blend(d, s);
                }
            },
        }
    }
}

/// Blend a non-premultiplied RGBA source pixel over a destination pixel.
#[inline]
fn blend(dst: &mut [u8], src: &[u8]) {
    let sa = u32::from(src[3]);
    if sa == 255 {
        dst.copy_from_slice(src);
        return;
    }
    if sa == 0 {
        return;
    }

    let da = u32::from(dst[3]);
    // out_a = sa + da * (1 - sa), all scaled by 255.
    let out_a = sa * 255 + da * (255 - sa);
    if out_a == 0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }

    for i in 0..3 {
        let sc = u32::from(src[i]);
        let dc = u32::from(dst[i]);
        let c = (sc * sa * 255 + dc * da * (255 - sa)) / out_a;
        dst[i] = c.min(255) as u8;
    }
    dst[3] = (out_a / 255).min(255) as u8;
}

/// Create a canvas filled with an RGBA color given as `0xRRGGBBAA`.
pub fn filled(width: u32, height: u32, color: u32) -> Vec<u8> {
    let rgba = color.to_be_bytes();
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for _ in 0..width as usize * height as usize {
        pixels.extend_from_slice(&rgba);
    }
    pixels
}

/// Advance the animation of `frames` to `now`.
///
/// Returns whether the current frame changed and the deadline of the next frame change.
pub fn advance(
    animation: &mut Animation,
    frames: &[Frame],
    current: &mut usize,
    now: Instant,
) -> (bool, Option<Instant>) {
    if frames.len() < 2 || animation.state == AnimationState::Stopped {
        return (false, None);
    }

    let shown_at = *animation.shown_at.get_or_insert(now);
    let mut shown_at = shown_at;
    let start = *current;
    let mut changed = false;

    // Bound iterations so a fully gapless animation can't spin forever.
    for _ in 0..frames.len() * 2 {
        let gap = frames[*current].gap;
        let deadline = shown_at + Duration::from_millis(gap.max(0) as u64);
        if gap > 0 && now < deadline {
            animation.shown_at = Some(shown_at);
            return (changed || *current != start, Some(deadline));
        }

        // Move to the next frame.
        let next = *current + 1;
        if next >= frames.len() {
            match animation.state {
                // Wait for more frames to arrive.
                AnimationState::Loading => {
                    animation.shown_at = Some(shown_at);
                    return (changed || *current != start, None);
                },
                _ => {
                    animation.loops_done += 1;
                    if animation.loops > 1 && animation.loops_done >= animation.loops - 1 {
                        animation.state = AnimationState::Stopped;
                        animation.shown_at = None;
                        return (changed || *current != start, None);
                    }
                    *current = 0;
                },
            }
        } else {
            *current = next;
        }
        changed = true;

        shown_at = if now.saturating_duration_since(deadline) > MAX_LAG { now } else { deadline };
    }

    // Only gapless frames: show the current one and check again later.
    animation.shown_at = Some(now);
    (changed || *current != start, Some(now + Duration::from_millis(DEFAULT_GAP as u64)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(gaps: &[i32]) -> Vec<Frame> {
        gaps.iter()
            .enumerate()
            .map(|(i, &gap)| Frame {
                data: Arc::new(ImageData::full(i as u64, 1, 1, vec![0; 4])),
                gap,
            })
            .collect()
    }

    #[test]
    fn blend_pixels() {
        let mut dst = [0, 0, 255, 255];
        blend(&mut dst, &[255, 0, 0, 128]);
        assert_eq!(dst[3], 255);
        assert!(dst[0] > 120 && dst[0] < 135 && dst[2] > 120 && dst[2] < 135);

        let mut dst = [0, 0, 0, 0];
        blend(&mut dst, &[10, 20, 30, 100]);
        assert_eq!(dst, [10, 20, 30, 100]);
    }

    #[test]
    fn compose_clips() {
        let mut pixels = filled(2, 2, 0x000000ff);
        let src = filled(3, 3, 0xff0000ff);
        let mut canvas = Canvas { width: 2, height: 2, pixels: &mut pixels };
        let source = Source { width: 3, height: 3, pixels: &src };
        let rect = CompositionRect { src_x: 0, src_y: 0, dst_x: 1, dst_y: 1, width: 3, height: 3 };
        compose(&mut canvas, &source, rect, Composition::Overwrite);
        assert_eq!(&pixels[..12], &[0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]);
        assert_eq!(&pixels[12..], &[255, 0, 0, 255]);
    }

    #[test]
    fn playback() {
        let frames = frames(&[100, 100, 100]);
        let mut animation = Animation { state: AnimationState::Running, ..Default::default() };
        let mut current = 0;
        let t0 = Instant::now();

        let (changed, next) = advance(&mut animation, &frames, &mut current, t0);
        assert!(!changed);
        assert_eq!(next, Some(t0 + Duration::from_millis(100)));

        let (changed, _) =
            advance(&mut animation, &frames, &mut current, t0 + Duration::from_millis(150));
        assert!(changed);
        assert_eq!(current, 1);

        // Loops back to the start.
        advance(&mut animation, &frames, &mut current, t0 + Duration::from_millis(310));
        assert_eq!(current, 0);
        assert_eq!(animation.loops_done, 1);
    }

    #[test]
    fn loading_waits_and_loops_stop() {
        let frames = frames(&[10, 10]);
        let mut animation = Animation { state: AnimationState::Loading, ..Default::default() };
        let mut current = 0;
        let t0 = Instant::now();
        advance(&mut animation, &frames, &mut current, t0);
        let (_, next) =
            advance(&mut animation, &frames, &mut current, t0 + Duration::from_millis(50));
        assert_eq!((current, next), (1, None));

        // Two loops total (`v=3`), then stop.
        let mut animation =
            Animation { state: AnimationState::Running, loops: 3, ..Default::default() };
        let mut current = 0;
        advance(&mut animation, &frames, &mut current, t0);
        let (_, next) =
            advance(&mut animation, &frames, &mut current, t0 + Duration::from_millis(45));
        assert_eq!(animation.state, AnimationState::Stopped);
        assert_eq!(next, None);
    }

    #[test]
    fn gapless_frames_are_skipped() {
        let frames = frames(&[100, -1, 100]);
        let mut animation = Animation { state: AnimationState::Running, ..Default::default() };
        let mut current = 0;
        let t0 = Instant::now();
        advance(&mut animation, &frames, &mut current, t0);
        advance(&mut animation, &frames, &mut current, t0 + Duration::from_millis(101));
        assert_eq!(current, 2);
    }
}
