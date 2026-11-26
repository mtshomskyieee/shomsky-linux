//! Build-time image → colored ASCII for `:sine` brand panes.
//!
//! Usage:
//!   cargo run --manifest-path tools/img2ascii/Cargo.toml -- assets/brand
//!
//! Writes `*.txt` next to the source images. Each job can override the glyph
//! ramp and conversion knobs so faces/mugs/line-art read clearly.

use std::collections::VecDeque;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{GenericImageView, Rgba, RgbaImage};

/// Default dense → light ramp (index 0 = heaviest ink).
const DEFAULT_RAMP: &str = "█▓▒░#%@*+=:-. ";

/// How luminance maps to ink density.
#[derive(Clone, Copy, Debug)]
enum InkMode {
    /// Darker pixels → denser ink (photos on light/cream backgrounds).
    DarkInk,
    /// Brighter pixels → denser ink (neon / cyan line art on dark bg).
    BrightInk,
}

/// How studio backgrounds are separated from the subject.
#[derive(Clone, Copy, Debug)]
enum SubjectMode {
    /// Keep pixels whose color differs from corner-sampled bg.
    CornerDist,
    /// Flood-fill bg-like pixels from the image border; always keep the
    /// central ellipse so cream subjects on cream floors survive.
    FloodEllipse,
}

struct Job {
    src: &'static str,
    out: &'static str,
    cols: u32,
    rows: u32,
    ramp: &'static str,
    bg_threshold: f32,
    contrast: f32,
    ink: InkMode,
    /// Max-pool radius (0 = off). Thickens thin neon strokes before sampling.
    dilate: u32,
    subject: SubjectMode,
    /// Minimum normalized ink after contrast (keeps cream silhouettes solid).
    ink_floor: f32,
    /// Ellipse half-axes as fractions of width/height (FloodEllipse only).
    ellipse_rx: f32,
    ellipse_ry: f32,
    /// Vertical center bias (−0.5‥0.5); positive shifts ellipse downward.
    ellipse_cy: f32,
}

const JOBS: &[Job] = &[
    Job {
        src: "navi.png",
        out: "navi.txt",
        cols: 48,
        rows: 22,
        ramp: "████▓▒░#%@*+=:. ",
        bg_threshold: 22.0,
        contrast: 1.25,
        ink: InkMode::BrightInk,
        dilate: 2,
        subject: SubjectMode::CornerDist,
        ink_floor: 0.35,
        ellipse_rx: 0.45,
        ellipse_ry: 0.45,
        ellipse_cy: 0.0,
    },
    // Cream mug ≈ cream studio floor — corner distance alone eats the body.
    Job {
        src: "coffee-rest.jpg",
        out: "coffee-rest.txt",
        cols: 44,
        rows: 20,
        ramp: "█▓▒░#%@*+=:-. ",
        bg_threshold: 38.0,
        contrast: 1.45,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 0.48,
        ellipse_rx: 0.40,
        ellipse_ry: 0.46,
        ellipse_cy: 0.04,
    },
    Job {
        src: "coffee-busy.jpg",
        out: "coffee-busy.txt",
        cols: 44,
        rows: 20,
        ramp: "█▓▒░#%@*+=:-. ",
        bg_threshold: 38.0,
        contrast: 1.45,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 0.48,
        ellipse_rx: 0.42,
        ellipse_ry: 0.48,
        ellipse_cy: 0.02,
    },
    Job {
        src: "laptop-rest.jpg",
        out: "laptop-rest.txt",
        cols: 44,
        rows: 20,
        ramp: "█▓▒░%@#*+=:-. ",
        bg_threshold: 36.0,
        contrast: 1.4,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 0.42,
        ellipse_rx: 0.48,
        ellipse_ry: 0.42,
        ellipse_cy: 0.0,
    },
    Job {
        src: "laptop-busy.jpg",
        out: "laptop-busy.txt",
        cols: 44,
        rows: 20,
        ramp: "█▓▒░%@#*+=:-. ",
        bg_threshold: 36.0,
        contrast: 1.4,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 0.42,
        ellipse_rx: 0.48,
        ellipse_ry: 0.44,
        ellipse_cy: 0.0,
    },
];

fn main() {
    let dir = env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("assets/brand"));
    if !dir.is_dir() {
        eprintln!("img2ascii: not a directory: {}", dir.display());
        std::process::exit(1);
    }
    for job in JOBS {
        let src = dir.join(job.src);
        let out = dir.join(job.out);
        match convert(&src, job) {
            Ok(text) => {
                fs::write(&out, text).expect("write");
                println!(
                    "wrote {} ({}×{}, {:?}, {:?})",
                    out.display(),
                    job.cols,
                    job.rows,
                    job.ink,
                    job.subject
                );
            }
            Err(e) => {
                eprintln!("img2ascii: {}: {e}", src.display());
                std::process::exit(1);
            }
        }
    }
}

fn convert(path: &Path, job: &Job) -> Result<String, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    // Terminal cells are ~2× taller than wide: stretch width before resize.
    let (ow, oh) = img.dimensions();
    let stretched = img.resize_exact(ow.saturating_mul(2).max(1), oh.max(1), FilterType::Triangle);
    // Oversample, optional dilate, then down to cell grid — keeps thin neon lines.
    let over_w = job.cols.saturating_mul(3).max(job.cols);
    let over_h = job.rows.saturating_mul(3).max(job.rows);
    let over: RgbaImage = stretched
        .resize_exact(over_w, over_h, FilterType::CatmullRom)
        .to_rgba8();
    let over = if job.dilate > 0 {
        dilate_bright(&over, job.dilate)
    } else {
        over
    };
    let small: RgbaImage = image::imageops::resize(&over, job.cols, job.rows, FilterType::Triangle);

    let bg = sample_background(&small);
    let subject = match job.subject {
        SubjectMode::CornerDist => None,
        SubjectMode::FloodEllipse => Some(subject_mask_flood_ellipse(&small, bg, job)),
    };

    let ramp: Vec<char> = if job.ramp.is_empty() {
        DEFAULT_RAMP.chars().collect()
    } else {
        job.ramp.chars().collect()
    };
    let ramp_n = ramp.len().saturating_sub(1).max(1);

    let mut cells: Vec<Option<(u8, u8, u8, f32)>> =
        Vec::with_capacity((job.cols * job.rows) as usize);
    let mut strengths = Vec::new();
    for y in 0..job.rows {
        for x in 0..job.cols {
            let p = small.get_pixel(x, y).0;
            let (r, g, b, a) = (p[0], p[1], p[2], p[3]);
            let lum = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
            let keep = match (&subject, job.ink) {
                (Some(mask), _) => {
                    a >= 16 && mask[(y * job.cols + x) as usize]
                }
                (None, InkMode::DarkInk) => {
                    a >= 16 && color_dist([r, g, b], bg) >= job.bg_threshold
                }
                (None, InkMode::BrightInk) => {
                    a >= 16 && lum >= 0.18 && color_dist([r, g, b], bg) >= job.bg_threshold
                }
            };
            if !keep {
                cells.push(None);
                continue;
            }
            let strength = match job.ink {
                InkMode::DarkInk => 1.0 - lum, // darker → more ink
                InkMode::BrightInk => lum,     // brighter → more ink
            };
            cells.push(Some((r, g, b, strength)));
            strengths.push(strength);
        }
    }
    let (lo, hi) = percentile_bounds(&strengths, 0.05, 0.95);

    let mut out = String::new();
    let mut i = 0usize;
    for _y in 0..job.rows {
        for _x in 0..job.cols {
            match cells[i] {
                None => out.push(' '),
                Some((r, g, b, strength)) => {
                    let mut t = if hi > lo {
                        (strength - lo) / (hi - lo)
                    } else {
                        strength
                    };
                    t = (0.5 + (t - 0.5) * job.contrast).clamp(0.0, 1.0);
                    // Floor so cream bodies / neon strokes stay readable.
                    t = t.max(job.ink_floor);
                    let idx = ((t * ramp_n as f32).round() as usize).min(ramp_n);
                    let ch = ramp[idx];
                    let n = rgb_to_256(r, g, b);
                    out.push_str(&format!("\x1b[38;5;{n}m{ch}"));
                }
            }
            i += 1;
        }
        out.push_str("\x1b[0m\n");
    }
    Ok(out)
}

/// Mark the subject by flood-filling bg-like pixels from the border, while
/// forcing the central ellipse to stay foreground (cream-on-cream mugs).
fn subject_mask_flood_ellipse(img: &RgbaImage, bg: [u8; 3], job: &Job) -> Vec<bool> {
    let (w, h) = img.dimensions();
    let n = (w * h) as usize;
    let mut bg_like = vec![false; n];

    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x, y).0;
            let rgb = [p[0], p[1], p[2]];
            let d = color_dist(rgb, bg);
            let (cx, cy) = ellipse_coords(x, y, w, h, job);
            let ell = cx * cx + cy * cy;
            let idx = (y * w + x) as usize;
            if ell <= 1.0 {
                // Inside subject ellipse: only fringe that matches bg exactly.
                bg_like[idx] = d < job.bg_threshold * 0.45 && ell > 0.82;
            } else {
                bg_like[idx] = d < job.bg_threshold * 1.35;
            }
        }
    }

    // Flood from borders through bg_like.
    let mut is_bg = vec![false; n];
    let mut q = VecDeque::new();
    let push = |x: u32, y: u32, is_bg: &mut [bool], q: &mut VecDeque<(u32, u32)>, bg_like: &[bool]| {
        let idx = (y * w + x) as usize;
        if bg_like[idx] && !is_bg[idx] {
            is_bg[idx] = true;
            q.push_back((x, y));
        }
    };
    for x in 0..w {
        push(x, 0, &mut is_bg, &mut q, &bg_like);
        push(x, h - 1, &mut is_bg, &mut q, &bg_like);
    }
    for y in 0..h {
        push(0, y, &mut is_bg, &mut q, &bg_like);
        push(w - 1, y, &mut is_bg, &mut q, &bg_like);
    }
    while let Some((x, y)) = q.pop_front() {
        for (dx, dy) in [(1i32, 0), (-1, 0), (0, 1), (0, -1)] {
            let nx = x as i32 + dx;
            let ny = y as i32 + dy;
            if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                continue;
            }
            push(nx as u32, ny as u32, &mut is_bg, &mut q, &bg_like);
        }
    }

    // Subject = not flood-filled background. Also drop near-transparent.
    let mut subject = vec![false; n];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            let a = img.get_pixel(x, y).0[3];
            subject[idx] = a >= 16 && !is_bg[idx];
        }
    }

    // One pass of morphological close (dilate then erode) to fill pinholes
    // in cream mug bodies without re-attaching the studio floor.
    morph_close(&mut subject, w, h, 1);
    subject
}

fn ellipse_coords(x: u32, y: u32, w: u32, h: u32, job: &Job) -> (f32, f32) {
    let cx = ((x as f32 + 0.5) / w as f32 - 0.5) / job.ellipse_rx.max(0.05);
    let cy = ((y as f32 + 0.5) / h as f32 - 0.5 - job.ellipse_cy) / job.ellipse_ry.max(0.05);
    (cx, cy)
}

fn morph_close(mask: &mut [bool], w: u32, h: u32, radius: i32) {
    if radius <= 0 {
        return;
    }
    let dil = morph_dilate(mask, w, h, radius);
    let closed = morph_erode(&dil, w, h, radius);
    mask.copy_from_slice(&closed);
}

fn morph_dilate(mask: &[bool], w: u32, h: u32, radius: i32) -> Vec<bool> {
    let mut out = vec![false; mask.len()];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut any = false;
            'n: for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    if mask[(ny as u32 * w + nx as u32) as usize] {
                        any = true;
                        break 'n;
                    }
                }
            }
            out[(y as u32 * w + x as u32) as usize] = any;
        }
    }
    out
}

fn morph_erode(mask: &[bool], w: u32, h: u32, radius: i32) -> Vec<bool> {
    let mut out = vec![false; mask.len()];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut all = true;
            'n: for dy in -radius..=radius {
                for dx in -radius..=radius {
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    if !mask[(ny as u32 * w + nx as u32) as usize] {
                        all = false;
                        break 'n;
                    }
                }
            }
            out[(y as u32 * w + x as u32) as usize] = all;
        }
    }
    out
}

/// Brighten each pixel to the max luminance in a `(2r+1)²` neighborhood.
fn dilate_bright(img: &RgbaImage, radius: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut out = img.clone();
    let r = radius as i32;
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut best = img.get_pixel(x as u32, y as u32).0;
            let mut best_lum = lum(best);
            for dy in -r..=r {
                for dx in -r..=r {
                    let xx = x + dx;
                    let yy = y + dy;
                    if xx < 0 || yy < 0 || xx >= w as i32 || yy >= h as i32 {
                        continue;
                    }
                    let p = img.get_pixel(xx as u32, yy as u32).0;
                    let l = lum(p);
                    if l > best_lum {
                        best_lum = l;
                        best = p;
                    }
                }
            }
            out.put_pixel(x as u32, y as u32, Rgba(best));
        }
    }
    out
}

fn lum(p: [u8; 4]) -> f32 {
    (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32) / 255.0
}

fn sample_background(img: &RgbaImage) -> [u8; 3] {
    let (w, h) = img.dimensions();
    let corners = [
        img.get_pixel(0, 0),
        img.get_pixel(w - 1, 0),
        img.get_pixel(0, h - 1),
        img.get_pixel(w - 1, h - 1),
    ];
    let mut r = 0u32;
    let mut g = 0u32;
    let mut b = 0u32;
    for c in corners {
        let Rgba([rr, gg, bb, _]) = *c;
        r += rr as u32;
        g += gg as u32;
        b += bb as u32;
    }
    [(r / 4) as u8, (g / 4) as u8, (b / 4) as u8]
}

fn color_dist(a: [u8; 3], b: [u8; 3]) -> f32 {
    let dr = a[0] as f32 - b[0] as f32;
    let dg = a[1] as f32 - b[1] as f32;
    let db = a[2] as f32 - b[2] as f32;
    (dr * dr + dg * dg + db * db).sqrt()
}

fn percentile_bounds(vals: &[f32], lo_p: f32, hi_p: f32) -> (f32, f32) {
    if vals.is_empty() {
        return (0.0, 1.0);
    }
    let mut v = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let lo_i = ((v.len() - 1) as f32 * lo_p).round() as usize;
    let hi_i = ((v.len() - 1) as f32 * hi_p).round() as usize;
    (v[lo_i], v[hi_i.max(lo_i)])
}

fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let to6 = |v: u8| ((v as u16 * 5) / 255) as u8;
    16 + 36 * to6(r) + 6 * to6(g) + to6(b)
}
