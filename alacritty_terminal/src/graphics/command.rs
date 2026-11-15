//! Parsing of kitty graphics protocol control data.

/// A parsed kitty graphics command.
///
/// Field names follow the single letter keys of the protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// `a`: action.
    pub action: u8,
    /// `q`: quietness (0 = all responses, 1 = errors only, 2 = nothing).
    pub quiet: u32,
    /// `f`: pixel format (24, 32 or 100).
    pub format: u32,
    /// `t`: transmission medium.
    pub medium: u8,
    /// `o`: compression.
    pub compression: Option<u8>,
    /// `m`: whether more chunks follow.
    pub more: bool,
    /// `i`: image id.
    pub id: u32,
    /// `I`: image number.
    pub number: u32,
    /// `p`: placement id.
    pub placement_id: u32,
    /// `s`: image width in pixels.
    pub width: u32,
    /// `v`: image height in pixels.
    pub height: u32,
    /// `S`: size of data to read from a file.
    pub size: usize,
    /// `O`: offset of data to read from a file.
    pub offset: usize,
    /// `x`, `y`, `w`, `h`: source rectangle (and parameters for some deletions).
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// `X`, `Y`: pixel offsets inside the first cell.
    pub x_offset: u32,
    pub y_offset: u32,
    /// `c`, `r`: number of columns and rows to display the image over.
    pub columns: u32,
    pub rows: u32,
    /// `C`: cursor movement policy (1 = do not move the cursor).
    pub no_cursor_move: bool,
    /// `z`: z-index.
    pub z: i32,
    /// `d`: what to delete.
    pub delete: u8,
    /// `U`: create a virtual placement for Unicode placeholders.
    pub virtual_placement: bool,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            action: b't',
            quiet: 0,
            format: 32,
            medium: b'd',
            compression: None,
            more: false,
            id: 0,
            number: 0,
            placement_id: 0,
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            x_offset: 0,
            y_offset: 0,
            columns: 0,
            rows: 0,
            no_cursor_move: false,
            z: 0,
            delete: b'a',
            virtual_placement: false,
        }
    }
}

/// Parse the control data and payload of a graphics command.
///
/// The input excludes the leading `G`.
pub fn parse(data: &[u8]) -> Option<(Command, &[u8])> {
    let (control, payload) = match data.iter().position(|&b| b == b';') {
        Some(pos) => (&data[..pos], &data[pos + 1..]),
        None => (data, &[][..]),
    };

    let mut cmd = Command::default();

    for pair in control.split(|&b| b == b',') {
        if pair.is_empty() {
            continue;
        }

        let (key, value) = match pair {
            [key, b'=', value @ ..] => (*key, value),
            _ => return None,
        };

        match key {
            b'a' => cmd.action = char_value(value)?,
            b't' => cmd.medium = char_value(value)?,
            b'o' => cmd.compression = Some(char_value(value)?),
            b'd' => cmd.delete = char_value(value)?,
            b'q' => cmd.quiet = uint(value)?,
            b'f' => cmd.format = uint(value)?,
            b'm' => cmd.more = uint(value)? == 1,
            b'i' => cmd.id = uint(value)?,
            b'I' => cmd.number = uint(value)?,
            b'p' => cmd.placement_id = uint(value)?,
            b's' => cmd.width = uint(value)?,
            b'v' => cmd.height = uint(value)?,
            b'S' => cmd.size = uint(value)? as usize,
            b'O' => cmd.offset = uint(value)? as usize,
            b'x' => cmd.x = uint(value)?,
            b'y' => cmd.y = uint(value)?,
            b'w' => cmd.w = uint(value)?,
            b'h' => cmd.h = uint(value)?,
            b'X' => cmd.x_offset = uint(value)?,
            b'Y' => cmd.y_offset = uint(value)?,
            b'c' => cmd.columns = uint(value)?,
            b'r' => cmd.rows = uint(value)?,
            b'C' => cmd.no_cursor_move = uint(value)? == 1,
            b'U' => cmd.virtual_placement = uint(value)? == 1,
            b'z' => cmd.z = int(value)?,
            // Unsupported keys (animation, relative placements, ...) are ignored.
            _ => (),
        }
    }

    Some((cmd, payload))
}

fn char_value(value: &[u8]) -> Option<u8> {
    match value {
        [c] => Some(*c),
        _ => None,
    }
}

fn uint(value: &[u8]) -> Option<u32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn int(value: &[u8]) -> Option<i32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let (cmd, payload) = parse(b"a=T,f=100,i=31,q=2,z=-5;iVBORw0K").unwrap();
        assert_eq!(cmd.action, b'T');
        assert_eq!(cmd.format, 100);
        assert_eq!(cmd.id, 31);
        assert_eq!(cmd.quiet, 2);
        assert_eq!(cmd.z, -5);
        assert_eq!(payload, b"iVBORw0K");
    }

    #[test]
    fn parse_defaults() {
        let (cmd, payload) = parse(b"m=1").unwrap();
        assert_eq!(cmd.action, b't');
        assert_eq!(cmd.format, 32);
        assert_eq!(cmd.medium, b'd');
        assert!(cmd.more);
        assert!(payload.is_empty());
    }

    #[test]
    fn parse_invalid() {
        assert!(parse(b"a=Tx").is_none());
        assert!(parse(b"i=abc").is_none());
    }
}
