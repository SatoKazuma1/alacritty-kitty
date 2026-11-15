//! Loading of transmitted image data.

use std::fmt;
use std::io::Read;

use super::command::Command;

/// Maximum width or height of an image in pixels.
pub const MAX_IMAGE_DIMENSION: u32 = 10_000;

/// Maximum size of raw (compressed or file) data read for a single image.
const MAX_DATA_SIZE: usize = 400 * 1024 * 1024;

/// Error reported back to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }

    pub fn inval(message: impl Into<String>) -> Self {
        Self::new("EINVAL", message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.code, self.message)
    }
}

/// Decoded RGBA pixels.
#[derive(Debug)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Read the raw bytes of a transmission.
///
/// For direct transmission the payload is the (already base64 decoded) data. For the other
/// mediums it is the path or shared memory name.
pub fn read_medium(cmd: &Command, payload: Vec<u8>) -> Result<Vec<u8>, Error> {
    match cmd.medium {
        b'd' => Ok(payload),
        #[cfg(not(target_family = "wasm"))]
        b'f' | b't' => read_file(cmd, &payload),
        #[cfg(unix)]
        b's' => read_shm(cmd, &payload),
        _ => Err(Error::inval("unsupported transmission medium")),
    }
}

#[cfg(not(target_family = "wasm"))]
fn read_file(cmd: &Command, payload: &[u8]) -> Result<Vec<u8>, Error> {
    use std::fs::{self, File};
    use std::io::{Seek, SeekFrom};
    use std::path::Path;

    let path = std::str::from_utf8(payload).map_err(|_| Error::inval("invalid file path"))?;
    let path = Path::new(path);

    // Refuse reading from sensitive pseudo filesystems.
    let forbidden = ["/proc", "/sys", "/dev"];
    let in_shm = path.starts_with("/dev/shm");
    if !path.is_absolute() || (!in_shm && forbidden.iter().any(|dir| path.starts_with(dir))) {
        return Err(Error::inval("refusing to read from this path"));
    }

    let metadata = fs::metadata(path).map_err(|err| Error::new("ENOENT", err.to_string()))?;
    if !metadata.is_file() {
        return Err(Error::inval("not a regular file"));
    }

    let mut file = File::open(path).map_err(|err| Error::new("ENOENT", err.to_string()))?;
    let file_len = metadata.len() as usize;

    let offset = cmd.offset.min(file_len);
    let len = if cmd.size == 0 { file_len - offset } else { cmd.size.min(file_len - offset) };
    if len > MAX_DATA_SIZE {
        return Err(Error::inval("file too large"));
    }

    let mut data = vec![0; len];
    let result = file
        .seek(SeekFrom::Start(offset as u64))
        .and_then(|_| file.read_exact(&mut data))
        .map_err(|err| Error::new("EBADF", err.to_string()));

    // Remove temporary files once they've been read.
    if cmd.medium == b't' && is_temp_file(path) {
        let _ = fs::remove_file(path);
    }

    result.map(|_| data)
}

#[cfg(not(target_family = "wasm"))]
fn is_temp_file(path: &std::path::Path) -> bool {
    let name_ok = path.to_string_lossy().contains("tty-graphics-protocol");
    let tmp = std::env::temp_dir();
    let dir_ok = ["/tmp", "/dev/shm", "/var/tmp"]
        .iter()
        .map(std::path::Path::new)
        .chain(std::iter::once(tmp.as_path()))
        .any(|dir| path.starts_with(dir));
    name_ok && dir_ok
}

#[cfg(unix)]
fn read_shm(cmd: &Command, payload: &[u8]) -> Result<Vec<u8>, Error> {
    use std::ffi::CString;

    let name = CString::new(payload).map_err(|_| Error::inval("invalid shm name"))?;

    // SAFETY: All pointers are valid for the duration of the calls and the mapping is only
    // accessed within its bounds.
    unsafe {
        let fd = libc::shm_open(name.as_ptr(), libc::O_RDONLY, 0);
        if fd < 0 {
            return Err(Error::new("ENOENT", "failed to open shared memory"));
        }

        let mut stat: libc::stat = std::mem::zeroed();
        if libc::fstat(fd, &mut stat) != 0 {
            libc::close(fd);
            libc::shm_unlink(name.as_ptr());
            return Err(Error::new("EBADF", "failed to stat shared memory"));
        }

        let total = stat.st_size as usize;
        let offset = cmd.offset.min(total);
        let len = if cmd.size == 0 { total - offset } else { cmd.size.min(total - offset) };
        if len > MAX_DATA_SIZE || total == 0 {
            libc::close(fd);
            libc::shm_unlink(name.as_ptr());
            return Err(Error::inval("invalid shared memory size"));
        }

        let ptr = libc::mmap(std::ptr::null_mut(), total, libc::PROT_READ, libc::MAP_SHARED, fd, 0);
        libc::close(fd);

        let result = if ptr == libc::MAP_FAILED {
            Err(Error::new("EBADF", "failed to map shared memory"))
        } else {
            let slice = std::slice::from_raw_parts(ptr as *const u8, total);
            let data = slice[offset..offset + len].to_vec();
            libc::munmap(ptr, total);
            Ok(data)
        };

        libc::shm_unlink(name.as_ptr());

        result
    }
}

/// Decompress the data, if necessary.
pub fn decompress(cmd: &Command, data: Vec<u8>) -> Result<Vec<u8>, Error> {
    match cmd.compression {
        None => Ok(data),
        Some(b'z') => {
            let mut out = Vec::new();
            flate2::read::ZlibDecoder::new(&data[..])
                .take(MAX_DATA_SIZE as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|err| Error::inval(format!("zlib decompression failed: {err}")))?;
            if out.len() > MAX_DATA_SIZE {
                return Err(Error::inval("decompressed data too large"));
            }
            Ok(out)
        },
        Some(_) => Err(Error::inval("unsupported compression")),
    }
}

/// Convert the transmitted data into RGBA pixels.
pub fn decode(cmd: &Command, data: Vec<u8>) -> Result<Pixels, Error> {
    match cmd.format {
        24 | 32 => {
            let (width, height) = (cmd.width, cmd.height);
            check_dimensions(width, height)?;

            let bpp = if cmd.format == 24 { 3 } else { 4 };
            let expected = width as usize * height as usize * bpp;
            if data.len() < expected {
                return Err(Error::new(
                    "ENODATA",
                    format!("insufficient image data: {} < {expected}", data.len()),
                ));
            }

            let rgba = if bpp == 4 {
                let mut data = data;
                data.truncate(expected);
                data
            } else {
                let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
                for px in data[..expected].chunks_exact(3) {
                    rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
                rgba
            };

            Ok(Pixels { width, height, rgba })
        },
        100 => decode_png(&data),
        _ => Err(Error::inval("unsupported format")),
    }
}

fn check_dimensions(width: u32, height: u32) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::inval("image dimensions must be specified"));
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(Error::inval("image too large"));
    }
    Ok(())
}

fn decode_png(data: &[u8]) -> Result<Pixels, Error> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader =
        decoder.read_info().map_err(|err| Error::new("EBADPNG", format!("{err}")))?;

    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    check_dimensions(width, height)?;

    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|err| Error::new("EBADPNG", format!("{err}")))?;
    buf.truncate(info.buffer_size());

    let pixels = width as usize * height as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(pixels * 4);
            for px in buf.chunks_exact(3) {
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            rgba
        },
        png::ColorType::Grayscale => {
            let mut rgba = Vec::with_capacity(pixels * 4);
            for &v in &buf {
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
            rgba
        },
        png::ColorType::GrayscaleAlpha => {
            let mut rgba = Vec::with_capacity(pixels * 4);
            for px in buf.chunks_exact(2) {
                rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            rgba
        },
        png::ColorType::Indexed => return Err(Error::new("EBADPNG", "unexpanded palette")),
    };

    if rgba.len() < pixels * 4 {
        return Err(Error::new("EBADPNG", "truncated image"));
    }

    Ok(Pixels { width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_rgb() {
        let cmd = Command { format: 24, width: 2, height: 1, ..Default::default() };
        let pixels = decode(&cmd, vec![1, 2, 3, 4, 5, 6]).unwrap();
        assert_eq!(pixels.rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn decode_insufficient() {
        let cmd = Command { format: 32, width: 2, height: 2, ..Default::default() };
        assert_eq!(decode(&cmd, vec![0; 4]).unwrap_err().code, "ENODATA");
    }

    #[test]
    fn decode_png_roundtrip() {
        let mut png_data = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_data, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 0, 255, 0]).unwrap();
        }

        let cmd = Command { format: 100, ..Default::default() };
        let pixels = decode(&cmd, png_data).unwrap();
        assert_eq!((pixels.width, pixels.height), (2, 1));
        assert_eq!(pixels.rgba, vec![255, 0, 0, 255, 0, 255, 0, 255]);
    }

    #[test]
    fn zlib() {
        use std::io::Write;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(b"hello").unwrap();
        let compressed = encoder.finish().unwrap();

        let cmd = Command { compression: Some(b'z'), ..Default::default() };
        assert_eq!(decompress(&cmd, compressed).unwrap(), b"hello");
    }
}
