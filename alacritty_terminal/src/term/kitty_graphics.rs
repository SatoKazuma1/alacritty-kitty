//! Kitty graphics protocol integration with the terminal state.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use log::debug;

use crate::event::{Event, EventListener};
use crate::graphics::animation::{self, Canvas, Composition, CompositionRect, Source};
use crate::graphics::load::{self, Error};
use crate::graphics::{
    AnimationState, Command, Frame, Graphics, GraphicsDraw, PLACEHOLDER, Pending, Placement, Rect,
    command, diacritics,
};
use crate::grid::{Dimensions, Grid};
use crate::index::{Column, Line};
use crate::term::Term;
use crate::term::cell::Cell;
use crate::vte::ansi::{Color, Handler};

/// Maximum size of base64 decoded data accumulated for one direct transmission.
const MAX_DIRECT_DATA: usize = 400 * 1024 * 1024;

/// Lenient base64 engine, since some clients pad every chunk and others don't.
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

fn decode_base64_into(payload: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    // Ignore whitespace some clients add for line wrapping.
    let result = if payload.iter().any(u8::is_ascii_whitespace) {
        let filtered: Vec<u8> =
            payload.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
        BASE64.decode_vec(filtered, out)
    } else {
        BASE64.decode_vec(payload, out)
    };
    result.map_err(|err| Error::inval(format!("invalid base64: {err}")))?;

    if out.len() > MAX_DIRECT_DATA {
        return Err(Error::inval("too much data"));
    }

    Ok(())
}

/// Image id encoded in a cell color.
fn color_id(color: Color) -> u32 {
    match color {
        Color::Spec(rgb) => (u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b),
        Color::Indexed(index) => u32::from(index),
        Color::Named(_) => 0,
    }
}

/// Decoded state of a Unicode placeholder cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaceholderCell {
    id_low: u32,
    placement_id: u32,
    row: u32,
    column: u32,
    msb: u32,
}

impl PlaceholderCell {
    fn decode(cell: &Cell, prev: Option<PlaceholderCell>) -> Self {
        let zerowidth = cell.zerowidth().unwrap_or(&[]);
        let mut row = zerowidth.first().and_then(|&c| diacritics::diacritic_index(c));
        let mut column = zerowidth.get(1).and_then(|&c| diacritics::diacritic_index(c));
        let mut msb = zerowidth.get(2).and_then(|&c| diacritics::diacritic_index(c));

        let id_low = color_id(cell.fg);
        let placement_id = cell.underline_color().map_or(0, color_id);

        if let Some(prev) = prev.filter(|p| p.id_low == id_low && p.placement_id == placement_id) {
            match (row, column) {
                (None, _) => {
                    row = Some(prev.row);
                    column = Some(prev.column + 1);
                    msb = msb.or(Some(prev.msb));
                },
                (Some(r), None) if r == prev.row => {
                    column = Some(prev.column + 1);
                    msb = msb.or(Some(prev.msb));
                },
                (Some(r), Some(c)) if r == prev.row && c == prev.column + 1 => {
                    msb = msb.or(Some(prev.msb));
                },
                _ => (),
            }
        }

        Self {
            id_low,
            placement_id,
            row: row.unwrap_or(0),
            column: column.unwrap_or(0),
            msb: msb.unwrap_or(0),
        }
    }

    fn image_id(&self) -> u32 {
        self.id_low | (self.msb << 24)
    }
}

impl<T: EventListener> Term<T> {
    /// Update the cell size used for graphics placement.
    pub fn set_cell_size(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        if (self.graphics.cell_width, self.graphics.cell_height) != (width, height) {
            self.graphics.cell_width = width;
            self.graphics.cell_height = height;
            self.mark_fully_damaged();
        }
    }

    /// Image storage.
    pub fn graphics(&self) -> &Graphics {
        &self.graphics
    }

    /// Handle a kitty graphics command.
    pub(super) fn kitty_graphics_command(&mut self, data: &[u8]) {
        if !self.config.kitty_graphics {
            return;
        }

        let Some((cmd, payload)) = command::parse(data) else {
            debug!("Invalid kitty graphics command");
            return;
        };

        // Continuation of a chunked transmission.
        if let Some(mut pending) = self.graphics.pending.take() {
            if pending.error.is_none() {
                if let Err(err) = decode_base64_into(payload, &mut pending.data) {
                    pending.data = Vec::new();
                    pending.error = Some(err);
                }
            }

            if cmd.quiet != 0 {
                pending.cmd.quiet = cmd.quiet;
            }

            if cmd.more {
                self.graphics.pending = Some(pending);
                return;
            }

            let Pending { cmd, data, error } = pending;
            match error {
                Some(err) => self.respond(&cmd, Err(err)),
                None => self.finish_transmission(cmd, data),
            }
            return;
        }

        match cmd.action {
            b't' | b'T' | b'q' | b'f' => {
                let mut data = Vec::new();
                let error = decode_base64_into(payload, &mut data).err();

                if cmd.more && cmd.medium == b'd' {
                    self.graphics.pending = Some(Pending { cmd, data, error });
                    return;
                }

                match error {
                    Some(err) => self.respond(&cmd, Err(err)),
                    None => self.finish_transmission(cmd, data),
                }
            },
            b'p' => {
                let index = self
                    .graphics
                    .image_index(cmd.id)
                    .or_else(|| self.graphics.image_index_by_number(cmd.number));
                match index {
                    Some(index) => {
                        let mut cmd = cmd.clone();
                        cmd.id = self.graphics.images[index].id;
                        let result = self.place(index, &cmd);
                        self.respond(&cmd, result);
                    },
                    None => self.respond(&cmd, Err(Error::new("ENOENT", "image not found"))),
                }
            },
            b'd' => self.delete(&cmd),
            b'a' => {
                let result = self.control_animation(&cmd);
                self.respond_errors(&cmd, result);
            },
            b'c' => {
                let result = self.compose_frames(&cmd);
                self.respond(&cmd, result);
            },
            _ => self.respond(&cmd, Err(Error::inval("unknown action"))),
        }
    }

    /// Process a fully received transmission.
    fn finish_transmission(&mut self, mut cmd: Command, data: Vec<u8>) {
        let raw = load::read_medium(&cmd, data).and_then(|data| load::decompress(&cmd, data));

        if cmd.action == b'f' {
            let result = raw.and_then(|data| self.add_frame(&cmd, data));
            self.respond(&cmd, result);
            return;
        }

        let result = raw.and_then(|data| load::decode(&cmd, data));

        let pixels = match result {
            Ok(pixels) => pixels,
            Err(err) => {
                self.respond(&cmd, Err(err));
                return;
            },
        };

        if cmd.action == b'q' {
            self.respond(&cmd, Ok(()));
            return;
        }

        if cmd.id == 0 && cmd.number != 0 {
            cmd.id = self.graphics.alloc_id();
        }

        let index = self.graphics.insert_image(cmd.id, cmd.number, pixels);
        self.mark_fully_damaged();

        let result = if cmd.action == b'T' { self.place(index, &cmd) } else { Ok(()) };

        self.graphics.remove_unreferenced_anonymous();
        self.respond(&cmd, result);
    }

    /// Find the image referenced by a command.
    fn command_image(&self, cmd: &Command) -> Result<usize, Error> {
        self.graphics
            .image_index(cmd.id)
            .or_else(|| self.graphics.image_index_by_number(cmd.number))
            .ok_or_else(|| Error::new("ENOENT", "image not found"))
    }

    /// Add or edit an animation frame (`a=f`).
    fn add_frame(&mut self, cmd: &Command, data: Vec<u8>) -> Result<(), Error> {
        let index = self.command_image(cmd)?;
        let image = &self.graphics.images[index];
        let (width, height) = (image.width(), image.height());

        // Raw data covers the rest of the image unless its size is given.
        let mut data_cmd = cmd.clone();
        if data_cmd.format != 100 {
            if data_cmd.width == 0 {
                data_cmd.width = width.saturating_sub(cmd.x);
            }
            if data_cmd.height == 0 {
                data_cmd.height = height.saturating_sub(cmd.y);
            }
        }
        let pixels = load::decode(&data_cmd, data)?;

        let frame_pixels = |number: u32| -> Result<Vec<u8>, Error> {
            let frame = image.frames.get(number as usize - 1);
            frame
                .map(|frame| frame.data.pixels.clone())
                .ok_or_else(|| Error::new("ENOENT", format!("frame {number} not found")))
        };

        // Edit an existing frame, or create a new one on top of a base frame or color.
        let (mut canvas, edit) = if cmd.rows != 0 {
            (frame_pixels(cmd.rows)?, Some(cmd.rows as usize - 1))
        } else if cmd.columns != 0 {
            (frame_pixels(cmd.columns)?, None)
        } else {
            (animation::filled(width, height, cmd.y_offset), None)
        };

        let mode = if cmd.x_offset == 1 { Composition::Overwrite } else { Composition::AlphaBlend };
        let rect = CompositionRect {
            src_x: 0,
            src_y: 0,
            dst_x: cmd.x,
            dst_y: cmd.y,
            width: pixels.width,
            height: pixels.height,
        };
        animation::compose(
            &mut Canvas { width, height, pixels: &mut canvas },
            &Source { width: pixels.width, height: pixels.height, pixels: &pixels.rgba },
            rect,
            mode,
        );

        let data = self.graphics.image_data(width, height, canvas);
        let atime = self.graphics.tick();
        let image = &mut self.graphics.images[index];
        image.atime = atime;
        match edit {
            Some(i) => {
                image.frames[i].data = data;
                if cmd.z != 0 {
                    image.frames[i].gap = cmd.z;
                }
            },
            None => {
                let gap = if cmd.z != 0 { cmd.z } else { animation::DEFAULT_GAP };
                image.frames.push(Frame { data, gap });
            },
        }

        self.graphics.enforce_memory_limit(index);
        self.mark_fully_damaged();

        Ok(())
    }

    /// Control animation playback (`a=a`).
    fn control_animation(&mut self, cmd: &Command) -> Result<(), Error> {
        let index = self.command_image(cmd)?;
        let image = &mut self.graphics.images[index];

        // Gap of a specific frame.
        if cmd.rows != 0 && cmd.z != 0 {
            let frame = image.frames.get_mut(cmd.rows as usize - 1);
            let frame = frame.ok_or_else(|| Error::new("ENOENT", "frame not found"))?;
            frame.gap = cmd.z;
        }

        // Current frame.
        if cmd.columns != 0 {
            let frame = cmd.columns as usize - 1;
            if frame >= image.frames.len() {
                return Err(Error::new("ENOENT", "frame not found"));
            }
            image.current_frame = frame;
            image.animation.shown_at = None;
        }

        // Number of loops.
        if cmd.height != 0 {
            image.animation.loops = cmd.height;
        }

        let animation = &mut image.animation;
        match cmd.width {
            1 => {
                animation.state = AnimationState::Stopped;
                animation.loops_done = 0;
                animation.shown_at = None;
            },
            2 | 3 => {
                if animation.state == AnimationState::Stopped {
                    animation.shown_at = None;
                }
                animation.state =
                    if cmd.width == 2 { AnimationState::Loading } else { AnimationState::Running };
            },
            _ => (),
        }

        self.mark_fully_damaged();

        Ok(())
    }

    /// Compose a rectangle from one frame onto another (`a=c`).
    fn compose_frames(&mut self, cmd: &Command) -> Result<(), Error> {
        let index = self.command_image(cmd)?;
        let image = &self.graphics.images[index];
        let (width, height) = (image.width(), image.height());

        let src = cmd.rows.max(1) as usize - 1;
        let dst = cmd.columns.max(1) as usize - 1;
        if src >= image.frames.len() || dst >= image.frames.len() {
            return Err(Error::new("ENOENT", "frame not found"));
        }

        let w = if cmd.w == 0 { width } else { cmd.w };
        let h = if cmd.h == 0 { height } else { cmd.h };
        let (src_x, src_y, dst_x, dst_y) = (cmd.x_offset, cmd.y_offset, cmd.x, cmd.y);
        let out_of_bounds = |x: u32, y: u32| {
            x.checked_add(w).is_none_or(|end| end > width)
                || y.checked_add(h).is_none_or(|end| end > height)
        };
        if out_of_bounds(src_x, src_y) || out_of_bounds(dst_x, dst_y) {
            return Err(Error::inval("rectangle out of bounds"));
        }

        let overlaps =
            src_x < dst_x + w && dst_x < src_x + w && src_y < dst_y + h && dst_y < src_y + h;
        if src == dst && overlaps {
            return Err(Error::inval("source and destination rectangles overlap"));
        }

        let mut canvas = image.frames[dst].data.pixels.clone();
        let mode =
            if cmd.no_cursor_move { Composition::Overwrite } else { Composition::AlphaBlend };
        animation::compose(
            &mut Canvas { width, height, pixels: &mut canvas },
            &Source { width, height, pixels: &image.frames[src].data.pixels },
            CompositionRect { src_x, src_y, dst_x, dst_y, width: w, height: h },
            mode,
        );

        let data = self.graphics.image_data(width, height, canvas);
        self.graphics.images[index].frames[dst].data = data;
        self.mark_fully_damaged();

        Ok(())
    }

    /// Advance running animations.
    ///
    /// Returns when the next animation frame is due.
    pub fn advance_animations(&mut self, now: Instant) -> Option<Instant> {
        let (changed, next) = self.graphics.advance_animations(now);
        if changed {
            self.mark_fully_damaged();
        }
        next
    }

    /// Create a placement for the image at `index`.
    fn place(&mut self, index: usize, cmd: &Command) -> Result<(), Error> {
        let atime = self.graphics.tick();
        let image = &self.graphics.images[index];
        let (width, height) = (image.width(), image.height());

        let x = cmd.x.min(width);
        let y = cmd.y.min(height);
        let source = Rect {
            x,
            y,
            width: if cmd.w == 0 { width - x } else { cmd.w.min(width - x) },
            height: if cmd.h == 0 { height - y } else { cmd.h.min(height - y) },
        };
        if source.width == 0 || source.height == 0 {
            return Err(Error::inval("empty source rectangle"));
        }

        let mut placement = Placement {
            id: cmd.placement_id,
            tag: 0,
            column: 0,
            x_offset: cmd.x_offset,
            y_offset: cmd.y_offset,
            source,
            columns: cmd.columns,
            rows: cmd.rows,
            z: cmd.z,
            is_virtual: cmd.virtual_placement,
        };

        if !placement.is_virtual {
            let cursor = self.grid.cursor.point;
            let row = &mut self.grid[cursor.line];
            if row.graphics_tag == 0 {
                row.graphics_tag = self.graphics.new_tag();
            }
            placement.tag = row.graphics_tag;
            placement.column = cursor.column.0;
        }

        let size = placement.size(self.graphics.cell_width, self.graphics.cell_height);
        let is_virtual = placement.is_virtual;

        // Anonymous images can't be referenced by the client anymore, so a new anonymous
        // placement exactly covering an older one replaces it. Without this, players like
        // `mpv --vo=kitty`, which send every frame as a new anonymous image, would pile up
        // hundreds of stacked frames.
        let (cell_width, cell_height) = (self.graphics.cell_width, self.graphics.cell_height);
        if self.graphics.images[index].id == 0 && !placement.is_virtual {
            for (i, other) in self.graphics.images.iter_mut().enumerate() {
                if i == index || other.id != 0 || other.number != 0 {
                    continue;
                }
                other.placements.retain(|p| {
                    let covered = p.tag == placement.tag
                        && p.column == placement.column
                        && p.x_offset == placement.x_offset
                        && p.y_offset == placement.y_offset
                        && p.z == placement.z
                        && p.size(cell_width, cell_height) == size;
                    !covered
                });
            }
        }

        let image = &mut self.graphics.images[index];
        image.atime = atime;
        if placement.id != 0 {
            image.placements.retain(|p| p.id != placement.id);
        }
        image.placements.push(placement);

        if !is_virtual && !cmd.no_cursor_move {
            for _ in 1..size.rows {
                self.linefeed();
            }

            let column = self.grid.cursor.point.column.0 + size.columns as usize;
            if column >= self.columns() {
                self.grid.cursor.point.column = Column(0);
                self.linefeed();
            } else {
                self.grid.cursor.point.column = Column(column);
            }
            self.grid.cursor.input_needs_wrap = false;
        }

        self.collect_garbage();
        self.mark_fully_damaged();

        Ok(())
    }

    /// Handle a deletion command.
    fn delete(&mut self, cmd: &Command) {
        let free = cmd.delete.is_ascii_uppercase();
        let what = cmd.delete.to_ascii_lowercase();

        // Screen lines of all visible anchor tags.
        let mut lines: HashMap<u64, i32> = HashMap::new();
        for line in 0..self.screen_lines() as i32 {
            let tag = self.grid[Line(line)].graphics_tag;
            if tag != 0 {
                lines.insert(tag, line);
            }
        }

        let (cell_width, cell_height) = (self.graphics.cell_width, self.graphics.cell_height);
        let cursor = self.grid.cursor.point;
        let number_index = self.graphics.image_index_by_number(cmd.number);

        // Check whether a placement covers a cell (zero based).
        let covers = |p: &Placement, column: Option<usize>, line: Option<i32>| -> bool {
            let Some(&start) = lines.get(&p.tag) else { return false };
            let size = p.size(cell_width, cell_height);
            let line_ok = line.is_none_or(|l| l >= start && l < start + size.rows as i32);
            let column_ok =
                column.is_none_or(|c| c >= p.column && c < p.column + size.columns as usize);
            line_ok && column_ok
        };

        let cell_x = (cmd.x as usize).checked_sub(1);
        let cell_y = (cmd.y as i32).checked_sub(1);

        let mut to_free = vec![false; self.graphics.images.len()];
        for (index, image) in self.graphics.images.iter_mut().enumerate() {
            let before = image.placements.len();
            let image_matches = match what {
                b'i' => cmd.id != 0 && image.id == cmd.id,
                b'n' => number_index == Some(index),
                b'r' => image.id != 0 && image.id >= cmd.x && image.id <= cmd.y,
                _ => true,
            };
            if !image_matches {
                continue;
            }

            image.placements.retain(|p| {
                let delete = match what {
                    b'a' => p.is_virtual || lines.contains_key(&p.tag),
                    b'i' | b'n' => cmd.placement_id == 0 || p.id == cmd.placement_id,
                    b'r' => true,
                    b'c' => covers(p, Some(cursor.column.0), Some(cursor.line.0)),
                    b'p' => cell_x.is_some() && cell_y.is_some() && covers(p, cell_x, cell_y),
                    b'q' => {
                        cell_x.is_some()
                            && cell_y.is_some()
                            && covers(p, cell_x, cell_y)
                            && p.z == cmd.z
                    },
                    b'x' => cell_x.is_some() && covers(p, cell_x, None),
                    b'y' => cell_y.is_some() && covers(p, None, cell_y),
                    b'z' => p.z == cmd.z,
                    _ => false,
                };
                !delete
            });

            // Free image data once no placements reference it anymore.
            let deleted = before != image.placements.len();
            to_free[index] = free
                && image.placements.is_empty()
                && (deleted || matches!(what, b'a' | b'i' | b'n' | b'r'));
        }

        let mut to_free = to_free.into_iter();
        self.graphics.images.retain(|_| !to_free.next().unwrap_or(false));

        self.graphics.remove_unreferenced_anonymous();
        self.mark_fully_damaged();
    }

    /// Remove placements whose anchor rows have been cleared or dropped from history.
    fn collect_garbage(&mut self) {
        let mut live = HashSet::new();
        collect_tags(&self.grid, &mut live);
        collect_tags(&self.inactive_grid, &mut live);
        self.graphics.retain_tags(&live);
        self.graphics.remove_unreferenced_anonymous();
    }

    /// Send only error responses to the client.
    fn respond_errors(&mut self, cmd: &Command, result: Result<(), Error>) {
        if result.is_err() {
            self.respond(cmd, result);
        }
    }

    /// Send a response to the client.
    fn respond(&mut self, cmd: &Command, result: Result<(), Error>) {
        if cmd.id == 0 && cmd.number == 0 {
            return;
        }

        let message = match &result {
            Ok(()) if cmd.quiet >= 1 => return,
            Err(_) if cmd.quiet >= 2 => return,
            Ok(()) => String::from("OK"),
            Err(err) => err.to_string(),
        };

        let mut keys = Vec::new();
        if cmd.id != 0 {
            keys.push(format!("i={}", cmd.id));
        }
        if cmd.number != 0 {
            keys.push(format!("I={}", cmd.number));
        }
        if cmd.placement_id != 0 {
            keys.push(format!("p={}", cmd.placement_id));
        }

        let text = format!("\x1b_G{};{}\x1b\\", keys.join(","), message);
        self.event_proxy.send_event(Event::PtyWrite(text));
    }

    /// Compute all image quads visible in the viewport.
    pub fn graphics_frame(&self) -> Vec<GraphicsDraw> {
        let mut draws = Vec::new();
        if !self.graphics.has_placements() {
            return draws;
        }

        let (cell_width, cell_height) = (self.graphics.cell_width, self.graphics.cell_height);
        let display_offset = self.grid.display_offset() as i32;
        let screen_lines = self.screen_lines() as i32;

        // Anchored placements.
        let mut tags: HashMap<u64, Vec<(usize, usize)>> = HashMap::new();
        let mut max_rows = 0;
        for (image_index, image) in self.graphics.images.iter().enumerate() {
            for (placement_index, placement) in image.placements.iter().enumerate() {
                if placement.is_virtual {
                    continue;
                }
                let size = placement.size(cell_width, cell_height);
                max_rows = max_rows.max(size.rows as i32);
                tags.entry(placement.tag).or_default().push((image_index, placement_index));
            }
        }

        if !tags.is_empty() {
            let topmost = self.grid.topmost_line().0;
            let start = (-display_offset - max_rows + 1).max(topmost);
            let end = -display_offset + screen_lines;
            for line in start..end {
                let tag = self.grid[Line(line)].graphics_tag;
                let Some(entries) = tags.get(&tag).filter(|_| tag != 0) else { continue };

                for &(image_index, placement_index) in entries {
                    let image = &self.graphics.images[image_index];
                    let placement = &image.placements[placement_index];
                    let size = placement.size(cell_width, cell_height);
                    let source = placement.source;

                    draws.push(GraphicsDraw {
                        image: image.data().clone(),
                        line: line + display_offset,
                        column: placement.column,
                        x: placement.x_offset as f32,
                        y: placement.y_offset as f32,
                        width: size.width,
                        height: size.height,
                        source: [
                            source.x as f32,
                            source.y as f32,
                            source.width as f32,
                            source.height as f32,
                        ],
                        z: placement.z,
                    });
                }
            }
        }

        // Unicode placeholders.
        if self.graphics.has_virtual_placements() {
            for viewport_line in 0..screen_lines {
                let line = Line(viewport_line - display_offset);
                let row = &self.grid[line];
                let mut prev = None;

                for column in 0..self.columns() {
                    let cell = &row[Column(column)];
                    if cell.c != PLACEHOLDER {
                        prev = None;
                        continue;
                    }

                    let placeholder = PlaceholderCell::decode(cell, prev);
                    prev = Some(placeholder);

                    self.placeholder_draw(&mut draws, &placeholder, viewport_line, column);
                }
            }
        }

        draws
    }

    /// Add the quad for a single placeholder cell.
    fn placeholder_draw(
        &self,
        draws: &mut Vec<GraphicsDraw>,
        cell: &PlaceholderCell,
        line: i32,
        column: usize,
    ) {
        let Some(index) = self.graphics.image_index(cell.image_id()) else { return };
        let image = &self.graphics.images[index];
        let placement = image
            .placements
            .iter()
            .find(|p| p.is_virtual && (cell.placement_id == 0 || p.id == cell.placement_id));
        let Some(placement) = placement else { return };

        let (cw, ch) = (self.graphics.cell_width as f32, self.graphics.cell_height as f32);
        let size = placement.size(self.graphics.cell_width, self.graphics.cell_height);
        if cell.row >= size.rows || cell.column >= size.columns {
            return;
        }

        // Fit the image into the placement box, preserving aspect ratio.
        let src = placement.source;
        let (src_w, src_h) = (src.width as f32, src.height as f32);
        let box_w = size.columns as f32 * cw;
        let box_h = size.rows as f32 * ch;
        let scale = (box_w / src_w).min(box_h / src_h);
        let (draw_w, draw_h) = (src_w * scale, src_h * scale);
        let (off_x, off_y) = ((box_w - draw_w) / 2., (box_h - draw_h) / 2.);

        let cell_x = cell.column as f32 * cw;
        let cell_y = cell.row as f32 * ch;
        let x0 = cell_x.max(off_x);
        let x1 = (cell_x + cw).min(off_x + draw_w);
        let y0 = cell_y.max(off_y);
        let y1 = (cell_y + ch).min(off_y + draw_h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }

        let source = [
            src.x as f32 + (x0 - off_x) / scale,
            src.y as f32 + (y0 - off_y) / scale,
            (x1 - x0) / scale,
            (y1 - y0) / scale,
        ];

        // Merge with the previous quad if it's horizontally contiguous.
        if let Some(last) = draws.last_mut() {
            let last_end = last.column as f32 * cw + last.x + last.width;
            let start = column as f32 * cw + (x0 - cell_x);
            let contiguous = last.image.uid == image.data().uid
                && last.line == line
                && last.z == placement.z
                && (last_end - start).abs() < 0.01
                && (last.y - (y0 - cell_y)).abs() < 0.01
                && (last.height - (y1 - y0)).abs() < 0.01
                && (last.source[0] + last.source[2] - source[0]).abs() < 0.01;
            if contiguous {
                last.width += x1 - x0;
                last.source[2] += source[2];
                return;
            }
        }

        draws.push(GraphicsDraw {
            image: image.data().clone(),
            line,
            column,
            x: x0 - cell_x,
            y: y0 - cell_y,
            width: x1 - x0,
            height: y1 - y0,
            source,
            z: placement.z,
        });
    }
}

fn collect_tags(grid: &Grid<Cell>, live: &mut HashSet<u64>) {
    let top = grid.topmost_line().0;
    let bottom = grid.screen_lines() as i32;
    for line in top..bottom {
        let tag = grid[Line(line)].graphics_tag;
        if tag != 0 {
            live.insert(tag);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    use super::*;
    use crate::term::Config;
    use crate::term::test::TermSize;
    use crate::vte::ansi::Processor;

    #[derive(Clone, Default)]
    struct Responses(Arc<Mutex<Vec<String>>>);

    impl EventListener for Responses {
        fn send_event(&self, event: Event) {
            if let Event::PtyWrite(text) = event {
                self.0.lock().unwrap().push(text);
            }
        }
    }

    fn term() -> (Term<Responses>, Processor, Responses) {
        let responses = Responses::default();
        let mut term = Term::new(Config::default(), &TermSize::new(20, 10), responses.clone());
        term.set_cell_size(10, 20);
        (term, Processor::new(), responses)
    }

    /// RGBA image of `w`x`h` pixels as base64.
    fn rgba(w: usize, h: usize) -> String {
        STANDARD.encode(vec![255u8; w * h * 4])
    }

    fn feed(term: &mut Term<Responses>, parser: &mut Processor, data: &str) {
        parser.advance(term, data.as_bytes());
    }

    #[test]
    fn transmit_and_display() {
        let (mut term, mut parser, responses) = term();
        let cmd = format!("\x1b_Ga=T,i=7,s=25,v=30;{}\x1b\\", rgba(25, 30));
        feed(&mut term, &mut parser, &cmd);

        assert_eq!(responses.0.lock().unwrap().as_slice(), ["\x1b_Gi=7;OK\x1b\\"]);

        // 25x30 px at 10x20 cells is 3 columns and 2 rows.
        assert_eq!(term.grid.cursor.point, crate::index::Point::new(Line(1), Column(3)));

        let frame = term.graphics_frame();
        assert_eq!(frame.len(), 1);
        assert_eq!((frame[0].line, frame[0].column), (0, 0));
        assert_eq!((frame[0].width, frame[0].height), (25., 30.));
    }

    #[test]
    fn chunked_and_quiet() {
        let (mut term, mut parser, responses) = term();
        let data = rgba(4, 4);
        let (a, b) = data.split_at(40);
        feed(&mut term, &mut parser, &format!("\x1b_Ga=t,i=3,s=4,v=4,q=1,m=1;{a}\x1b\\"));
        feed(&mut term, &mut parser, &format!("\x1b_Gm=0;{b}\x1b\\"));

        assert!(responses.0.lock().unwrap().is_empty());
        assert_eq!(term.graphics.images.len(), 1);
        assert_eq!(term.graphics.images[0].width(), 4);
        assert!(term.graphics.images[0].placements.is_empty());
    }

    #[test]
    fn errors_and_query() {
        let (mut term, mut parser, responses) = term();
        feed(&mut term, &mut parser, "\x1b_Ga=p,i=99\x1b\\");
        feed(&mut term, &mut parser, &format!("\x1b_Ga=q,i=1,s=1,v=1;{}\x1b\\", rgba(1, 1)));

        let responses = responses.0.lock().unwrap();
        assert!(responses[0].starts_with("\x1b_Gi=99;ENOENT:"));
        assert_eq!(responses[1], "\x1b_Gi=1;OK\x1b\\");
        assert!(term.graphics.images.is_empty());
    }

    #[test]
    fn scroll_with_text() {
        let (mut term, mut parser, _) = term();
        feed(&mut term, &mut parser, &format!("\x1b_Ga=T,i=1,s=10,v=20;{}\x1b\\", rgba(10, 20)));
        assert_eq!(term.graphics_frame()[0].line, 0);

        feed(&mut term, &mut parser, "\r\n\r\n\r\n");
        assert_eq!(term.graphics_frame()[0].line, 0);

        // Scroll the image off the screen into history.
        feed(&mut term, &mut parser, &"\r\n".repeat(10));
        assert!(term.graphics_frame().is_empty());

        // Scrolling the viewport up reveals it again.
        term.scroll_display(crate::grid::Scroll::Delta(5));
        assert_eq!(term.graphics_frame().len(), 1);

        // Clearing history drops the placement.
        feed(&mut term, &mut parser, "\x1b[3J");
        feed(&mut term, &mut parser, &format!("\x1b_Ga=T,i=2,s=10,v=20;{}\x1b\\", rgba(10, 20)));
        assert_eq!(term.graphics.images.iter().map(|i| i.placements.len()).sum::<usize>(), 1);
    }

    #[test]
    fn delete() {
        let (mut term, mut parser, _) = term();
        let img = rgba(10, 20);
        feed(&mut term, &mut parser, &format!("\x1b_Ga=T,i=1,s=10,v=20;{img}\x1b\\"));
        feed(&mut term, &mut parser, &format!("\x1b_Ga=T,i=2,p=5,s=10,v=20;{img}\x1b\\"));
        assert_eq!(term.graphics_frame().len(), 2);

        feed(&mut term, &mut parser, "\x1b_Ga=d,d=i,i=1\x1b\\");
        assert_eq!(term.graphics_frame().len(), 1);
        assert_eq!(term.graphics.images.len(), 2);

        feed(&mut term, &mut parser, "\x1b_Ga=d,d=I,i=1\x1b\\");
        assert_eq!(term.graphics.images.len(), 1);

        feed(&mut term, &mut parser, "\x1b_Ga=d,d=A\x1b\\");
        assert!(term.graphics.images.is_empty());
    }

    #[test]
    fn alt_screen_clear() {
        let (mut term, mut parser, _) = term();
        feed(&mut term, &mut parser, "\x1b[?1049h");
        feed(&mut term, &mut parser, &format!("\x1b_Ga=T,i=1,s=10,v=20;{}\x1b\\", rgba(10, 20)));
        assert_eq!(term.graphics_frame().len(), 1);

        feed(&mut term, &mut parser, "\x1b[?1049l");
        assert!(term.graphics_frame().is_empty());
    }

    #[test]
    fn unicode_placeholders() {
        let (mut term, mut parser, _) = term();
        // 20x40 px image over 2x2 cells.
        feed(
            &mut term,
            &mut parser,
            &format!("\x1b_Ga=T,U=1,i=42,c=2,r=2,s=20,v=40;{}\x1b\\", rgba(20, 40)),
        );
        assert!(term.graphics_frame().is_empty());

        // Image id 42 in the foreground color, row/column diacritics.
        let p = PLACEHOLDER;
        let d = |i: usize| char::from_u32(diacritics::DIACRITICS[i]).unwrap();
        let text = format!("\x1b[38;5;42m{p}{}{}{p}\r\n{p}{}{p}\x1b[39m", d(0), d(0), d(1));
        feed(&mut term, &mut parser, &text);

        let frame = term.graphics_frame();
        // One merged quad per row; the 1:2 image is centered horizontally in a 20x40 box.
        assert_eq!(frame.len(), 2);
        assert_eq!((frame[0].line, frame[0].column), (0, 0));
        assert_eq!((frame[0].width, frame[0].height), (20., 20.));
        assert_eq!(frame[1].line, 1);
        assert_eq!(frame[1].source, [0., 20., 20., 20.]);
    }

    #[test]
    fn icat_detection_queries() {
        let (mut term, mut parser, responses) = term();
        feed(
            &mut term,
            &mut parser,
            "\x1b_Ga=q,f=24,s=1,v=1,S=3,i=1;MTIz\x1b\\\x1b_Ga=q,f=24,t=t,s=1,v=1,S=47,i=2;\
             L2Rldi9zaG0va2l0dHktdHR5LWdyYXBoaWNzLXByb3RvY29sLTMwMzkwMDYwNzk\x1b\\\x1b_Ga=q,f=24,\
             t=s,s=1,v=1,S=19,i=3;L2ljYXQtN0E1VTNFWExMUjRGNA\x1b\\\x1b[c",
        );

        // `kitten icat` only accepts DA1 responses with more than 3 characters.
        let responses = responses.0.lock().unwrap();
        assert_eq!(responses.len(), 4);
        assert_eq!(responses[0], "\x1b_Gi=1;OK\x1b\\");
        assert_eq!(responses[3], "\x1b[?62;22c");
    }

    #[test]
    fn anonymous_frames_replace_each_other() {
        let (mut term, mut parser, _) = term();
        let frame = format!(
            "\x1b[1;1H\x1b_Ga=T,f=24,s=10,v=20,C=1,q=2;{}\x1b\\",
            STANDARD.encode([0u8; 600])
        );
        for _ in 0..5 {
            feed(&mut term, &mut parser, &frame);
        }

        assert_eq!(term.graphics.images.len(), 1);
        assert_eq!(term.graphics_frame().len(), 1);
    }

    /// Solid RGB frame data as base64.
    fn rgb(w: usize, h: usize, color: [u8; 3]) -> String {
        STANDARD.encode(color.repeat(w * h))
    }

    #[test]
    fn animation_like_icat() {
        use std::time::{Duration, Instant};

        let (mut term, mut parser, responses) = term();
        // Root frame, then gap and loops for it, then two frames and start playback.
        feed(
            &mut term,
            &mut parser,
            &format!("\x1b_Ga=T,f=24,i=5,s=2,v=2,q=2;{}\x1b\\", rgb(2, 2, [255, 0, 0])),
        );
        feed(&mut term, &mut parser, "\x1b_Ga=a,i=5,r=1,z=100,q=2\x1b\\");
        feed(
            &mut term,
            &mut parser,
            &format!("\x1b_Ga=f,f=24,i=5,z=100,X=1,q=2;{}\x1b\\", rgb(2, 2, [0, 255, 0])),
        );
        feed(&mut term, &mut parser, "\x1b_Ga=a,i=5,s=2,q=2\x1b\\");
        // Partial frame on top of frame 2: only the top-left pixel becomes blue.
        feed(
            &mut term,
            &mut parser,
            &format!("\x1b_Ga=f,f=24,i=5,c=2,s=1,v=1,z=100,q=2;{}\x1b\\", rgb(1, 1, [0, 0, 255])),
        );
        feed(&mut term, &mut parser, "\x1b_Ga=a,i=5,s=3,q=2\x1b\\");
        assert!(responses.0.lock().unwrap().is_empty());

        let image = &term.graphics.images[0];
        assert_eq!(image.frames.len(), 3);
        assert_eq!(&image.frames[2].data.pixels[..8], &[0, 0, 255, 255, 0, 255, 0, 255]);

        let pixel = |term: &Term<Responses>| term.graphics_frame()[0].image.pixels[..4].to_vec();
        assert_eq!(pixel(&term), [255, 0, 0, 255]);

        let t0 = Instant::now();
        let next = term.advance_animations(t0);
        assert_eq!(next, Some(t0 + Duration::from_millis(100)));
        term.advance_animations(t0 + Duration::from_millis(110));
        assert_eq!(pixel(&term), [0, 255, 0, 255]);
        term.advance_animations(t0 + Duration::from_millis(210));
        assert_eq!(pixel(&term), [0, 0, 255, 255]);
        term.advance_animations(t0 + Duration::from_millis(310));
        assert_eq!(pixel(&term), [255, 0, 0, 255]);

        // Client driven: stop and jump to frame 2.
        feed(&mut term, &mut parser, "\x1b_Ga=a,i=5,s=1,c=2\x1b\\");
        assert_eq!(term.advance_animations(t0 + Duration::from_secs(5)), None);
        assert_eq!(pixel(&term), [0, 255, 0, 255]);
    }

    #[test]
    fn compose_frames() {
        let (mut term, mut parser, responses) = term();
        feed(
            &mut term,
            &mut parser,
            &format!("\x1b_Ga=t,f=24,i=1,s=2,v=1,q=2;{}\x1b\\", rgb(2, 1, [255, 0, 0])),
        );
        feed(
            &mut term,
            &mut parser,
            &format!("\x1b_Ga=f,f=24,i=1,q=2;{}\x1b\\", rgb(2, 1, [0, 0, 255])),
        );
        // Copy the left pixel of frame 1 onto the right pixel of frame 2.
        feed(&mut term, &mut parser, "\x1b_Ga=c,i=1,r=1,c=2,w=1,h=1,X=0,Y=0,x=1,y=0,C=1\x1b\\");
        // Out of bounds.
        feed(&mut term, &mut parser, "\x1b_Ga=c,i=1,r=1,c=2,w=2,h=1,x=1\x1b\\");

        let frame = &term.graphics.images[0].frames[1].data.pixels;
        assert_eq!(&frame[..], &[0, 0, 255, 255, 255, 0, 0, 255]);
        let responses = responses.0.lock().unwrap();
        assert_eq!(responses[0], "\x1b_Gi=1;OK\x1b\\");
        assert!(responses[1].starts_with("\x1b_Gi=1;EINVAL"));
    }
}
