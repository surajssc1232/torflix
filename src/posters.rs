//! Movie covers in the terminal.
//!
//! Every character cell is drawn as `▀`: the foreground colour paints the top
//! half of the cell, the background the bottom. One cell therefore carries two
//! pixels stacked vertically, which is enough to recognise a poster and needs
//! no terminal graphics protocol — it works anywhere 24-bit colour does.
//!
//! Covers are fetched once and kept as JPEG bytes in the cache directory, so a
//! second look at the same title costs nothing.

use std::path::PathBuf;

pub type Rgb = (u8, u8, u8);

/// A decoded cover: `rows[y][x]` is the (top, bottom) colour of one cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Poster {
    pub width: u16,
    pub height: u16,
    pub rows: Vec<Vec<(Rgb, Rgb)>>,
}

/// Cells tall for a cover `width` cells wide, keeping a poster's 2:3 shape.
/// A cell is about twice as tall as it is wide, and holds two pixels, so its
/// two pixels are roughly square.
pub fn height_for(width: u16) -> u16 {
    ((width as f32 * 0.75).round() as u16).max(1)
}

fn cache_path(url: &str) -> PathBuf {
    crate::config::cache_dir()
        .join("posters")
        .join(format!("{:016x}.img", crate::config::stable_hash(url)))
}

/// The cover's bytes, from disk when they've been fetched before.
fn bytes(url: &str) -> Option<Vec<u8>> {
    let path = cache_path(url);
    if let Ok(b) = std::fs::read(&path) {
        if !b.is_empty() {
            return Some(b);
        }
    }
    let resp = minreq::get(url).with_timeout(20).send().ok()?;
    if resp.status_code >= 400 {
        return None;
    }
    let body = resp.as_bytes().to_vec();
    if body.is_empty() {
        return None;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&path, &body).ok();
    Some(body)
}

/// Fetch and decode a cover to a `width` × `height` grid of cells. Blocking —
/// call it off the draw thread.
pub fn load(url: &str, width: u16, height: u16) -> Option<Poster> {
    decode(&bytes(url)?, width, height)
}

fn decode(raw: &[u8], width: u16, height: u16) -> Option<Poster> {
    if width == 0 || height == 0 {
        return None;
    }
    let img = image::load_from_memory(raw).ok()?;
    // Two pixels per cell, stacked.
    let small = img
        .resize_exact(width as u32, height as u32 * 2, image::imageops::FilterType::Triangle)
        .to_rgb8();
    let rows = (0..height)
        .map(|y| {
            (0..width)
                .map(|x| {
                    let top = small.get_pixel(x as u32, y as u32 * 2).0;
                    let bottom = small.get_pixel(x as u32, y as u32 * 2 + 1).0;
                    ((top[0], top[1], top[2]), (bottom[0], bottom[1], bottom[2]))
                })
                .collect()
        })
        .collect();
    Some(Poster { width, height, rows })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2×4 image: red top half, blue bottom half, as PNG.
    fn sample() -> Vec<u8> {
        let mut img = image::RgbImage::new(2, 4);
        for (_, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgb(if y < 2 { [255, 0, 0] } else { [0, 0, 255] });
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn each_cell_carries_two_stacked_pixels() {
        let p = decode(&sample(), 2, 2).expect("decoded");
        assert_eq!((p.width, p.height), (2, 2));
        assert_eq!(p.rows.len(), 2);
        assert_eq!(p.rows[0].len(), 2);
        // Top row of cells is the red half, bottom row the blue one.
        assert_eq!(p.rows[0][0], ((255, 0, 0), (255, 0, 0)));
        assert_eq!(p.rows[1][1], ((0, 0, 255), (0, 0, 255)));
    }

    #[test]
    fn a_zero_sized_cover_is_not_drawn() {
        assert!(decode(&sample(), 0, 4).is_none());
        assert!(decode(&sample(), 4, 0).is_none());
    }

    #[test]
    fn rubbish_bytes_are_not_a_cover() {
        assert!(decode(b"definitely not an image", 4, 4).is_none());
    }

    #[test]
    fn covers_keep_a_posters_shape() {
        assert_eq!(height_for(16), 12);
        assert_eq!(height_for(20), 15);
        assert_eq!(height_for(0), 1);
    }
}
