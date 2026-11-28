//! Shared TUI chrome: character grid, junction-aware borders, and themes.
//!
//! Screens paint into a [`Canvas`], then flush once. Border cells carry edge
//! bits (N/E/S/W); the flush pass turns them into box-drawing glyphs so
//! crossings become `┼` by construction.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::ui;

// ── Theme ────────────────────────────────────────────────────────────────────

/// Named TUI color theme (chrome only). `:sine` art is [`Avatar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Theme {
    Harbor,
    Coffee,
    Laptop,
    Classic,
    FrutigerAeroGreen,
    FrutigerAeroOrange,
    Rainbow,
    CottonCandy,
    Purple,
    HackerGreen,
    HackerAmber,
}

impl Theme {
    pub const ALL: [Theme; 11] = [
        Theme::Harbor,
        Theme::Coffee,
        Theme::Laptop,
        Theme::Classic,
        Theme::FrutigerAeroGreen,
        Theme::FrutigerAeroOrange,
        Theme::Rainbow,
        Theme::CottonCandy,
        Theme::Purple,
        Theme::HackerGreen,
        Theme::HackerAmber,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Theme::Harbor => "harbor",
            Theme::Coffee => "coffee",
            Theme::Laptop => "laptop",
            Theme::Classic => "classic",
            Theme::FrutigerAeroGreen => "frutiger-aero-green",
            Theme::FrutigerAeroOrange => "frutiger-aero-orange",
            Theme::Rainbow => "rainbow",
            Theme::CottonCandy => "cotton-candy",
            Theme::Purple => "purple",
            Theme::HackerGreen => "hacker-green",
            Theme::HackerAmber => "hacker-amber",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Theme::Harbor => "Dark teal chrome colors",
            Theme::Coffee => "Warm amber chrome colors",
            Theme::Laptop => "Soft blue chrome colors",
            Theme::Classic => "Cyan chrome colors",
            Theme::FrutigerAeroGreen => "Glossy aqua-green Frutiger Aero chrome",
            Theme::FrutigerAeroOrange => "Sunset orange Frutiger Aero chrome",
            Theme::Rainbow => "Vivid pink / cyan / yellow accents",
            Theme::CottonCandy => "Pastel pink and sky-blue chrome",
            Theme::Purple => "Violet chrome colors",
            Theme::HackerGreen => "Matrix phosphor green on dark",
            Theme::HackerAmber => "Amber CRT phosphor on dark",
        }
    }

    pub fn parse(s: &str) -> Option<Theme> {
        match s.trim().to_ascii_lowercase().as_str() {
            "harbor" => Some(Theme::Harbor),
            "coffee" => Some(Theme::Coffee),
            "laptop" => Some(Theme::Laptop),
            "classic" => Some(Theme::Classic),
            "frutiger-aero-green" | "aero-green" | "fruitger-aero-green" => {
                Some(Theme::FrutigerAeroGreen)
            }
            "frutiger-aero-orange" | "aero-orange" | "fruitger-aero-orange" => {
                Some(Theme::FrutigerAeroOrange)
            }
            "rainbow" => Some(Theme::Rainbow),
            "cotton-candy" | "cottoncandy" => Some(Theme::CottonCandy),
            "purple" | "violet" => Some(Theme::Purple),
            "hacker-green" | "matrix" => Some(Theme::HackerGreen),
            "hacker-amber" | "amber" => Some(Theme::HackerAmber),
            _ => None,
        }
    }

    pub fn palette(self) -> Palette {
        match self {
            Theme::Harbor => Palette {
                border: Style::fg256(81),
                border_focus: Style::fg256(159).bold(),
                title: Style::fg256(159).bold(),
                dim: Style::fg256(245),
                text: Style::fg256(252),
                accent: Style::fg256(86),
                danger: Style::fg256(203),
                selection_fg: Style::fg256(232),
                selection_bg: Style::bg256(81),
                overlay_border: Style::fg256(229),
            },
            Theme::Coffee => Palette {
                border: Style::fg256(180),
                border_focus: Style::fg256(222).bold(),
                title: Style::fg256(222).bold(),
                dim: Style::fg256(243),
                text: Style::fg256(253),
                accent: Style::fg256(214),
                danger: Style::fg256(167),
                selection_fg: Style::fg256(232),
                selection_bg: Style::bg256(172),
                overlay_border: Style::fg256(229),
            },
            Theme::Laptop => Palette {
                border: Style::fg256(111),
                border_focus: Style::fg256(153).bold(),
                title: Style::fg256(153).bold(),
                dim: Style::fg256(244),
                text: Style::fg256(254),
                accent: Style::fg256(117),
                danger: Style::fg256(203),
                selection_fg: Style::fg256(232),
                selection_bg: Style::bg256(75),
                overlay_border: Style::fg256(229),
            },
            Theme::Classic => Palette {
                border: Style::ansi(36),
                border_focus: Style::ansi(36).bold(),
                title: Style::ansi(36).bold(),
                dim: Style::ansi(2),
                text: Style::default(),
                accent: Style::ansi(32),
                danger: Style::ansi(31),
                selection_fg: Style::default(),
                selection_bg: Style::ansi(7),
                overlay_border: Style::ansi(33),
            },
            Theme::FrutigerAeroGreen => Palette {
                border: Style::fg256(80),
                border_focus: Style::fg256(122).bold(),
                title: Style::fg256(159).bold(),
                dim: Style::fg256(246),
                text: Style::fg256(255),
                accent: Style::fg256(86),
                danger: Style::fg256(203),
                selection_fg: Style::fg256(232),
                selection_bg: Style::bg256(79),
                overlay_border: Style::fg256(195),
            },
            Theme::FrutigerAeroOrange => Palette {
                border: Style::fg256(214),
                border_focus: Style::fg256(221).bold(),
                title: Style::fg256(230).bold(),
                dim: Style::fg256(245),
                text: Style::fg256(255),
                accent: Style::fg256(208),
                danger: Style::fg256(167),
                selection_fg: Style::fg256(232),
                selection_bg: Style::bg256(208),
                overlay_border: Style::fg256(229),
            },
            Theme::Rainbow => Palette {
                border: Style::fg256(201),
                border_focus: Style::fg256(213).bold(),
                title: Style::fg256(51).bold(),
                dim: Style::fg256(245),
                text: Style::fg256(255),
                accent: Style::fg256(226),
                danger: Style::fg256(196),
                selection_fg: Style::fg256(232),
                selection_bg: Style::bg256(165),
                overlay_border: Style::fg256(219),
            },
            Theme::CottonCandy => Palette {
                border: Style::fg256(218),
                border_focus: Style::fg256(225).bold(),
                title: Style::fg256(183).bold(),
                dim: Style::fg256(246),
                text: Style::fg256(255),
                accent: Style::fg256(117),
                danger: Style::fg256(203),
                selection_fg: Style::fg256(232),
                selection_bg: Style::bg256(212),
                overlay_border: Style::fg256(225),
            },
            Theme::Purple => Palette {
                border: Style::fg256(141),
                border_focus: Style::fg256(183).bold(),
                title: Style::fg256(183).bold(),
                dim: Style::fg256(245),
                text: Style::fg256(254),
                accent: Style::fg256(147),
                danger: Style::fg256(203),
                selection_fg: Style::fg256(231),
                selection_bg: Style::bg256(98),
                overlay_border: Style::fg256(189),
            },
            Theme::HackerGreen => Palette {
                border: Style::fg256(40),
                border_focus: Style::fg256(46).bold(),
                title: Style::fg256(82).bold(),
                dim: Style::fg256(239),
                text: Style::fg256(48),
                accent: Style::fg256(40),
                danger: Style::fg256(196),
                selection_fg: Style::fg256(22),
                selection_bg: Style::bg256(40),
                overlay_border: Style::fg256(82),
            },
            Theme::HackerAmber => Palette {
                border: Style::fg256(178),
                border_focus: Style::fg256(214).bold(),
                title: Style::fg256(220).bold(),
                dim: Style::fg256(239),
                text: Style::fg256(184),
                accent: Style::fg256(214),
                danger: Style::fg256(196),
                selection_fg: Style::fg256(232),
                selection_bg: Style::bg256(136),
                overlay_border: Style::fg256(220),
            },
        }
    }
}

/// ANSI style fragments for one theme. Empty strings when color is off.
#[derive(Debug, Clone, Copy, Default)]
pub struct Style {
    fg: Option<ColorSpec>,
    bg: Option<ColorSpec>,
    bold: bool,
    dim: bool,
    reverse: bool,
}

#[derive(Debug, Clone, Copy)]
enum ColorSpec {
    Ansi(u8),
    C256(u8),
}

impl Style {
    pub const fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            dim: false,
            reverse: false,
        }
    }

    pub const fn ansi(code: u8) -> Self {
        Self {
            fg: Some(ColorSpec::Ansi(code)),
            bg: None,
            bold: false,
            dim: false,
            reverse: false,
        }
    }

    pub const fn fg256(n: u8) -> Self {
        Self {
            fg: Some(ColorSpec::C256(n)),
            bg: None,
            bold: false,
            dim: false,
            reverse: false,
        }
    }

    pub const fn bg256(n: u8) -> Self {
        Self {
            fg: None,
            bg: Some(ColorSpec::C256(n)),
            bold: false,
            dim: false,
            reverse: false,
        }
    }

    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    pub fn sgr(self) -> String {
        if !ui::color_enabled() {
            return String::new();
        }
        let mut parts = Vec::new();
        if self.bold {
            parts.push("1".to_string());
        }
        if self.dim {
            parts.push("2".to_string());
        }
        if self.reverse {
            parts.push("7".to_string());
        }
        match self.fg {
            Some(ColorSpec::Ansi(c)) => parts.push(c.to_string()),
            Some(ColorSpec::C256(n)) => parts.push(format!("38;5;{n}")),
            None => {}
        }
        match self.bg {
            Some(ColorSpec::Ansi(c)) => parts.push(c.to_string()),
            Some(ColorSpec::C256(n)) => parts.push(format!("48;5;{n}")),
            None => {}
        }
        if parts.is_empty() {
            return String::new();
        }
        format!("\x1b[{}m", parts.join(";"))
    }

    /// Merge with another style: other wins for set fields.
    pub fn merge(self, other: Style) -> Style {
        Style {
            fg: other.fg.or(self.fg),
            bg: other.bg.or(self.bg),
            bold: self.bold || other.bold,
            dim: self.dim || other.dim,
            reverse: self.reverse || other.reverse,
        }
    }
}

/// Full color roles for the active theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub border: Style,
    pub border_focus: Style,
    pub title: Style,
    pub dim: Style,
    pub text: Style,
    pub accent: Style,
    pub danger: Style,
    pub selection_fg: Style,
    pub selection_bg: Style,
    pub overlay_border: Style,
}

impl Palette {
    /// Selection bar: bg + fg combined.
    pub fn selection(self) -> Style {
        self.selection_bg.merge(self.selection_fg).bold()
    }
}

// ── Active theme persistence ─────────────────────────────────────────────────

/// `:sine` left-top brand art. Independent of [`Theme`] chrome colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Avatar {
    Navi,
    Coffee,
    Laptop,
    Mana,
    Leaf,
    Classic,
}

impl Avatar {
    pub const ALL: [Avatar; 6] = [
        Avatar::Navi,
        Avatar::Coffee,
        Avatar::Laptop,
        Avatar::Mana,
        Avatar::Leaf,
        Avatar::Classic,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Avatar::Navi => "navi",
            Avatar::Coffee => "coffee",
            Avatar::Laptop => "laptop",
            Avatar::Mana => "mana",
            Avatar::Leaf => "leaf",
            Avatar::Classic => "classic",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Avatar::Navi => "navi eye",
            Avatar::Coffee => "coffee mug",
            Avatar::Laptop => "laptop",
            Avatar::Mana => "mana flame",
            Avatar::Leaf => "leaf",
            Avatar::Classic => "braille bunny",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Avatar::Navi => ":sine avatar — navi eye (img2ascii)",
            Avatar::Coffee => ":sine avatar — coffee mug rest/busy poses",
            Avatar::Laptop => ":sine avatar — laptop rest/busy poses",
            Avatar::Mana => ":sine avatar — mana flame rest/busy poses",
            Avatar::Leaf => ":sine avatar — leaf rest/busy poses",
            Avatar::Classic => ":sine avatar — braille bunny with bouncing eyes",
        }
    }

    pub fn parse(s: &str) -> Option<Avatar> {
        match s.trim().to_ascii_lowercase().as_str() {
            "navi" | "harbor" => Some(Avatar::Navi), // harbor was the old bundled name
            "coffee" => Some(Avatar::Coffee),
            "laptop" => Some(Avatar::Laptop),
            "mana" | "mana-flame" | "flame" => Some(Avatar::Mana),
            "leaf" => Some(Avatar::Leaf),
            "classic" | "bunny" => Some(Avatar::Classic),
            _ => None,
        }
    }
}

static ACTIVE: RwLock<Theme> = RwLock::new(Theme::Harbor);
static ACTIVE_AVATAR: RwLock<Avatar> = RwLock::new(Avatar::Navi);

fn user_dir() -> PathBuf {
    PathBuf::from(std::env::var("ENGRAM_USER_DIR").unwrap_or_else(|_| "/engram/user".into()))
}

/// Path to `/engram/user/.theme` (or `$ENGRAM_USER_DIR/.theme`).
pub fn theme_path() -> PathBuf {
    user_dir().join(".theme")
}

pub fn avatar_path() -> PathBuf {
    user_dir().join(".avatar")
}

/// Load theme + avatar from env then files. Unknown / missing → harbor / navi.
pub fn load_theme() -> Theme {
    if let Ok(v) = std::env::var("ENGRAM_THEME") {
        if let Some(t) = Theme::parse(&v) {
            set_active(t);
        } else {
            let t = read_theme_file(&theme_path()).unwrap_or(Theme::Harbor);
            set_active(t);
        }
    } else {
        let t = read_theme_file(&theme_path()).unwrap_or(Theme::Harbor);
        set_active(t);
    }
    if let Ok(v) = std::env::var("ENGRAM_AVATAR") {
        if let Some(a) = Avatar::parse(&v) {
            set_active_avatar(a);
        } else {
            let a = read_avatar_file(&avatar_path()).unwrap_or(Avatar::Navi);
            set_active_avatar(a);
        }
    } else {
        let a = read_avatar_file(&avatar_path()).unwrap_or(Avatar::Navi);
        set_active_avatar(a);
    }
    active()
}

fn read_theme_file(path: &Path) -> Option<Theme> {
    let text = fs::read_to_string(path).ok()?;
    Theme::parse(text.trim())
}

fn read_avatar_file(path: &Path) -> Option<Avatar> {
    let text = fs::read_to_string(path).ok()?;
    Avatar::parse(text.trim())
}

/// Persist theme name and update the in-process active theme.
pub fn save_theme(theme: Theme) -> io::Result<()> {
    set_active(theme);
    let path = theme_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, format!("{}\n", theme.name()))
}

pub fn save_avatar(avatar: Avatar) -> io::Result<()> {
    set_active_avatar(avatar);
    let path = avatar_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, format!("{}\n", avatar.name()))
}

pub fn set_active(theme: Theme) {
    if let Ok(mut g) = ACTIVE.write() {
        *g = theme;
    }
}

pub fn set_active_avatar(avatar: Avatar) {
    if let Ok(mut g) = ACTIVE_AVATAR.write() {
        *g = avatar;
    }
}

pub fn active() -> Theme {
    ACTIVE.read().map(|g| *g).unwrap_or(Theme::Harbor)
}

pub fn active_avatar() -> Avatar {
    ACTIVE_AVATAR.read().map(|g| *g).unwrap_or(Avatar::Navi)
}

pub fn palette() -> Palette {
    active().palette()
}

// ── Border edge bits ─────────────────────────────────────────────────────────

const N: u8 = 1;
const E: u8 = 2;
const S: u8 = 4;
const W: u8 = 8;

fn junction_glyph(bits: u8) -> char {
    match bits & 0x0f {
        b if b == N | S => '│',
        b if b == E | W => '─',
        b if b == E | S => '┌',
        b if b == W | S => '┐',
        b if b == E | N => '└',
        b if b == W | N => '┘',
        b if b == N | S | E => '├',
        b if b == N | S | W => '┤',
        b if b == E | W | S => '┬',
        b if b == E | W | N => '┴',
        b if b == N | E | S | W => '┼',
        b if b == N => '│',
        b if b == S => '│',
        b if b == E => '─',
        b if b == W => '─',
        _ => ' ',
    }
}

// ── Canvas ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Cell {
    glyph: char,
    style: Style,
    edges: u8,
    is_border: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            glyph: ' ',
            style: Style::default(),
            edges: 0,
            is_border: false,
        }
    }
}

/// Absolute rectangle in 0-based canvas coords (x, y, w, h).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

impl Rect {
    pub fn new(x: usize, y: usize, w: usize, h: usize) -> Self {
        Self { x, y, w, h }
    }

    pub fn inner(self) -> Rect {
        Rect {
            x: self.x.saturating_add(1),
            y: self.y.saturating_add(1),
            w: self.w.saturating_sub(2),
            h: self.h.saturating_sub(2),
        }
    }
}

/// Row-major character grid. Paint, then [`Canvas::flush`].
pub struct Canvas {
    pub cols: usize,
    pub rows: usize,
    cells: Vec<Cell>,
}

impl Canvas {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            cols,
            rows,
            cells: vec![Cell::default(); cols * rows],
        }
    }

    fn idx(&self, x: usize, y: usize) -> Option<usize> {
        if x < self.cols && y < self.rows {
            Some(y * self.cols + x)
        } else {
            None
        }
    }

    pub fn put(&mut self, x: usize, y: usize, ch: char, style: Style) {
        if let Some(i) = self.idx(x, y) {
            let c = &mut self.cells[i];
            if !c.is_border {
                c.glyph = ch;
                c.style = style;
            } else if ch != ' ' {
                // Content wins over empty border interior only; borders stay.
                c.glyph = ch;
                c.style = style;
                c.is_border = false;
                c.edges = 0;
            }
        }
    }

    /// Put a character that is purely content (overwrites border cells).
    pub fn put_force(&mut self, x: usize, y: usize, ch: char, style: Style) {
        if let Some(i) = self.idx(x, y) {
            let c = &mut self.cells[i];
            c.glyph = ch;
            c.style = style;
            c.is_border = false;
            c.edges = 0;
        }
    }

    fn add_edge(&mut self, x: usize, y: usize, bits: u8, style: Style) {
        if let Some(i) = self.idx(x, y) {
            let c = &mut self.cells[i];
            c.is_border = true;
            c.edges |= bits;
            c.style = style;
            c.glyph = junction_glyph(c.edges);
        }
    }

    /// Draw a full rectangle border. Title sits on the top rule; footer on the bottom.
    pub fn frame(&mut self, rect: Rect, title: &str, footer: &str, style: Style, title_style: Style) {
        if rect.w < 2 || rect.h < 2 {
            return;
        }
        let x0 = rect.x;
        let y0 = rect.y;
        let x1 = rect.x + rect.w - 1;
        let y1 = rect.y + rect.h - 1;

        self.add_edge(x0, y0, E | S, style);
        self.add_edge(x1, y0, W | S, style);
        self.add_edge(x0, y1, E | N, style);
        self.add_edge(x1, y1, W | N, style);

        for x in (x0 + 1)..x1 {
            self.add_edge(x, y0, E | W, style);
            self.add_edge(x, y1, E | W, style);
        }
        for y in (y0 + 1)..y1 {
            self.add_edge(x0, y, N | S, style);
            self.add_edge(x1, y, N | S, style);
        }

        // Title into top rule: " title "
        if !title.is_empty() && rect.w > 4 {
            let label = format!(" {title} ");
            let max = rect.w.saturating_sub(2);
            let visible: String = label.chars().take(max).collect();
            let start = x0 + 1;
            for (i, ch) in visible.chars().enumerate() {
                self.put_force(start + i, y0, ch, title_style);
            }
        }

        // Footer into bottom rule
        if !footer.is_empty() && rect.w > 4 {
            let label = format!(" {footer} ");
            let max = rect.w.saturating_sub(2);
            let visible: String = label.chars().take(max).collect();
            let start = x0 + 1;
            let dim = style; // keep border color; caller can pass dim via title_style for footer if needed
            let foot_style = Palette {
                border: dim,
                ..Theme::Classic.palette()
            }
            .dim;
            // Prefer palette dim if color on — use a soft dim style
            let fs = if ui::color_enabled() {
                Style {
                    fg: style.fg,
                    bg: None,
                    bold: false,
                    dim: true,
                    reverse: false,
                }
            } else {
                foot_style
            };
            for (i, ch) in visible.chars().enumerate() {
                self.put_force(start + i, y1, ch, fs);
            }
        }
    }

    /// Vertical split at `div_x` (absolute). Adds N|S edges; junctions resolve on flush.
    pub fn split_col(&mut self, rect: Rect, div_x: usize, style: Style) {
        if div_x <= rect.x || div_x >= rect.x + rect.w.saturating_sub(1) {
            return;
        }
        let y0 = rect.y;
        let y1 = rect.y + rect.h.saturating_sub(1);
        for y in y0..=y1 {
            let mut bits = N | S;
            if y == y0 {
                bits = E | W | S; // will OR with existing top
                // On the top border: add S to make ┬
                self.add_edge(div_x, y, E | W | S, style);
            } else if y == y1 {
                self.add_edge(div_x, y, E | W | N, style);
            } else {
                self.add_edge(div_x, y, bits, style);
            }
        }
    }

    /// Horizontal split at `div_y` (absolute).
    pub fn split_row(&mut self, rect: Rect, div_y: usize, style: Style) {
        if div_y <= rect.y || div_y >= rect.y + rect.h.saturating_sub(1) {
            return;
        }
        let x0 = rect.x;
        let x1 = rect.x + rect.w.saturating_sub(1);
        for x in x0..=x1 {
            if x == x0 {
                self.add_edge(x, div_y, N | S | E, style);
            } else if x == x1 {
                self.add_edge(x, div_y, N | S | W, style);
            } else {
                self.add_edge(x, div_y, E | W, style);
            }
        }
    }

    /// Write a text row into the canvas, padding/truncating to `width`.
    pub fn fill_row(&mut self, x: usize, y: usize, width: usize, text: &str, style: Style) {
        let mut n = 0;
        let mut chars = text.chars().peekable();
        while n < width {
            if let Some(c) = chars.peek().copied() {
                if c == '\x1b' {
                    // Skip ANSI in source; callers should pass plain text + style.
                    chars.next();
                    if chars.peek() == Some(&'[') {
                        chars.next();
                        for c2 in chars.by_ref() {
                            if c2.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    }
                    continue;
                }
                chars.next();
                self.put_force(x + n, y, c, style);
                n += 1;
            } else {
                self.put_force(x + n, y, ' ', style);
                n += 1;
            }
        }
    }

    /// Write text that may already contain SGR; each visible cell gets `base` style
    /// unless the stream sets its own (we strip and use base for simplicity when
    /// `preserve_ansi` is false). When true, emit via a side channel — for brand
    /// art we paint glyph-by-glyph with baked colors via [`Canvas::put_ansi_row`].
    pub fn put_ansi_row(&mut self, x: usize, y: usize, width: usize, text: &str, fallback: Style) {
        let mut n = 0;
        let mut cur = fallback;
        let mut chars = text.chars().peekable();
        while n < width {
            match chars.next() {
                Some('\x1b') => {
                    if chars.peek() == Some(&'[') {
                        chars.next();
                        let mut code = String::new();
                        for c2 in chars.by_ref() {
                            if c2.is_ascii_alphabetic() {
                                if c2 == 'm' {
                                    cur = parse_sgr(&code).unwrap_or(fallback);
                                }
                                break;
                            }
                            code.push(c2);
                        }
                    }
                }
                Some(c) => {
                    self.put_force(x + n, y, c, cur);
                    n += 1;
                }
                None => {
                    self.put_force(x + n, y, ' ', fallback);
                    n += 1;
                }
            }
        }
    }

    /// Floating overlay box centered in `outer`.
    pub fn overlay(
        &mut self,
        outer: Rect,
        width: usize,
        height: usize,
        title: &str,
        lines: &[&str],
        border: Style,
        title_style: Style,
        text_style: Style,
    ) {
        let w = width.min(outer.w.saturating_sub(2)).max(10);
        let h = height.min(outer.h.saturating_sub(2)).max(4);
        let x = outer.x + (outer.w.saturating_sub(w)) / 2;
        let y = outer.y + (outer.h.saturating_sub(h)) / 2;
        let rect = Rect::new(x, y, w, h);
        // Clear interior
        for yy in y..y + h {
            for xx in x..x + w {
                self.put_force(xx, yy, ' ', text_style);
            }
        }
        self.frame(rect, title, "", border, title_style);
        let inner = rect.inner();
        for (i, line) in lines.iter().enumerate() {
            if i >= inner.h {
                break;
            }
            self.fill_row(inner.x, inner.y + i, inner.w, line, text_style);
        }
    }

    /// Resolve junctions and write to stdout.
    pub fn flush(&self) {
        let mut buf = String::with_capacity(self.rows * self.cols * 8);
        let _ = write!(buf, "\x1b[2J\x1b[H\x1b[?25l");
        let reset = "\x1b[0m";
        let mut last_sgr = String::new();
        for y in 0..self.rows {
            if y > 0 {
                buf.push_str("\r\n");
            }
            for x in 0..self.cols {
                let c = &self.cells[y * self.cols + x];
                let glyph = if c.is_border {
                    junction_glyph(c.edges)
                } else {
                    c.glyph
                };
                let sgr = c.style.sgr();
                if sgr != last_sgr {
                    buf.push_str(reset);
                    buf.push_str(&sgr);
                    last_sgr = sgr;
                }
                buf.push(glyph);
            }
        }
        buf.push_str(reset);
        let mut out = io::stdout().lock();
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
    }

    /// Glyph-only dump for tests (no ANSI).
    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        for y in 0..self.rows {
            if y > 0 {
                out.push('\n');
            }
            for x in 0..self.cols {
                let c = &self.cells[y * self.cols + x];
                let glyph = if c.is_border {
                    junction_glyph(c.edges)
                } else {
                    c.glyph
                };
                out.push(glyph);
            }
        }
        out
    }

    pub fn glyph_at(&self, x: usize, y: usize) -> Option<char> {
        let i = self.idx(x, y)?;
        let c = &self.cells[i];
        Some(if c.is_border {
            junction_glyph(c.edges)
        } else {
            c.glyph
        })
    }
}

fn parse_sgr(code: &str) -> Option<Style> {
    if code.is_empty() || code == "0" {
        return Some(Style::default());
    }
    let mut style = Style::default();
    let parts: Vec<&str> = code.split(';').collect();
    let mut i = 0;
    while i < parts.len() {
        match parts[i] {
            "0" => style = Style::default(),
            "1" => style.bold = true,
            "2" => style.dim = true,
            "7" => style.reverse = true,
            "38" if parts.get(i + 1) == Some(&"5") => {
                if let Some(n) = parts.get(i + 2).and_then(|s| s.parse().ok()) {
                    style.fg = Some(ColorSpec::C256(n));
                }
                i += 2;
            }
            "48" if parts.get(i + 1) == Some(&"5") => {
                if let Some(n) = parts.get(i + 2).and_then(|s| s.parse().ok()) {
                    style.bg = Some(ColorSpec::C256(n));
                }
                i += 2;
            }
            p => {
                if let Ok(n) = p.parse::<u8>() {
                    if (30..=37).contains(&n) || (90..=97).contains(&n) || n == 39 {
                        style.fg = Some(ColorSpec::Ansi(n));
                    } else if (40..=47).contains(&n) || (100..=107).contains(&n) {
                        style.bg = Some(ColorSpec::Ansi(n));
                    }
                }
            }
        }
        i += 1;
    }
    Some(style)
}

/// Strip SGR sequences from a string, leaving glyphs only.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c2 in chars.by_ref() {
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Visible length ignoring ANSI.
pub fn visible_len(s: &str) -> usize {
    strip_ansi(s).chars().count()
}

/// Convenience: themed border / title / dim SGR strings for screens that still
/// emit ANSI by hand (`:chat`, `:quickyn`, `:web`).
pub struct ThemeAnsi {
    pub border: String,
    pub title: String,
    pub dim: String,
    pub text: String,
    pub accent: String,
    pub danger: String,
    pub selection: String,
    pub overlay: String,
    pub reset: String,
}

pub fn theme_ansi() -> ThemeAnsi {
    let p = palette();
    let color = ui::color_enabled();
    ThemeAnsi {
        border: p.border.sgr(),
        title: p.title.sgr(),
        dim: p.dim.sgr(),
        text: p.text.sgr(),
        accent: p.accent.sgr(),
        danger: p.danger.sgr(),
        selection: p.selection().sgr(),
        overlay: p.overlay_border.sgr(),
        reset: if color { "\x1b[0m".into() } else { String::new() },
    }
}

/// Serialize tests that mutate `ENGRAM_USER_DIR` / `ENGRAM_THEME` / `NO_COLOR`.
#[cfg(test)]
pub(crate) fn theme_env_lock() -> &'static std::sync::Mutex<()> {
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_lock() -> &'static std::sync::Mutex<()> {
        theme_env_lock()
    }

    #[test]
    fn two_by_two_split_has_cross() {
        let mut c = Canvas::new(11, 7);
        let rect = Rect::new(0, 0, 11, 7);
        let style = Style::default();
        c.frame(rect, "t", "f", style, style);
        c.split_col(rect, 5, style);
        c.split_row(rect, 3, style);
        assert_eq!(c.glyph_at(0, 0), Some('┌'));
        assert_eq!(c.glyph_at(10, 0), Some('┐'));
        assert_eq!(c.glyph_at(0, 6), Some('└'));
        assert_eq!(c.glyph_at(10, 6), Some('┘'));
        assert_eq!(c.glyph_at(5, 3), Some('┼'));
        assert_eq!(c.glyph_at(5, 0), Some('┬'));
        assert_eq!(c.glyph_at(5, 6), Some('┴'));
        assert_eq!(c.glyph_at(0, 3), Some('├'));
        assert_eq!(c.glyph_at(10, 3), Some('┤'));
    }

    #[test]
    fn title_row_width_matches_frame() {
        let mut c = Canvas::new(20, 5);
        let rect = Rect::new(0, 0, 20, 5);
        c.frame(rect, "hello", "", Style::default(), Style::default());
        let plain = c.to_plain();
        let top = plain.lines().next().unwrap();
        assert_eq!(top.chars().count(), 20);
    }

    #[test]
    fn theme_parse_and_fallback() {
        assert_eq!(Theme::parse("harbor"), Some(Theme::Harbor));
        assert_eq!(Theme::parse("COFFEE"), Some(Theme::Coffee));
        assert_eq!(Theme::parse("laptop"), Some(Theme::Laptop));
        assert_eq!(Theme::parse("classic"), Some(Theme::Classic));
        assert_eq!(
            Theme::parse("frutiger-aero-green"),
            Some(Theme::FrutigerAeroGreen)
        );
        assert_eq!(
            Theme::parse("aero-orange"),
            Some(Theme::FrutigerAeroOrange)
        );
        assert_eq!(Theme::parse("rainbow"), Some(Theme::Rainbow));
        assert_eq!(Theme::parse("cotton-candy"), Some(Theme::CottonCandy));
        assert_eq!(Theme::parse("purple"), Some(Theme::Purple));
        assert_eq!(Theme::parse("hacker-green"), Some(Theme::HackerGreen));
        assert_eq!(Theme::parse("hacker-amber"), Some(Theme::HackerAmber));
        assert_eq!(Theme::parse("nope"), None);
        assert_eq!(Theme::parse(""), None);
        assert_eq!(Avatar::parse("mana"), Some(Avatar::Mana));
        assert_eq!(Avatar::parse("mana-flame"), Some(Avatar::Mana));
        assert_eq!(Avatar::parse("leaf"), Some(Avatar::Leaf));
        assert_eq!(Theme::ALL.len(), 11);
        assert_eq!(Avatar::ALL.len(), 6);
    }

    #[test]
    fn theme_file_roundtrip() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile_dir();
        std::env::set_var("ENGRAM_USER_DIR", &dir);
        std::env::remove_var("ENGRAM_THEME");
        save_theme(Theme::Coffee).unwrap();
        let got = read_theme_file(&theme_path()).unwrap();
        assert_eq!(got, Theme::Coffee);
        fs::write(theme_path(), "garbage\n").unwrap();
        assert_eq!(read_theme_file(&theme_path()), None);
        fs::write(theme_path(), "").unwrap();
        assert_eq!(read_theme_file(&theme_path()), None);
        let _ = fs::remove_dir_all(&dir);
        std::env::remove_var("ENGRAM_USER_DIR");
    }

    #[test]
    fn no_color_yields_empty_sgr() {
        let _g = env_lock().lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("NO_COLOR", "1");
        let s = Theme::Harbor.palette().border.sgr();
        assert!(s.is_empty(), "expected empty SGR, got {s:?}");
        std::env::remove_var("NO_COLOR");
    }

    fn tempfile_dir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("shomsky-theme-{}", std::process::id()));
        let _ = fs::create_dir_all(&p);
        p
    }
}
