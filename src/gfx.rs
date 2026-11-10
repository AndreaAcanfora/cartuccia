//! Pixel art in the source becomes Game Boy tiles: 8x8 pixels, 2 bits each, stored as
//! 16 bytes (for each row, the low bits of its 8 pixels, then the high bits).

use crate::diag::{Error, Result, Span};

pub type Tile = [u8; 16];

/// Parses a `[[ ... ]]` block. Pixels: `.` or `0` white, `1` light grey, `2` dark grey,
/// `3` or `#` black. Width and height must be multiples of 8; tiles come out row by row.
pub fn parse_art(art: &str, span: Span) -> Result<Vec<Tile>> {
    let rows: Vec<&str> = art
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let bad = |msg: String| Error::new(span, msg);
    if rows.is_empty() {
        return Err(bad("this graphic has no pixels".into()));
    }
    let width = rows[0].chars().count();
    if width == 0 || !width.is_multiple_of(8) || !rows.len().is_multiple_of(8) {
        return Err(bad(format!(
            "a graphic is made of 8x8 tiles: this one is {}x{} pixels",
            width,
            rows.len()
        )));
    }
    let mut pixels = Vec::with_capacity(width * rows.len());
    for (y, row) in rows.iter().enumerate() {
        if row.chars().count() != width {
            return Err(bad(format!(
                "row {} is {} pixels wide, the first row is {}",
                y + 1,
                row.chars().count(),
                width
            )));
        }
        for ch in row.chars() {
            pixels.push(match ch {
                '.' | '0' => 0u8,
                '1' => 1,
                '2' => 2,
                '3' | '#' => 3,
                other => {
                    return Err(bad(format!("`{other}` is not a pixel: use . 1 2 3 (or #)")));
                }
            });
        }
    }
    let mut tiles = Vec::new();
    for ty in 0..rows.len() / 8 {
        for tx in 0..width / 8 {
            let mut tile = [0u8; 16];
            for y in 0..8 {
                for x in 0..8 {
                    let p = pixels[(ty * 8 + y) * width + tx * 8 + x];
                    let bit = 7 - x;
                    tile[y * 2] |= (p & 1) << bit;
                    tile[y * 2 + 1] |= (p >> 1) << bit;
                }
            }
            tiles.push(tile);
        }
    }
    Ok(tiles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_2bpp() {
        let art = "3.......\n.2......\n..1.....\n........\n........\n........\n........\n#######.";
        let t = parse_art(art, Span::default()).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(&t[0][0..6], &[0x80, 0x80, 0x00, 0x40, 0x20, 0x00]);
        assert_eq!(&t[0][14..16], &[0xFE, 0xFE]);
    }

    #[test]
    fn rejects_bad_sizes() {
        assert!(parse_art("...", Span::default()).is_err());
    }
}
