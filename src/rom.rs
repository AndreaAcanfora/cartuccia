//! The cartridge image: 32 KB, no memory bank controller, with the header the boot ROM checks.

use crate::diag::{Error, Span};

pub const ROM_SIZE: usize = 0x8000;

/// The logo the boot ROM compares before it starts a cartridge.
const LOGO: [u8; 48] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// Pads the assembled program to 32 KB and fills in the header.
pub fn build(mut bytes: Vec<u8>, title: &str) -> Result<Vec<u8>, Error> {
    if bytes.len() > ROM_SIZE {
        return Err(Error::new(
            Span {
                line: 1,
                col: 1,
                ..Span::default()
            },
            format!(
                "the game is {} bytes: a cartridge without a bank controller holds {}",
                bytes.len(),
                ROM_SIZE
            ),
        ));
    }
    bytes.resize(ROM_SIZE, 0xFF);
    bytes[0x104..0x134].copy_from_slice(&LOGO);
    let mut name = [0u8; 15];
    for (k, c) in title
        .bytes()
        .filter(|c| c.is_ascii_graphic() || *c == b' ')
        .map(|c| c.to_ascii_uppercase())
        .take(15)
        .enumerate()
    {
        name[k] = c;
    }
    bytes[0x134..0x143].copy_from_slice(&name);
    bytes[0x143] = 0x00; // original Game Boy
    bytes[0x144] = b'0';
    bytes[0x145] = b'0';
    bytes[0x146] = 0x00; // no Super Game Boy functions
    bytes[0x147] = 0x00; // ROM only
    bytes[0x148] = 0x00; // 32 KB
    bytes[0x149] = 0x00; // no cartridge RAM
    bytes[0x14A] = 0x01; // sold outside Japan
    bytes[0x14B] = 0x33; // see the new licensee code
    bytes[0x14C] = 0x00; // version
    let mut check = 0u8;
    for b in &bytes[0x134..0x14D] {
        check = check.wrapping_sub(*b).wrapping_sub(1);
    }
    bytes[0x14D] = check;
    bytes[0x14E] = 0;
    bytes[0x14F] = 0;
    let sum: u16 = bytes.iter().fold(0u16, |s, b| s.wrapping_add(*b as u16));
    bytes[0x14E..0x150].copy_from_slice(&sum.to_be_bytes());
    Ok(bytes)
}
