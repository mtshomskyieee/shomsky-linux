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
use image::{Rgba, RgbaImage};

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

/// Optional hue gate after the subject mask (drops soft studio bloom).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChromaKey {
    None,
    Cyan,
    Green,
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
    chroma: ChromaKey,
    /// Crop to chroma/feature bbox (padded) before cell resize — more face detail.
    crop_subject: bool,
    /// Stamp angled dark sockets + bright pupils (leaf avatar).
    stamp_scary_eyes: bool,
}

const JOBS: &[Job] = &[
    Job {
        src: "navi.png",
        out: "navi.txt",
        cols: 48,
        rows: 22,
        // Solid colored cells — glyph soup reads as noise at this scale.
        ramp: "████████████████",
        bg_threshold: 22.0,
        contrast: 1.0,
        ink: InkMode::BrightInk,
        dilate: 2,
        subject: SubjectMode::CornerDist,
        ink_floor: 1.0,
        ellipse_rx: 0.45,
        ellipse_ry: 0.45,
        ellipse_cy: 0.0,
        chroma: ChromaKey::None,
        crop_subject: false,
        stamp_scary_eyes: false,
    },
    // Cream mug ≈ cream studio floor — corner distance alone eats the body.
    Job {
        src: "coffee-rest.jpg",
        out: "coffee-rest.txt",
        cols: 44,
        rows: 20,
        ramp: "████████████████",
        bg_threshold: 38.0,
        contrast: 1.0,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 1.0,
        ellipse_rx: 0.40,
        ellipse_ry: 0.46,
        ellipse_cy: 0.04,
        chroma: ChromaKey::None,
        crop_subject: false,
        stamp_scary_eyes: false,
    },
    Job {
        src: "coffee-busy.jpg",
        out: "coffee-busy.txt",
        cols: 44,
        rows: 20,
        ramp: "████████████████",
        bg_threshold: 38.0,
        contrast: 1.0,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 1.0,
        ellipse_rx: 0.42,
        ellipse_ry: 0.48,
        ellipse_cy: 0.02,
        chroma: ChromaKey::None,
        crop_subject: false,
        stamp_scary_eyes: false,
    },
    Job {
        src: "laptop-rest.jpg",
        out: "laptop-rest.txt",
        cols: 44,
        rows: 20,
        ramp: "████████████████",
        bg_threshold: 36.0,
        contrast: 1.0,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 1.0,
        ellipse_rx: 0.48,
        ellipse_ry: 0.42,
        ellipse_cy: 0.0,
        chroma: ChromaKey::None,
        crop_subject: false,
        stamp_scary_eyes: false,
    },
    Job {
        src: "laptop-busy.jpg",
        out: "laptop-busy.txt",
        cols: 44,
        rows: 20,
        ramp: "████████████████",
        bg_threshold: 36.0,
        contrast: 1.0,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 1.0,
        ellipse_rx: 0.48,
        ellipse_ry: 0.44,
        ellipse_cy: 0.0,
        chroma: ChromaKey::None,
        crop_subject: false,
        stamp_scary_eyes: false,
    },
    // Soft bloom on gray/warm floors becomes muddy ░ — chroma-key + crop.
    // Block-only ramp: punctuation (@%*+=) reads as noise on this subject.
    Job {
        src: "mana-rest.jpg",
        out: "mana-rest.txt",
        cols: 48,
        rows: 22,
        // Solid colored cells only — density comes from color, not glyph soup.
        ramp: "████████████████",
        bg_threshold: 32.0,
        contrast: 1.0,
        ink: InkMode::BrightInk,
        dilate: 1,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 1.0,
        ellipse_rx: 0.38,
        ellipse_ry: 0.42,
        ellipse_cy: -0.04,
        chroma: ChromaKey::Cyan,
        crop_subject: true,
        stamp_scary_eyes: false,
    },
    Job {
        src: "mana-busy.jpg",
        out: "mana-busy.txt",
        cols: 48,
        rows: 22,
        ramp: "████████████████",
        bg_threshold: 32.0,
        contrast: 1.0,
        ink: InkMode::BrightInk,
        dilate: 1,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 1.0,
        ellipse_rx: 0.40,
        ellipse_ry: 0.44,
        ellipse_cy: -0.04,
        chroma: ChromaKey::Cyan,
        crop_subject: true,
        stamp_scary_eyes: false,
    },
    // Green leaf on pale-green studio — chroma green + solid blocks.
    Job {
        src: "leaf-rest.jpg",
        out: "leaf-rest.txt",
        cols: 48,
        rows: 22,
        ramp: "████████████████",
        bg_threshold: 30.0,
        contrast: 1.0,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 1.0,
        ellipse_rx: 0.36,
        ellipse_ry: 0.44,
        ellipse_cy: 0.0,
        chroma: ChromaKey::Green,
        crop_subject: true,
        stamp_scary_eyes: true,
    },
    Job {
        src: "leaf-busy.jpg",
        out: "leaf-busy.txt",
        cols: 48,
        rows: 22,
        ramp: "████████████████",
        bg_threshold: 30.0,
        contrast: 1.0,
        ink: InkMode::DarkInk,
        dilate: 0,
        subject: SubjectMode::FloodEllipse,
        ink_floor: 1.0,
        ellipse_rx: 0.40,
        ellipse_ry: 0.46,
        ellipse_cy: 0.0,
        chroma: ChromaKey::Green,
        crop_subject: true,
        stamp_scary_eyes: true,
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
    let rgba = img.to_rgba8();
    let rgba = if job.crop_subject && job.chroma != ChromaKey::None {
        crop_to_chroma_subject(&rgba, job.chroma)
    } else {
        rgba
    };
    // Terminal cells are ~2× taller than wide: stretch width before resize.
    let (ow, oh) = rgba.dimensions();
    let stretched = image::imageops::resize(
        &rgba,
        ow.saturating_mul(2).max(1),
        oh.max(1),
        FilterType::Triangle,
    );
    // Oversample, optional dilate, then down to cell grid — keeps thin neon lines.
    let over_w = job.cols.saturating_mul(3).max(job.cols);
    let over_h = job.rows.saturating_mul(3).max(job.rows);
    let over: RgbaImage =
        image::imageops::resize(&stretched, over_w, over_h, FilterType::CatmullRom);
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

    let mut raw: Vec<Option<(u8, u8, u8, f32)>> =
        Vec::with_capacity((job.cols * job.rows) as usize);
    for y in 0..job.rows {
        for x in 0..job.cols {
            let p = small.get_pixel(x, y).0;
            let (r, g, b, a) = (p[0], p[1], p[2], p[3]);
            let lum = (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0;
            let mut keep = match (&subject, job.ink) {
                (Some(mask), _) => a >= 16 && mask[(y * job.cols + x) as usize],
                (None, InkMode::DarkInk) => {
                    a >= 16 && color_dist([r, g, b], bg) >= job.bg_threshold
                }
                (None, InkMode::BrightInk) => {
                    a >= 16 && lum >= 0.18 && color_dist([r, g, b], bg) >= job.bg_threshold
                }
            };
            if keep && job.chroma != ChromaKey::None {
                keep = matches_chroma(job.chroma, [r, g, b], lum) || is_dark_feature(lum, [r, g, b]);
            }
            if !keep {
                raw.push(None);
                continue;
            }
            // Bright body → dense; dark eyes/mouth → also dense (not sparse dots).
            let strength = match job.ink {
                InkMode::DarkInk => 1.0 - lum,
                InkMode::BrightInk
                    if job.chroma != ChromaKey::None && is_dark_feature(lum, [r, g, b]) =>
                {
                    (1.0 - lum).max(0.85)
                }
                InkMode::BrightInk => lum,
            };
            raw.push(Some((r, g, b, strength)));
        }
    }
    // Drop desk/window fragments that survive chroma (keep the main blob only).
    // Mana (cyan): erode→largest→dilate kills soft bloom. Leaf (green): largest
    // only — erode punches messy face holes that read as broken eyes.
    let mut cells = match job.chroma {
        ChromaKey::None => raw,
        ChromaKey::Cyan => {
            let eroded = erode_optional_cells(&raw, job.cols, job.rows);
            let largest = keep_largest_component(eroded, job.cols, job.rows);
            dilate_optional_cells(&largest, job.cols, job.rows)
        }
        ChromaKey::Green => keep_largest_component(raw, job.cols, job.rows),
    };
    if job.stamp_scary_eyes {
        stamp_scary_leaf_eyes(&mut cells, job.cols, job.rows);
    }
    let strengths: Vec<f32> = cells.iter().filter_map(|c| c.map(|(_, _, _, s)| s)).collect();
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

/// Carve two angled sockets with bright pupils into the leaf face.
fn stamp_scary_leaf_eyes(cells: &mut [Option<(u8, u8, u8, f32)>], cols: u32, rows: u32) {
    let mut min_x = cols;
    let mut min_y = rows;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut found = false;
    for y in 0..rows {
        for x in 0..cols {
            if cells[(y * cols + x) as usize].is_some() {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if !found || max_x <= min_x + 8 || max_y <= min_y + 6 {
        return;
    }
    let bw = max_x - min_x + 1;
    let bh = max_y - min_y + 1;
    // Face sits in the upper-middle of the leaf blade.
    let eye_y = min_y + (bh as f32 * 0.42).round() as u32;
    let left_x = min_x + (bw as f32 * 0.32).round() as u32;
    let right_x = min_x + (bw as f32 * 0.68).round() as u32;

    let put = |cells: &mut [Option<(u8, u8, u8, f32)>], x: i32, y: i32, rgb: [u8; 3]| {
        if x < 0 || y < 0 || x >= cols as i32 || y >= rows as i32 {
            return;
        }
        cells[(y as u32 * cols + x as u32) as usize] = Some((rgb[0], rgb[1], rgb[2], 1.0));
    };

    let socket = [5u8, 8, 4]; // near-black void
    let pupil = [60u8, 140, 255]; // bright blue
    let glint = [220u8, 245, 255]; // cool white highlight

    for (ex, inward) in [(left_x as i32, 1i32), (right_x as i32, -1i32)] {
        let ey = eye_y as i32;
        // Wide almond socket — solid dark blocks (not empty terminal holes).
        for dy in 0..4i32 {
            for dx in -3i32..=3 {
                if dy == 0 && dx.abs() == 3 {
                    continue;
                }
                if dy == 3 && dx.abs() >= 2 {
                    continue;
                }
                put(cells, ex + dx, ey + dy, socket);
            }
        }
        // Blue pupil, slightly inward; no brows (source face is brow-free).
        put(cells, ex + inward, ey + 1, pupil);
        put(cells, ex, ey + 1, pupil);
        put(cells, ex + inward, ey + 2, pupil);
        put(cells, ex, ey + 2, pupil);
        put(cells, ex + inward, ey + 1, glint);
    }
}

fn matches_chroma(key: ChromaKey, rgb: [u8; 3], lum: f32) -> bool {
    match key {
        ChromaKey::None => true,
        ChromaKey::Cyan => is_cyan_body(rgb, lum),
        ChromaKey::Green => is_green_body(rgb, lum),
    }
}

/// Cyan / aqua body of the mana flame (not gray desk bloom or warm room light).
fn is_cyan_body(rgb: [u8; 3], lum: f32) -> bool {
    let [r, g, b] = rgb;
    let blue_bias = b as i16 - r as i16;
    let green_bias = g as i16 - r as i16;
    lum >= 0.40 && blue_bias >= 22 && green_bias >= 12 && b >= 130 && b > g.saturating_sub(5)
}

/// Leaf body — green-dominant, not pale studio wash.
fn is_green_body(rgb: [u8; 3], lum: f32) -> bool {
    let [r, g, b] = rgb;
    let green_bias = g as i16 - r as i16;
    let green_over_blue = g as i16 - b as i16;
    lum >= 0.22 && green_bias >= 18 && green_over_blue >= 20 && g >= 90
}

fn erode_optional_cells(
    cells: &[Option<(u8, u8, u8, f32)>],
    cols: u32,
    rows: u32,
) -> Vec<Option<(u8, u8, u8, f32)>> {
    let mut out = vec![None; cells.len()];
    for y in 0..rows {
        for x in 0..cols {
            let i = (y * cols + x) as usize;
            let Some(v) = cells[i] else { continue };
            let mut all = true;
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx >= cols || ny >= rows {
                    all = false;
                    break;
                }
                if cells[(ny * cols + nx) as usize].is_none() {
                    all = false;
                    break;
                }
            }
            if all {
                out[i] = Some(v);
            }
        }
    }
    out
}

fn dilate_optional_cells(
    cells: &[Option<(u8, u8, u8, f32)>],
    cols: u32,
    rows: u32,
) -> Vec<Option<(u8, u8, u8, f32)>> {
    let mut out = cells.to_vec();
    for y in 0..rows {
        for x in 0..cols {
            let i = (y * cols + x) as usize;
            let Some(v) = cells[i] else { continue };
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx >= cols || ny >= rows {
                    continue;
                }
                let j = (ny * cols + nx) as usize;
                if out[j].is_none() {
                    out[j] = Some(v);
                }
            }
        }
    }
    out
}

/// Keep only the largest 4-connected blob of kept cells.
fn keep_largest_component(
    cells: Vec<Option<(u8, u8, u8, f32)>>,
    cols: u32,
    rows: u32,
) -> Vec<Option<(u8, u8, u8, f32)>> {
    let n = (cols * rows) as usize;
    if cells.len() != n {
        return cells;
    }
    let mut seen = vec![false; n];
    let mut best: Vec<usize> = Vec::new();
    for start in 0..n {
        if cells[start].is_none() || seen[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut comp = Vec::new();
        seen[start] = true;
        while let Some(i) = stack.pop() {
            comp.push(i);
            let x = (i as u32) % cols;
            let y = (i as u32) / cols;
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
            ] {
                if nx >= cols || ny >= rows {
                    continue;
                }
                let j = (ny * cols + nx) as usize;
                if cells[j].is_some() && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            }
        }
        if comp.len() > best.len() {
            best = comp;
        }
    }
    let mut keep = vec![false; n];
    for i in best {
        keep[i] = true;
    }
    cells
        .into_iter()
        .enumerate()
        .map(|(i, c)| if keep[i] { c } else { None })
        .collect()
}

/// Eyes / mouth / pupils inside the flame face.
fn is_dark_feature(lum: f32, rgb: [u8; 3]) -> bool {
    let [r, g, b] = rgb;
    // Dark ink, or near-black with a little blue.
    lum <= 0.38 && (r as u16 + g as u16 + b as u16) < 280
}

/// Crop to a padded bbox of chroma + dark feature pixels on a dark pad.
fn crop_to_chroma_subject(img: &RgbaImage, key: ChromaKey) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x, y).0;
            let rgb = [p[0], p[1], p[2]];
            let l = lum(p);
            if matches_chroma(key, rgb, l) || is_dark_feature(l, rgb) {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if !found || max_x <= min_x || max_y <= min_y {
        return img.clone();
    }
    let bw = (max_x - min_x + 1).max(1);
    let bh = (max_y - min_y + 1).max(1);
    let pad_x = (bw as f32 * 0.12).round() as u32;
    let pad_y = (bh as f32 * 0.12).round() as u32;
    let x0 = min_x.saturating_sub(pad_x);
    let y0 = min_y.saturating_sub(pad_y);
    let x1 = (max_x + pad_x).min(w - 1);
    let y1 = (max_y + pad_y).min(h - 1);
    let cw = (x1 - x0 + 1).max(1);
    let ch = (y1 - y0 + 1).max(1);
    // Pad onto near-black so leftover fringe knocks out cleanly.
    let mut out = RgbaImage::from_pixel(cw, ch, Rgba([8, 10, 18, 255]));
    for y in 0..ch {
        for x in 0..cw {
            let sx = x0 + x;
            let sy = y0 + y;
            if sx < w && sy < h {
                out.put_pixel(x, y, *img.get_pixel(sx, sy));
            }
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
