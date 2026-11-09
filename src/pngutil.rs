//! PNG decoding for Linux, where icons come as image files rather than as
//! resources inside executables.

use std::io::Read;

/// Decoded image: width, height and RGBA8 pixels, row by row.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Decode any PNG into RGBA8. Palette, grey and 16-bit images are normalised.
pub fn decode(reader: impl Read) -> Option<Rgba> {
    let mut decoder = png::Decoder::new(reader);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).ok()?;
    buffer.truncate(info.buffer_size());

    let pixels = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buffer.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };

    Some(Rgba {
        width: info.width,
        height: info.height,
        pixels,
    })
}

/// Volume11's own icon at 64 px, compiled into the binary.
pub const OWN_ICON_64: &[u8] = include_bytes!("../assets/linux/volume11-64.png");
/// The same icon at 32 px, for the tray.
pub const OWN_ICON_32: &[u8] = include_bytes!("../assets/linux/volume11-32.png");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_icons_decode() {
        for (bytes, size) in [(OWN_ICON_64, 64), (OWN_ICON_32, 32)] {
            let image = decode(bytes).expect("embedded icon decodes");
            assert_eq!((image.width, image.height), (size, size));
            assert_eq!(image.pixels.len(), (size * size * 4) as usize);
        }
    }
}
