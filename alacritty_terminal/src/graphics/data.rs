//! Image pixel storage.
//!
//! Animation frames are usually small changes on top of a previous frame. Instead of storing a
//! full copy of every frame, frames are stored as a base image plus the changed rectangles and
//! only fully composed when needed, which is when the renderer uploads them to the GPU.

use std::sync::Arc;

use super::animation::{self, Canvas, Composition, CompositionRect, Source};

/// Maximum length of a chain of delta frames before a frame is stored in full.
///
/// This bounds the cost of composing a frame.
const MAX_DELTA_DEPTH: usize = 32;

/// Decoded image ready for upload to the GPU.
#[derive(Debug)]
pub struct ImageData {
    /// Unique identifier for this image data, never reused.
    pub uid: u64,
    pub width: u32,
    pub height: u32,
    source: PixelSource,
}

#[derive(Debug)]
enum PixelSource {
    Full(Arc<[u8]>),
    Delta(Delta),
}

/// Frame stored as changes to a base image or a background color.
#[derive(Debug)]
struct Delta {
    base: Option<Arc<ImageData>>,
    /// Background color as `0xRRGGBBAA` when there is no base.
    background: u32,
    ops: Vec<DeltaOp>,
    depth: usize,
}

/// Rectangle of pixels composed onto the frame.
#[derive(Debug, Clone)]
pub struct DeltaOp {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Non-premultiplied RGBA pixels of the rectangle.
    pub pixels: Arc<[u8]>,
    pub mode: Composition,
}

impl ImageData {
    /// Image data with all pixels.
    pub fn full(uid: u64, width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self { uid, width, height, source: PixelSource::Full(pixels.into()) }
    }

    /// Image data composed of `op` on top of `base` or the `background` color.
    pub fn delta(
        uid: u64,
        width: u32,
        height: u32,
        base: Option<Arc<ImageData>>,
        background: u32,
        op: DeltaOp,
    ) -> Self {
        let covers_all = op.x == 0 && op.y == 0 && op.width == width && op.height == height;

        // A full replacement doesn't need any base.
        let replaces = op.mode == Composition::Overwrite || (base.is_none() && background == 0);
        if covers_all && replaces {
            let pixels = op.pixels.to_vec();
            return Self::full(uid, width, height, pixels);
        }

        let depth = base.as_ref().map_or(0, |base| base.depth() + 1);
        let delta = Delta { base, background, ops: vec![op], depth };
        let data = Self { uid, width, height, source: PixelSource::Delta(delta) };

        // Flatten long chains so composing a frame stays cheap.
        if depth >= MAX_DELTA_DEPTH {
            let pixels = data.pixels().to_vec();
            return Self::full(uid, width, height, pixels);
        }

        data
    }

    fn depth(&self) -> usize {
        match &self.source {
            PixelSource::Full(_) => 0,
            PixelSource::Delta(delta) => delta.depth,
        }
    }

    /// Composed RGBA pixels.
    ///
    /// Delta frames are composed on every call, so callers should only request pixels when
    /// needed, like when uploading a texture.
    pub fn pixels(&self) -> Arc<[u8]> {
        let delta = match &self.source {
            PixelSource::Full(pixels) => return pixels.clone(),
            PixelSource::Delta(delta) => delta,
        };

        let mut canvas = match &delta.base {
            Some(base) => base.pixels().to_vec(),
            None => animation::filled(self.width, self.height, delta.background),
        };

        for op in &delta.ops {
            let rect = CompositionRect {
                src_x: 0,
                src_y: 0,
                dst_x: op.x,
                dst_y: op.y,
                width: op.width,
                height: op.height,
            };
            animation::compose(
                &mut Canvas { width: self.width, height: self.height, pixels: &mut canvas },
                &Source { width: op.width, height: op.height, pixels: &op.pixels },
                rect,
                op.mode,
            );
        }

        canvas.into()
    }

    /// Memory used by this image data in bytes, excluding shared base images.
    pub fn memory(&self) -> usize {
        match &self.source {
            PixelSource::Full(pixels) => pixels.len(),
            PixelSource::Delta(delta) => delta.ops.iter().map(|op| op.pixels.len()).sum(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(x: u32, y: u32, w: u32, h: u32, rgba: [u8; 4], mode: Composition) -> DeltaOp {
        let pixels: Vec<u8> = rgba.repeat((w * h) as usize);
        DeltaOp { x, y, width: w, height: h, pixels: pixels.into(), mode }
    }

    #[test]
    fn delta_composition() {
        let base = Arc::new(ImageData::full(1, 2, 2, [255, 0, 0, 255].repeat(4)));
        let frame = ImageData::delta(
            2,
            2,
            2,
            Some(base.clone()),
            0,
            op(1, 1, 1, 1, [0, 0, 255, 255], Composition::Overwrite),
        );

        // Only the changed pixel is stored.
        assert_eq!(frame.memory(), 4);
        let pixels = frame.pixels();
        assert_eq!(&pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(&pixels[12..], &[0, 0, 255, 255]);
    }

    #[test]
    fn full_replacement_and_flattening() {
        let frame = ImageData::delta(
            1,
            1,
            1,
            None,
            0,
            op(0, 0, 1, 1, [1, 2, 3, 4], Composition::AlphaBlend),
        );
        assert_eq!(frame.depth(), 0);
        assert!(matches!(frame.source, PixelSource::Full(_)));

        let mut data = Arc::new(frame);
        for uid in 0..MAX_DELTA_DEPTH as u64 + 5 {
            let op = op(0, 0, 1, 1, [9, 9, 9, 9], Composition::AlphaBlend);
            data = Arc::new(ImageData::delta(uid, 1, 1, Some(data), 0, op));
            assert!(data.depth() < MAX_DELTA_DEPTH);
        }
    }
}
