//! Sixel: pictures made of escape codes.
//!
//! A sixel image is written in bands six pixels tall. Each character carries
//! one column of that band — six stacked pixels as six bits, offset by `?` —
//! and a band is written once per colour it uses, so colours stack up over the
//! same six rows. `$` returns to the start of the band for the next colour,
//! `-` moves down to the next band.
//!
//! Colours have to be declared up front and there are only so many registers,
//! so the picture is reduced to at most [`MAX_COLORS`] of its own colours by
//! median cut: the colour cube is split along its longest side, over and over,
//! and each resulting box becomes the average of the pixels inside it. Picking
//! the palette out of the picture rather than from a fixed cube is what keeps
//! a poster's skin tones and dark scenes from banding.

use image::RgbImage;

/// Palette size. Terminals guarantee 256 registers; staying under that leaves
/// room and keeps each band's colour runs short.
const MAX_COLORS: usize = 128;
/// Colour bits kept per channel when counting. 5 bits is 32³ boxes, fine
/// enough to split on and small enough to walk quickly.
const BITS: u32 = 5;
const LEVELS: usize = 1 << BITS;

/// Encode an image as a sixel escape sequence, ready to write at the cursor.
pub fn encode(img: &RgbImage) -> String {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return String::new();
    }
    let hist = histogram(img);
    let palette = median_cut(&hist, MAX_COLORS);
    let lookup = nearest_table(&palette);
    let indexed: Vec<u8> = img.pixels().map(|p| lookup[bin(p.0)]).collect();

    // P1 = 0: pixel aspect 1:1. P2 = 1: zero bits are left alone rather than
    // painted as background, so the terminal's own colours show through.
    let mut out = format!("\x1bP0;1;0q\"1;1;{w};{h}");
    for (i, c) in palette.iter().enumerate() {
        // Sixel colour components are percentages, not bytes.
        out.push_str(&format!(
            "#{};2;{};{};{}",
            i,
            pct(c[0]),
            pct(c[1]),
            pct(c[2])
        ));
    }
    for band in 0..h.div_ceil(6) {
        let top = band * 6;
        let rows = 6.min(h - top) as usize;
        // Which colour sits in each of this band's six rows, per column.
        let mut used = vec![false; palette.len()];
        for row in 0..rows {
            let start = ((top as usize + row) * w as usize) as usize;
            for x in 0..w as usize {
                used[indexed[start + x] as usize] = true;
            }
        }
        let mut first = true;
        for (colour, _) in used.iter().enumerate().filter(|(_, u)| **u) {
            if !first {
                out.push('$');
            }
            first = false;
            out.push_str(&format!("#{colour}"));
            let mut bits = vec![0u8; w as usize];
            for (row, bit) in (0..rows).zip(0..) {
                let start = (top as usize + row) * w as usize;
                for x in 0..w as usize {
                    if indexed[start + x] as usize == colour {
                        bits[x] |= 1 << bit;
                    }
                }
            }
            run_length(&bits, &mut out);
        }
        out.push('-');
    }
    out.push_str("\x1b\\");
    out
}

/// A byte as a sixel colour percentage.
fn pct(v: u8) -> u32 {
    (v as u32 * 100 + 127) / 255
}

/// Write the band's columns, collapsing repeats into `!count`.
fn run_length(bits: &[u8], out: &mut String) {
    let mut i = 0;
    while i < bits.len() {
        let mut n = 1;
        while i + n < bits.len() && bits[i + n] == bits[i] {
            n += 1;
        }
        let ch = (b'?' + bits[i]) as char;
        // `!3?` is longer than `???`, so only wrap runs that actually save.
        if n > 3 {
            out.push_str(&format!("!{n}{ch}"));
        } else {
            for _ in 0..n {
                out.push(ch);
            }
        }
        i += n;
    }
}

/// The index of a colour among the 32³ counting boxes.
fn bin(c: [u8; 3]) -> usize {
    let q = |v: u8| (v >> (8 - BITS)) as usize;
    (q(c[0]) << (2 * BITS)) | (q(c[1]) << BITS) | q(c[2])
}

/// The colour a box stands for. The kept bits are repeated into the dropped
/// ones rather than padded, so the ends of the range survive the round trip:
/// 31 is 255, not 252, and black stays black.
fn unbin(i: usize) -> [u8; 3] {
    let mask = LEVELS - 1;
    let up = |v: usize| ((v << (8 - BITS)) | (v >> (2 * BITS - 8))) as u8;
    [
        up((i >> (2 * BITS)) & mask),
        up((i >> BITS) & mask),
        up(i & mask),
    ]
}

/// How many pixels fall in each colour box, as (box, count), occupied only.
fn histogram(img: &RgbImage) -> Vec<(usize, u32)> {
    let mut counts = vec![0u32; LEVELS * LEVELS * LEVELS];
    for p in img.pixels() {
        counts[bin(p.0)] += 1;
    }
    counts
        .into_iter()
        .enumerate()
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// Split the colour space until there are `want` boxes, always splitting the
/// box with the longest side, then average each one.
fn median_cut(hist: &[(usize, u32)], want: usize) -> Vec<[u8; 3]> {
    if hist.is_empty() {
        return vec![[0, 0, 0]];
    }
    let mut boxes: Vec<Vec<(usize, u32)>> = vec![hist.to_vec()];
    while boxes.len() < want {
        // The box with the longest side is the one worth splitting.
        let Some((which, axis, _)) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| {
                let (axis, span) = longest_side(b);
                (i, axis, span)
            })
            .max_by_key(|&(_, _, span)| span)
        else {
            break;
        };
        let mut b = boxes.swap_remove(which);
        b.sort_by_key(|&(i, _)| unbin(i)[axis]);
        // Split where half the pixels lie, not half the colours, so a wide
        // flat background doesn't take the same share as a face.
        let half = b.iter().map(|&(_, n)| n as u64).sum::<u64>() / 2;
        let mut seen = 0u64;
        let mut at = 0;
        for (i, &(_, n)) in b.iter().enumerate() {
            seen += n as u64;
            if seen > half {
                at = i.max(1);
                break;
            }
        }
        let rest = b.split_off(at.min(b.len() - 1).max(1));
        boxes.push(b);
        boxes.push(rest);
    }
    boxes.iter().map(|b| average(b)).collect()
}

/// The axis this box spans most, and by how much.
fn longest_side(b: &[(usize, u32)]) -> (usize, u8) {
    let mut lo = [255u8; 3];
    let mut hi = [0u8; 3];
    for &(i, _) in b {
        let c = unbin(i);
        for a in 0..3 {
            lo[a] = lo[a].min(c[a]);
            hi[a] = hi[a].max(c[a]);
        }
    }
    (0..3)
        .map(|a| (a, hi[a] - lo[a]))
        .max_by_key(|&(_, span)| span)
        .unwrap_or((0, 0))
}

/// The box's colour, weighted by how many pixels each shade holds.
fn average(b: &[(usize, u32)]) -> [u8; 3] {
    let mut sum = [0u64; 3];
    let mut total = 0u64;
    for &(i, n) in b {
        let c = unbin(i);
        for a in 0..3 {
            sum[a] += c[a] as u64 * n as u64;
        }
        total += n as u64;
    }
    if total == 0 {
        return [0, 0, 0];
    }
    [
        (sum[0] / total) as u8,
        (sum[1] / total) as u8,
        (sum[2] / total) as u8,
    ]
}

/// The closest palette entry for every colour box, worked out once so each
/// pixel costs a single lookup.
fn nearest_table(palette: &[[u8; 3]]) -> Vec<u8> {
    (0..LEVELS * LEVELS * LEVELS)
        .map(|i| {
            let c = unbin(i);
            palette
                .iter()
                .enumerate()
                .min_by_key(|(_, p)| {
                    (0..3)
                        .map(|a| {
                            let d = p[a] as i32 - c[a] as i32;
                            d * d
                        })
                        .sum::<i32>()
                })
                .map(|(i, _)| i as u8)
                .unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 3]) -> RgbImage {
        RgbImage::from_pixel(w, h, image::Rgb(c))
    }

    #[test]
    fn a_solid_image_is_one_colour_and_one_band() {
        let s = encode(&solid(8, 6, [255, 0, 0]));
        assert!(s.starts_with("\x1bP0;1;0q\"1;1;8;6"), "{s}");
        assert!(s.ends_with("\x1b\\"));
        // One colour register, declared as percentages.
        assert!(s.contains("#0;2;100;0;0"), "{s}");
        assert!(!s.contains("#1;2;"), "{s}");
        // Six full rows is bit pattern 63, written as `~`, run-length encoded.
        assert!(s.contains("!8~"), "{s}");
        assert_eq!(s.matches('-').count(), 1);
    }

    #[test]
    fn short_runs_are_written_out_rather_than_counted() {
        let mut out = String::new();
        run_length(&[63, 63, 63, 0], &mut out);
        assert_eq!(out, "~~~?");
        let mut out = String::new();
        run_length(&[63; 5], &mut out);
        assert_eq!(out, "!5~");
    }

    #[test]
    fn a_tall_image_is_split_into_bands_of_six() {
        let s = encode(&solid(2, 13, [0, 0, 0]));
        assert_eq!(s.matches('-').count(), 3, "13 rows is three bands");
    }

    #[test]
    fn two_colours_share_a_band() {
        let mut img = solid(4, 6, [255, 0, 0]);
        for y in 0..6 {
            img.put_pixel(2, y, image::Rgb([0, 0, 255]));
        }
        let s = encode(&img);
        // Both colours are declared, and the band is written once per colour.
        assert!(s.contains("#0;2;") && s.contains("#1;2;") && !s.contains("#2;2;"), "{s}");
        assert_eq!(s.matches('$').count(), 1, "{s}");
        // Red either side of a blue column, in one band.
        assert!(s.contains("~~?~$") && s.contains("??~?"), "{s}");
    }

    #[test]
    fn the_palette_comes_out_of_the_picture() {
        // Four distinct colours, each a quarter of the image.
        let mut img = RgbImage::new(4, 1);
        let want = [[200, 10, 10], [10, 200, 10], [10, 10, 200], [200, 200, 10]];
        for (x, c) in want.iter().enumerate() {
            img.put_pixel(x as u32, 0, image::Rgb(*c));
        }
        let palette = median_cut(&histogram(&img), MAX_COLORS);
        assert_eq!(palette.len(), 4);
        for c in want {
            let near = palette
                .iter()
                .any(|p| (0..3).all(|a| (p[a] as i32 - c[a] as i32).abs() <= 8));
            assert!(near, "{c:?} missing from {palette:?}");
        }
    }

    #[test]
    fn the_palette_never_outgrows_the_registers() {
        let mut img = RgbImage::new(64, 64);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([x as u8 * 4, y as u8 * 4, (x + y) as u8 * 2]);
        }
        assert!(median_cut(&histogram(&img), MAX_COLORS).len() <= MAX_COLORS);
    }

    /// Encode a real cover and write both the sixel and the pixels it was made
    /// from, so the two can be compared outside the test.
    /// `TORFLIX_SIXEL_CHECK=<jpeg or png> cargo test -- --ignored sixel`
    #[test]
    #[ignore = "needs TORFLIX_SIXEL_CHECK=<image path>"]
    fn renders_a_real_picture() {
        let path = std::env::var("TORFLIX_SIXEL_CHECK").expect("TORFLIX_SIXEL_CHECK");
        let raw_file = std::fs::read(&path).expect("read");
        let img = image::load_from_memory(&raw_file).expect("decode").to_rgb8();
        let dir = std::path::Path::new(&path).parent().expect("parent");
        let mut raw = format!("{} {}\n", img.width(), img.height()).into_bytes();
        raw.extend_from_slice(img.as_raw());
        std::fs::write(dir.join("check.rgb"), raw).expect("write pixels");
        std::fs::write(dir.join("check.sixel"), encode(&img)).expect("write sixel");
    }

    #[test]
    fn an_empty_image_is_nothing_to_draw() {
        assert_eq!(encode(&RgbImage::new(0, 0)), "");
    }
}
