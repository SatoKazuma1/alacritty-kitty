//! Kitty graphics protocol.
//!
//! See <https://sw.kovidgoyal.net/kitty/graphics-protocol/>.
//!
//! Images are stored in [`Graphics`]. Placements are anchored to grid rows through a row tag
//! (see [`crate::grid::Row::graphics_tag`]), which makes them follow scrolling, scrollback and
//! reflow together with the text. Virtual placements are rendered through Unicode placeholder
//! cells (`U+10EEEE`) instead.

use std::sync::Arc;

pub mod command;
pub mod diacritics;
pub mod load;

pub use command::Command;

/// Unicode placeholder character for virtual placements.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Default memory limit for stored image data.
pub const DEFAULT_MEMORY_LIMIT: usize = 320 * 1024 * 1024;

/// Decoded image ready for upload to the GPU.
#[derive(Debug)]
pub struct ImageData {
    /// Unique identifier for this image data, never reused.
    pub uid: u64,
    pub width: u32,
    pub height: u32,
    /// Non-premultiplied RGBA8 pixels.
    pub pixels: Vec<u8>,
}

/// Rectangle in pixels.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Placement of an image.
#[derive(Debug, Clone)]
pub struct Placement {
    pub id: u32,
    /// Row tag the placement is anchored to; zero for virtual placements.
    pub tag: u64,
    pub column: usize,
    pub x_offset: u32,
    pub y_offset: u32,
    /// Source rectangle inside the image.
    pub source: Rect,
    /// Requested number of columns, zero for automatic.
    pub columns: u32,
    /// Requested number of rows, zero for automatic.
    pub rows: u32,
    pub z: i32,
    pub is_virtual: bool,
}

impl Placement {
    /// Size of the placement in cells and pixels for the given cell size.
    pub fn size(&self, cell_width: u32, cell_height: u32) -> PlacementSize {
        let cw = cell_width.max(1) as f32;
        let ch = cell_height.max(1) as f32;
        let src_w = self.source.width.max(1) as f32;
        let src_h = self.source.height.max(1) as f32;

        let (width, height) = match (self.columns, self.rows) {
            (0, 0) => (src_w, src_h),
            (c, 0) => {
                let width = c as f32 * cw - self.x_offset as f32;
                (width, src_h * width / src_w)
            },
            (0, r) => {
                let height = r as f32 * ch - self.y_offset as f32;
                (src_w * height / src_h, height)
            },
            (c, r) => (c as f32 * cw - self.x_offset as f32, r as f32 * ch - self.y_offset as f32),
        };
        let width = width.max(1.);
        let height = height.max(1.);

        let columns = if self.columns != 0 {
            self.columns
        } else {
            ((self.x_offset as f32 + width) / cw).ceil() as u32
        };
        let rows = if self.rows != 0 {
            self.rows
        } else {
            ((self.y_offset as f32 + height) / ch).ceil() as u32
        };

        PlacementSize { columns: columns.max(1), rows: rows.max(1), width, height }
    }
}

/// Effective size of a placement.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacementSize {
    pub columns: u32,
    pub rows: u32,
    /// Displayed width in pixels.
    pub width: f32,
    /// Displayed height in pixels.
    pub height: f32,
}

/// A stored image.
#[derive(Debug)]
pub struct Image {
    /// Client assigned id, zero for anonymous images.
    pub id: u32,
    /// Client assigned number.
    pub number: u32,
    pub data: Arc<ImageData>,
    pub placements: Vec<Placement>,
    /// Last access time, used for eviction.
    pub(crate) atime: u64,
}

impl Image {
    fn memory(&self) -> usize {
        self.data.pixels.len()
    }
}

/// Chunked transmission in progress.
#[derive(Debug)]
pub(crate) struct Pending {
    pub cmd: Command,
    pub data: Vec<u8>,
    pub error: Option<load::Error>,
}

/// A single textured quad to be drawn by the renderer.
#[derive(Debug, Clone)]
pub struct GraphicsDraw {
    pub image: Arc<ImageData>,
    /// Viewport line of the cell the quad is positioned relative to; may be negative.
    pub line: i32,
    /// Column of the cell the quad is positioned relative to.
    pub column: usize,
    /// Offset in pixels from the top-left of the cell.
    pub x: f32,
    pub y: f32,
    /// Size of the quad in pixels.
    pub width: f32,
    pub height: f32,
    /// Source rectangle in image pixels (x, y, width, height).
    pub source: [f32; 4],
    pub z: i32,
}

/// Image storage.
#[derive(Debug)]
pub struct Graphics {
    pub(crate) images: Vec<Image>,
    pub(crate) pending: Option<Pending>,
    next_uid: u64,
    next_tag: u64,
    next_id: u32,
    clock: u64,
    pub(crate) memory_limit: usize,
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
}

impl Default for Graphics {
    fn default() -> Self {
        Self {
            images: Vec::new(),
            pending: None,
            next_uid: 1,
            next_tag: 1,
            next_id: 1,
            clock: 0,
            memory_limit: DEFAULT_MEMORY_LIMIT,
            cell_width: 10,
            cell_height: 20,
        }
    }
}

impl Graphics {
    /// Remove all images and placements.
    pub fn clear(&mut self) {
        self.images.clear();
        self.pending = None;
    }

    /// Whether there are any placements at all.
    pub fn has_placements(&self) -> bool {
        self.images.iter().any(|image| !image.placements.is_empty())
    }

    /// Whether any virtual placements exist.
    pub fn has_virtual_placements(&self) -> bool {
        self.images.iter().any(|image| image.placements.iter().any(|p| p.is_virtual))
    }

    pub fn images(&self) -> &[Image] {
        &self.images
    }

    pub(crate) fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub(crate) fn new_tag(&mut self) -> u64 {
        let tag = self.next_tag;
        self.next_tag += 1;
        tag
    }

    /// Allocate a free image id.
    pub(crate) fn alloc_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if self.image_index(id).is_none() {
                return id;
            }
        }
    }

    pub(crate) fn image_index(&self, id: u32) -> Option<usize> {
        if id == 0 {
            return None;
        }
        self.images.iter().position(|image| image.id == id)
    }

    /// Find the newest image with the given number.
    pub(crate) fn image_index_by_number(&self, number: u32) -> Option<usize> {
        if number == 0 {
            return None;
        }
        self.images
            .iter()
            .enumerate()
            .filter(|(_, image)| image.number == number)
            .max_by_key(|(_, image)| image.atime)
            .map(|(i, _)| i)
    }

    /// Add a new image, replacing any existing image with the same id.
    ///
    /// Returns the index of the image.
    pub(crate) fn insert_image(&mut self, id: u32, number: u32, pixels: load::Pixels) -> usize {
        if let Some(index) = self.image_index(id) {
            self.images.remove(index);
        }

        let data = Arc::new(ImageData {
            uid: self.next_uid,
            width: pixels.width,
            height: pixels.height,
            pixels: pixels.rgba,
        });
        self.next_uid += 1;

        let atime = self.tick();
        self.images.push(Image { id, number, data, placements: Vec::new(), atime });
        let index = self.images.len() - 1;

        self.enforce_memory_limit(index);

        // The new image is never evicted, but indices may have shifted.
        self.images.iter().position(|image| image.atime == atime).unwrap_or(0)
    }

    /// Evict old images until memory usage is below the limit.
    ///
    /// The image at `keep` is never evicted.
    fn enforce_memory_limit(&mut self, keep: usize) {
        let keep_atime = self.images[keep].atime;
        let mut used: usize = self.images.iter().map(Image::memory).sum();

        while used > self.memory_limit {
            // Prefer evicting images without placements, oldest first.
            let victim = self
                .images
                .iter()
                .enumerate()
                .filter(|(_, image)| image.atime != keep_atime)
                .min_by_key(|(_, image)| (!image.placements.is_empty(), image.atime))
                .map(|(i, _)| i);

            match victim {
                Some(index) => used -= self.images.remove(index).memory(),
                None => break,
            }
        }
    }

    /// Remove placements whose anchor rows no longer exist.
    pub(crate) fn retain_tags(&mut self, live: &std::collections::HashSet<u64>) {
        for image in &mut self.images {
            image.placements.retain(|p| p.is_virtual || live.contains(&p.tag));
        }
    }

    /// Remove anonymous images without any placements.
    pub(crate) fn remove_unreferenced_anonymous(&mut self) {
        self.images.retain(|image| image.id != 0 || !image.placements.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(columns: u32, rows: u32) -> Placement {
        Placement {
            id: 0,
            tag: 1,
            column: 0,
            x_offset: 0,
            y_offset: 0,
            source: Rect { x: 0, y: 0, width: 100, height: 50 },
            columns,
            rows,
            z: 0,
            is_virtual: false,
        }
    }

    #[test]
    fn placement_size() {
        let size = placement(0, 0).size(10, 20);
        assert_eq!((size.columns, size.rows), (10, 3));
        assert_eq!((size.width, size.height), (100., 50.));

        let size = placement(20, 0).size(10, 20);
        assert_eq!((size.columns, size.rows), (20, 5));
        assert_eq!((size.width, size.height), (200., 100.));

        let size = placement(5, 5).size(10, 20);
        assert_eq!((size.columns, size.rows), (5, 5));
        assert_eq!((size.width, size.height), (50., 100.));
    }

    #[test]
    fn memory_limit() {
        let mut graphics = Graphics { memory_limit: 100, ..Default::default() };
        let pixels = || load::Pixels { width: 4, height: 4, rgba: vec![0; 64] };
        graphics.insert_image(1, 0, pixels());
        graphics.insert_image(2, 0, pixels());
        assert_eq!(graphics.images.len(), 1);
        assert_eq!(graphics.images[0].id, 2);
    }
}
