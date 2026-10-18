//! The app icon, drawn in code: no image files and no image codec.
//!
//! A blue rounded square with a white download arrow, 4×4 supersampled so the
//! edges are smooth at tray sizes.

const BLUE: [u8; 3] = [0x25, 0x63, 0xEB];

/// Straight (non-premultiplied) RGBA, row by row.
pub fn rgba(size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    const SS: u32 = 4;
    for y in 0..size {
        for x in 0..size {
            let (mut bg, mut fg) = (0u32, 0u32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (x as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if in_rounded_square(u, v) {
                        bg += 1;
                        if in_arrow(u, v) {
                            fg += 1;
                        }
                    }
                }
            }
            let n = (SS * SS) as f32;
            let alpha = bg as f32 / n;
            // Within the square, mix white over blue by arrow coverage.
            let white = if bg > 0 { fg as f32 / bg as f32 } else { 0.0 };
            for c in BLUE {
                out.push((c as f32 + (255.0 - c as f32) * white).round() as u8);
            }
            out.push((alpha * 255.0).round() as u8);
        }
    }
    out
}

/// What the Linux tray wants: ARGB32, big-endian.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn argb(size: u32) -> Vec<u8> {
    let rgba = rgba(size);
    let (pixels, _) = rgba.as_chunks::<4>();
    pixels
        .iter()
        .flat_map(|&[r, g, b, a]| [a, r, g, b])
        .collect()
}

fn in_rounded_square(u: f32, v: f32) -> bool {
    const R: f32 = 0.22;
    let dx = (u - 0.5).abs() - (0.5 - R);
    let dy = (v - 0.5).abs() - (0.5 - R);
    if dx <= 0.0 || dy <= 0.0 {
        return dx <= R && dy <= R;
    }
    dx * dx + dy * dy <= R * R
}

fn in_arrow(u: f32, v: f32) -> bool {
    let shaft = (0.43..=0.57).contains(&u) && (0.18..=0.52).contains(&v);
    let head = in_triangle((u, v), (0.25, 0.46), (0.75, 0.46), (0.5, 0.72));
    let tray = (0.24..=0.76).contains(&u) && (0.79..=0.86).contains(&v);
    shaft || head || tray
}

fn in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let side = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| {
        (p.0 - r.0) * (q.1 - r.1) - (q.0 - r.0) * (p.1 - r.1)
    };
    let (d1, d2, d3) = (side(p, a, b), side(p, b, c), side(p, c, a));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_are_transparent_and_the_arrow_is_white() {
        let size = 32;
        let px = |img: &[u8], x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            [img[i], img[i + 1], img[i + 2], img[i + 3]]
        };
        let img = rgba(size);
        assert_eq!(img.len(), (size * size * 4) as usize);
        assert_eq!(px(&img, 0, 0)[3], 0, "rounded corner");
        assert_eq!(px(&img, 16, 10), [255, 255, 255, 255], "the arrow's shaft");
        assert_eq!(
            px(&img, 4, 16),
            [BLUE[0], BLUE[1], BLUE[2], 255],
            "plain background"
        );

        let argb = argb(size);
        assert_eq!(
            &argb[..4],
            &[0, BLUE[0], BLUE[1], BLUE[2]],
            "alpha first for the tray"
        );
    }
}
