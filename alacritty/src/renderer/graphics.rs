//! Rendering of kitty graphics protocol images.

use std::collections::HashMap;
use std::mem;

use log::warn;

use alacritty_terminal::graphics::{GraphicsDraw, ImageData};
use alacritty_terminal::grid::Dimensions;

use crate::display::SizeInfo;
use crate::gl::types::*;
use crate::renderer::shader::{ShaderProgram, ShaderVersion};
use crate::{gl, renderer};

const GRAPHICS_SHADER_V: &str = include_str!("../../res/graphics.v.glsl");
const GRAPHICS_SHADER_F: &str = include_str!("../../res/graphics.f.glsl");

/// Number of drawn frames after which unused textures are released.
const TEXTURE_TTL_FRAMES: u64 = 120;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
}

#[derive(Debug)]
struct Texture {
    id: GLuint,
    last_used: u64,
}

impl Drop for Texture {
    fn drop(&mut self) {
        unsafe { gl::DeleteTextures(1, &self.id) };
    }
}

#[derive(Debug)]
pub struct GraphicsRenderer {
    program: ShaderProgram,
    vao: GLuint,
    vbo: GLuint,
    textures: HashMap<u64, Texture>,
    vertices: Vec<Vertex>,
    frame: u64,
    max_texture_size: u32,
    mipmaps: bool,
}

impl GraphicsRenderer {
    pub fn new(shader_version: ShaderVersion) -> Result<Self, renderer::Error> {
        let program =
            ShaderProgram::new(shader_version, None, GRAPHICS_SHADER_V, GRAPHICS_SHADER_F)?;

        let mut vao: GLuint = 0;
        let mut vbo: GLuint = 0;
        let mut max_texture_size: GLint = 0;

        unsafe {
            gl::GetIntegerv(gl::MAX_TEXTURE_SIZE, &mut max_texture_size);

            gl::GenVertexArrays(1, &mut vao);
            gl::GenBuffers(1, &mut vbo);

            gl::BindVertexArray(vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            let stride = mem::size_of::<Vertex>() as i32;
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, stride, std::ptr::null());
            gl::EnableVertexAttribArray(0);
            gl::VertexAttribPointer(
                1,
                2,
                gl::FLOAT,
                gl::FALSE,
                stride,
                (mem::size_of::<f32>() * 2) as *const _,
            );
            gl::EnableVertexAttribArray(1);

            gl::BindVertexArray(0);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
        }

        // GLES2 attribute locations aren't fixed by the shader.
        if let ShaderVersion::Gles2 = shader_version {
            unsafe {
                gl::BindAttribLocation(program.id(), 0, c"aPos".as_ptr());
                gl::BindAttribLocation(program.id(), 1, c"aTexCoord".as_ptr());
                gl::LinkProgram(program.id());
            }
        }

        // Sampler always uses texture unit 0.
        unsafe {
            let location = gl::GetUniformLocation(program.id(), c"image".as_ptr());
            gl::UseProgram(program.id());
            gl::Uniform1i(location, 0);
            gl::UseProgram(0);
        }

        Ok(Self {
            program,
            vao,
            vbo,
            textures: HashMap::new(),
            vertices: Vec::new(),
            frame: 0,
            max_texture_size: max_texture_size.max(1024) as u32,
            mipmaps: matches!(shader_version, ShaderVersion::Glsl3),
        })
    }

    /// Mark the start of a new frame and release unused textures.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        let frame = self.frame;
        self.textures.retain(|_, texture| frame - texture.last_used <= TEXTURE_TTL_FRAMES);
    }

    /// Draw image quads.
    pub fn draw<'a>(
        &mut self,
        size_info: &SizeInfo,
        draws: impl Iterator<Item = &'a GraphicsDraw>,
    ) {
        let half_width = size_info.width() / 2.;
        let half_height = size_info.height() / 2.;
        let cell_width = size_info.cell_width();
        let cell_height = size_info.cell_height();
        let padding_x = size_info.padding_x();
        let padding_y = size_info.padding_y();

        let mut draws = draws.peekable();
        if draws.peek().is_none() {
            return;
        }

        // The text renderer caches its bound atlas texture, so it must be restored afterwards.
        let mut previous_texture: GLint = 0;

        unsafe {
            gl::ActiveTexture(gl::TEXTURE0);
            gl::GetIntegerv(gl::TEXTURE_BINDING_2D, &mut previous_texture);

            // Draw over the entire window, but clip to the terminal grid.
            gl::Viewport(0, 0, size_info.width() as i32, size_info.height() as i32);
            gl::Enable(gl::SCISSOR_TEST);
            let grid_height = size_info.screen_lines() as f32 * cell_height;
            let grid_width = size_info.columns() as f32 * cell_width;
            gl::Scissor(
                padding_x as i32,
                (size_info.height() - padding_y - grid_height) as i32,
                grid_width.ceil() as i32,
                grid_height.ceil() as i32,
            );
            gl::BlendFuncSeparate(
                gl::ONE,
                gl::ONE_MINUS_SRC_ALPHA,
                gl::ONE,
                gl::ONE_MINUS_SRC_ALPHA,
            );

            gl::UseProgram(self.program.id());
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::ActiveTexture(gl::TEXTURE0);
        }

        // Draws are batched by image, keeping the order of distinct z-indices.
        let mut current: Option<u64> = None;
        for draw in draws {
            if current != Some(draw.image.uid) {
                self.flush();
                let texture = self.texture(&draw.image);
                unsafe { gl::BindTexture(gl::TEXTURE_2D, texture) };
                current = Some(draw.image.uid);
            }

            let x = padding_x + draw.column as f32 * cell_width + draw.x;
            let y = padding_y + draw.line as f32 * cell_height + draw.y;

            let x0 = x / half_width - 1.;
            let x1 = (x + draw.width) / half_width - 1.;
            let y0 = -y / half_height + 1.;
            let y1 = -(y + draw.height) / half_height + 1.;

            let (w, h) = (draw.image.width as f32, draw.image.height as f32);
            let u0 = draw.source[0] / w;
            let u1 = (draw.source[0] + draw.source[2]) / w;
            let v0 = draw.source[1] / h;
            let v1 = (draw.source[1] + draw.source[3]) / h;

            let quad = [
                Vertex { x: x0, y: y0, u: u0, v: v0 },
                Vertex { x: x0, y: y1, u: u0, v: v1 },
                Vertex { x: x1, y: y0, u: u1, v: v0 },
                Vertex { x: x1, y: y1, u: u1, v: v1 },
            ];
            self.vertices
                .extend_from_slice(&[quad[0], quad[1], quad[2], quad[2], quad[3], quad[1]]);
        }
        self.flush();

        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, previous_texture as GLuint);
            gl::BindBuffer(gl::ARRAY_BUFFER, 0);
            gl::BindVertexArray(0);
            gl::UseProgram(0);
            gl::Disable(gl::SCISSOR_TEST);

            // Restore the text rendering state.
            gl::BlendFunc(gl::SRC1_COLOR, gl::ONE_MINUS_SRC1_COLOR);
        }
    }

    /// Draw all accumulated vertices.
    fn flush(&mut self) {
        if self.vertices.is_empty() {
            return;
        }

        unsafe {
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (self.vertices.len() * mem::size_of::<Vertex>()) as isize,
                self.vertices.as_ptr() as *const _,
                gl::STREAM_DRAW,
            );
            gl::DrawArrays(gl::TRIANGLES, 0, self.vertices.len() as i32);
        }

        self.vertices.clear();
    }

    /// Get the texture for an image, uploading it if necessary.
    fn texture(&mut self, image: &ImageData) -> GLuint {
        let frame = self.frame;
        if let Some(texture) = self.textures.get_mut(&image.uid) {
            texture.last_used = frame;
            return texture.id;
        }

        let max = self.max_texture_size;
        let scaled;
        let (width, height, pixels) = if image.width > max || image.height > max {
            warn!("Downscaling {}x{} image to fit GPU texture limits", image.width, image.height);
            scaled = downscale(image, max);
            (scaled.0, scaled.1, &scaled.2[..])
        } else {
            (image.width, image.height, &image.pixels[..])
        };

        let mut id: GLuint = 0;
        unsafe {
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                width as i32,
                height as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                pixels.as_ptr() as *const _,
            );

            let min_filter = if self.mipmaps {
                gl::GenerateMipmap(gl::TEXTURE_2D);
                gl::LINEAR_MIPMAP_LINEAR
            } else {
                gl::LINEAR
            };
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, min_filter as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
        }

        self.textures.insert(image.uid, Texture { id, last_used: frame });

        id
    }
}

impl Drop for GraphicsRenderer {
    fn drop(&mut self) {
        self.textures.clear();
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteVertexArrays(1, &self.vao);
        }
    }
}

/// Nearest neighbor downscale of an image to fit into `max`x`max` pixels.
fn downscale(image: &ImageData, max: u32) -> (u32, u32, Vec<u8>) {
    let scale = (max as f32 / image.width as f32).min(max as f32 / image.height as f32);
    let width = ((image.width as f32 * scale) as u32).clamp(1, max);
    let height = ((image.height as f32 * scale) as u32).clamp(1, max);

    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let src_y = (y as u64 * image.height as u64 / height as u64) as usize;
        for x in 0..width {
            let src_x = (x as u64 * image.width as u64 / width as u64) as usize;
            let offset = (src_y * image.width as usize + src_x) * 4;
            pixels.extend_from_slice(&image.pixels[offset..offset + 4]);
        }
    }

    (width, height, pixels)
}
