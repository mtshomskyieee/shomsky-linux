//! Reusable split-pane frame for hand-rolled ANSI TUIs.
//!
//! Layout tree: leaf panes with optional header/footer, horizontal (`Cols`) and
//! vertical (`Rows`) splits, optional status + command-line chrome. No agents —
//! callers publish lines into named slots. Resize via Ctrl-arrows; typed input
//! always goes to the command line. Recomputes from `term_size()` each draw.

use std::io::{self, Read, Write};
use std::time::Duration;

use crate::term;
use crate::ui;

/// Where `publish` appends lines. An unsplit side treats `Left`/`LeftTop` (and
/// `Right`/`RightTop`) as the same leaf.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)] // public publish API; not every slot is used by SINE yet
pub enum Slot {
    Left,
    Right,
    LeftTop,
    LeftBottom,
    RightTop,
    RightBottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeTarget {
    Vertical,
    LeftHorizontal,
    RightHorizontal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    CtrlUp,
    CtrlDown,
    CtrlLeft,
    CtrlRight,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    Tab,
    Backspace,
    CtrlA,
    CtrlC,
    CtrlL,
    CtrlU,
    Char(char),
    Other,
}

const RATIO_MIN: f32 = 0.2;
const RATIO_MAX: f32 = 0.8;
const RATIO_STEP: f32 = 0.05;

#[derive(Debug, Clone)]
pub struct Leaf {
    pub id: String,
    pub header: Option<String>,
    pub footer: Option<String>,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub follow_bottom: bool,
}

impl Leaf {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            header: None,
            footer: None,
            lines: Vec::new(),
            scroll: 0,
            follow_bottom: true,
        }
    }

    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header = Some(header.into());
        self
    }

    #[allow(dead_code)]
    pub fn with_footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.lines.clear();
        self.scroll = 0;
        self.follow_bottom = true;
    }

    pub fn publish_line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
        if self.follow_bottom {
            // scroll is clamped on draw against content height
        }
    }

    pub fn publish_text(&mut self, text: &str) {
        for line in text.lines() {
            self.publish_line(line.to_string());
        }
        if text.ends_with('\n') {
            // trailing blank already covered by lines(); keep as-is
        }
    }

    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines;
        self.follow_bottom = true;
        self.scroll = 0;
    }

    fn content_height(&self, pane_h: usize) -> usize {
        let mut h = pane_h;
        if self.header.is_some() {
            h = h.saturating_sub(1);
        }
        if self.footer.is_some() {
            h = h.saturating_sub(1);
        }
        h.max(1)
    }

    #[allow(dead_code)]
    fn clamp_scroll(&mut self, pane_h: usize) {
        let body = self.content_height(pane_h);
        let max_scroll = self.lines.len().saturating_sub(body);
        if self.follow_bottom {
            self.scroll = max_scroll;
        } else {
            self.scroll = self.scroll.min(max_scroll);
        }
    }

    pub fn scroll_by(&mut self, delta: isize, pane_h: usize) {
        self.follow_bottom = false;
        let body = self.content_height(pane_h);
        let max_scroll = self.lines.len().saturating_sub(body) as isize;
        let next = (self.scroll as isize + delta).clamp(0, max_scroll.max(0));
        self.scroll = next as usize;
        if self.scroll >= max_scroll as usize {
            self.follow_bottom = true;
        }
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Leaf(Leaf),
    Cols {
        ratio: f32,
        left: Box<Node>,
        right: Box<Node>,
    },
    Rows {
        ratio: f32,
        top: Box<Node>,
        bottom: Box<Node>,
    },
}

impl Node {
    pub fn leaf(leaf: Leaf) -> Self {
        Node::Leaf(leaf)
    }

    pub fn cols(ratio: f32, left: Node, right: Node) -> Self {
        Node::Cols {
            ratio: clamp_ratio(ratio),
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    pub fn rows(ratio: f32, top: Node, bottom: Node) -> Self {
        Node::Rows {
            ratio: clamp_ratio(ratio),
            top: Box::new(top),
            bottom: Box::new(bottom),
        }
    }

    fn find_leaf_mut(&mut self, id: &str) -> Option<&mut Leaf> {
        match self {
            Node::Leaf(leaf) => {
                if leaf.id == id {
                    Some(leaf)
                } else {
                    None
                }
            }
            Node::Cols { left, right, .. } => left
                .find_leaf_mut(id)
                .or_else(|| right.find_leaf_mut(id)),
            Node::Rows { top, bottom, .. } => {
                top.find_leaf_mut(id).or_else(|| bottom.find_leaf_mut(id))
            }
        }
    }

    fn find_leaf(&self, id: &str) -> Option<&Leaf> {
        match self {
            Node::Leaf(leaf) => {
                if leaf.id == id {
                    Some(leaf)
                } else {
                    None
                }
            }
            Node::Cols { left, right, .. } => {
                left.find_leaf(id).or_else(|| right.find_leaf(id))
            }
            Node::Rows { top, bottom, .. } => {
                top.find_leaf(id).or_else(|| bottom.find_leaf(id))
            }
        }
    }

    fn adjust_cols_ratio(&mut self, delta: f32) -> bool {
        match self {
            Node::Cols { ratio, .. } => {
                *ratio = clamp_ratio(*ratio + delta);
                true
            }
            Node::Rows { top, bottom, .. } => {
                top.adjust_cols_ratio(delta) || bottom.adjust_cols_ratio(delta)
            }
            Node::Leaf(_) => false,
        }
    }

    fn adjust_rows_ratio_in_left(&mut self, delta: f32) -> bool {
        match self {
            Node::Cols { left, .. } => left.adjust_rows_ratio(delta),
            Node::Rows { ratio, .. } => {
                *ratio = clamp_ratio(*ratio + delta);
                true
            }
            Node::Leaf(_) => false,
        }
    }

    fn adjust_rows_ratio_in_right(&mut self, delta: f32) -> bool {
        match self {
            Node::Cols { right, .. } => right.adjust_rows_ratio(delta),
            Node::Rows { ratio, .. } => {
                *ratio = clamp_ratio(*ratio + delta);
                true
            }
            Node::Leaf(_) => false,
        }
    }

    fn adjust_rows_ratio(&mut self, delta: f32) -> bool {
        match self {
            Node::Rows { ratio, .. } => {
                *ratio = clamp_ratio(*ratio + delta);
                true
            }
            Node::Cols { left, right, .. } => {
                left.adjust_rows_ratio(delta) || right.adjust_rows_ratio(delta)
            }
            Node::Leaf(_) => false,
        }
    }

    fn cols_ratio(&self) -> Option<f32> {
        match self {
            Node::Cols { ratio, .. } => Some(*ratio),
            Node::Rows { top, bottom, .. } => top.cols_ratio().or_else(|| bottom.cols_ratio()),
            Node::Leaf(_) => None,
        }
    }

    fn left_rows_ratio(&self) -> Option<f32> {
        match self {
            Node::Cols { left, .. } => left.rows_ratio(),
            Node::Rows { ratio, .. } => Some(*ratio),
            Node::Leaf(_) => None,
        }
    }

    fn right_rows_ratio(&self) -> Option<f32> {
        match self {
            Node::Cols { right, .. } => right.rows_ratio(),
            Node::Rows { ratio, .. } => Some(*ratio),
            Node::Leaf(_) => None,
        }
    }

    fn rows_ratio(&self) -> Option<f32> {
        match self {
            Node::Rows { ratio, .. } => Some(*ratio),
            Node::Cols { left, right, .. } => left.rows_ratio().or_else(|| right.rows_ratio()),
            Node::Leaf(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnFocus {
    Left,
    Right,
}

/// Full-screen split frame with optional status and a bottom command line.
#[derive(Debug, Clone)]
pub struct SplitFrame {
    pub root: Node,
    pub status: Option<String>,
    /// Bottom help row (keys / auto hint). When unset, a generic resize hint is shown.
    pub footer: Option<String>,
    pub input: String,
    pub cursor: usize,
    pub input_history: Vec<String>,
    pub history_idx: Option<usize>,
    pub column_focus: ColumnFocus,
    pub prompt: String,
    /// Slot id → leaf id mapping for publish.
    slot_map: SlotMap,
}

#[derive(Debug, Clone, Default)]
struct SlotMap {
    left: Option<String>,
    right: Option<String>,
    left_top: Option<String>,
    left_bottom: Option<String>,
    right_top: Option<String>,
    right_bottom: Option<String>,
}

impl SlotMap {
    fn resolve(&self, slot: Slot) -> Option<&str> {
        match slot {
            Slot::Left => self
                .left
                .as_deref()
                .or(self.left_top.as_deref()),
            Slot::Right => self
                .right
                .as_deref()
                .or(self.right_top.as_deref()),
            Slot::LeftTop => self
                .left_top
                .as_deref()
                .or(self.left.as_deref()),
            Slot::LeftBottom => self
                .left_bottom
                .as_deref()
                .or(self.left.as_deref()),
            Slot::RightTop => self
                .right_top
                .as_deref()
                .or(self.right.as_deref()),
            Slot::RightBottom => self
                .right_bottom
                .as_deref()
                .or(self.right.as_deref()),
        }
    }
}

impl SplitFrame {
    /// Build the SINE-shaped layout: left top/bottom + right top/bottom.
    pub fn sine_layout() -> Self {
        let left_top = Leaf::new("left_top").with_header("SINE");
        let left_bottom = Leaf::new("left_bottom").with_header("agents / loops / files");
        let right_top = Leaf::new("right_top").with_header("agent");
        let right_bottom = Leaf::new("right_bottom").with_header("chat");

        let left = Node::rows(
            0.45,
            Node::leaf(left_top),
            Node::leaf(left_bottom),
        );
        let right = Node::rows(
            0.55,
            Node::leaf(right_top),
            Node::leaf(right_bottom),
        );
        let root = Node::cols(0.35, left, right);

        let mut frame = Self {
            root,
            status: None,
            footer: None,
            input: String::new(),
            cursor: 0,
            input_history: Vec::new(),
            history_idx: None,
            column_focus: ColumnFocus::Right,
            prompt: "> ".into(),
            slot_map: SlotMap {
                left: None,
                right: None,
                left_top: Some("left_top".into()),
                left_bottom: Some("left_bottom".into()),
                right_top: Some("right_top".into()),
                right_bottom: Some("right_bottom".into()),
            },
        };
        let _ = &mut frame;
        frame
    }

    pub fn publish(&mut self, slot: Slot, text: &str) {
        if let Some(id) = self.slot_map.resolve(slot).map(str::to_string) {
            if let Some(leaf) = self.root.find_leaf_mut(&id) {
                leaf.publish_text(text);
            }
        }
    }

    pub fn publish_line(&mut self, slot: Slot, line: impl Into<String>) {
        if let Some(id) = self.slot_map.resolve(slot).map(str::to_string) {
            if let Some(leaf) = self.root.find_leaf_mut(&id) {
                leaf.publish_line(line);
            }
        }
    }

    pub fn set_slot_lines(&mut self, slot: Slot, lines: Vec<String>) {
        if let Some(id) = self.slot_map.resolve(slot).map(str::to_string) {
            if let Some(leaf) = self.root.find_leaf_mut(&id) {
                leaf.set_lines(lines);
            }
        }
    }

    pub fn clear_slot(&mut self, slot: Slot) {
        if let Some(id) = self.slot_map.resolve(slot).map(str::to_string) {
            if let Some(leaf) = self.root.find_leaf_mut(&id) {
                leaf.clear();
            }
        }
    }

    pub fn leaf(&self, id: &str) -> Option<&Leaf> {
        self.root.find_leaf(id)
    }

    #[allow(dead_code)]
    pub fn leaf_mut(&mut self, id: &str) -> Option<&mut Leaf> {
        self.root.find_leaf_mut(id)
    }

    /// Update a leaf's header title (e.g. live context meter).
    pub fn set_leaf_header(&mut self, id: &str, header: impl Into<String>) {
        if let Some(leaf) = self.root.find_leaf_mut(id) {
            leaf.header = Some(header.into());
        }
    }

    pub fn vertical_ratio(&self) -> f32 {
        self.root.cols_ratio().unwrap_or(0.5)
    }

    pub fn left_horizontal_ratio(&self) -> f32 {
        self.root.left_rows_ratio().unwrap_or(0.5)
    }

    pub fn right_horizontal_ratio(&self) -> f32 {
        self.root.right_rows_ratio().unwrap_or(0.5)
    }

    pub fn set_ratios(&mut self, vertical: f32, left_h: f32, right_h: f32) {
        if let Node::Cols {
            ratio,
            left,
            right,
        } = &mut self.root
        {
            *ratio = clamp_ratio(vertical);
            if let Node::Rows {
                ratio: lr,
                ..
            } = left.as_mut()
            {
                *lr = clamp_ratio(left_h);
            }
            if let Node::Rows {
                ratio: rr,
                ..
            } = right.as_mut()
            {
                *rr = clamp_ratio(right_h);
            }
        }
    }

    pub fn resize(&mut self, target: ResizeTarget, grow: bool) {
        let delta = if grow { RATIO_STEP } else { -RATIO_STEP };
        match target {
            ResizeTarget::Vertical => {
                self.root.adjust_cols_ratio(delta);
            }
            ResizeTarget::LeftHorizontal => {
                self.root.adjust_rows_ratio_in_left(delta);
            }
            ResizeTarget::RightHorizontal => {
                self.root.adjust_rows_ratio_in_right(delta);
            }
        }
    }

    pub fn toggle_column_focus(&mut self) {
        self.column_focus = match self.column_focus {
            ColumnFocus::Left => ColumnFocus::Right,
            ColumnFocus::Right => ColumnFocus::Left,
        };
    }

    pub fn handle_resize_key(&mut self, key: Key) -> bool {
        match key {
            Key::CtrlLeft => {
                self.resize(ResizeTarget::Vertical, false);
                true
            }
            Key::CtrlRight => {
                self.resize(ResizeTarget::Vertical, true);
                true
            }
            Key::CtrlUp => {
                let target = match self.column_focus {
                    ColumnFocus::Left => ResizeTarget::LeftHorizontal,
                    ColumnFocus::Right => ResizeTarget::RightHorizontal,
                };
                self.resize(target, false);
                true
            }
            Key::CtrlDown => {
                let target = match self.column_focus {
                    ColumnFocus::Left => ResizeTarget::LeftHorizontal,
                    ColumnFocus::Right => ResizeTarget::RightHorizontal,
                };
                self.resize(target, true);
                true
            }
            Key::Tab => {
                self.toggle_column_focus();
                true
            }
            _ => false,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        let idx = byte_index_at_char(&self.input, self.cursor);
        self.input.insert(idx, c);
        self.cursor += 1;
        self.history_idx = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let idx = byte_index_at_char(&self.input, self.cursor - 1);
        self.input.remove(idx);
        self.cursor -= 1;
        self.history_idx = None;
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.history_idx = None;
    }

    /// Replace the command line (e.g. edit plan into the input).
    pub fn set_input(&mut self, text: impl Into<String>) {
        self.input = text.into();
        self.cursor = self.input.chars().count();
        self.history_idx = None;
    }

    pub fn take_input(&mut self) -> String {
        let line = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.history_idx = None;
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            self.input_history.push(trimmed.to_string());
        }
        line
    }

    pub fn history_up(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next = match self.history_idx {
            None => self.input_history.len().saturating_sub(1),
            Some(0) => 0,
            Some(i) => i.saturating_sub(1),
        };
        self.history_idx = Some(next);
        self.input = self.input_history[next].clone();
        self.cursor = self.input.chars().count();
    }

    pub fn history_down(&mut self) {
        let Some(idx) = self.history_idx else {
            return;
        };
        if idx + 1 >= self.input_history.len() {
            self.history_idx = None;
            self.input.clear();
            self.cursor = 0;
            return;
        }
        let next = idx + 1;
        self.history_idx = Some(next);
        self.input = self.input_history[next].clone();
        self.cursor = self.input.chars().count();
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_cursor_right(&mut self) {
        let n = self.input.chars().count();
        if self.cursor < n {
            self.cursor += 1;
        }
    }

    /// Scroll the focused column's bottom-most (or only) content pane.
    pub fn scroll_focused(&mut self, delta: isize) {
        let id = match self.column_focus {
            ColumnFocus::Left => self
                .slot_map
                .left_bottom
                .as_deref()
                .or(self.slot_map.left.as_deref())
                .or(self.slot_map.left_top.as_deref()),
            ColumnFocus::Right => self
                .slot_map
                .right_bottom
                .as_deref()
                .or(self.slot_map.right.as_deref())
                .or(self.slot_map.right_top.as_deref()),
        };
        let Some(id) = id.map(str::to_string) else {
            return;
        };
        let lay = FrameLayout::compute(&self.root);
        let pane_h = lay.pane_height_for(&id).unwrap_or(4);
        if let Some(leaf) = self.root.find_leaf_mut(&id) {
            leaf.scroll_by(delta, pane_h);
        }
    }

    pub fn draw(&self) {
        let lay = FrameLayout::compute(&self.root);
        let color = ui::color_enabled();
        let r = "\x1b[0m";
        let bold = if color { "\x1b[1m" } else { "" };
        let dim = if color { "\x1b[2m" } else { "" };
        let cyan = if color { "\x1b[36m" } else { "" };
        let inv = if color { "\x1b[7m" } else { "" };

        let mut buf = String::with_capacity(lay.rows * lay.cols * 2);
        use std::fmt::Write as _;
        let _ = write!(buf, "\x1b[2J\x1b[H");

        paint_node(
            &mut buf,
            &self.root,
            &lay,
            1,
            1,
            lay.cols,
            lay.body_rows,
            cyan,
            dim,
            bold,
            r,
            self.column_focus,
        );

        let status_y = lay.body_rows + 1;
        let status = self.status.clone().unwrap_or_default();
        let focus_mark = match self.column_focus {
            ColumnFocus::Left => "left",
            ColumnFocus::Right => "right",
        };
        let status_line = if status.is_empty() {
            format!("{dim} tab focus:{focus_mark} · ^←^→ split · ^↑^↓ pane {r}")
        } else {
            format!("{dim}{status} · focus:{focus_mark}{r}")
        };
        let _ = write!(
            buf,
            "\x1b[{status_y};1H{}",
            pad_visible(&status_line, lay.cols)
        );

        let input_y = status_y + 1;
        let prefix = &self.prompt;
        let input_w = lay.cols.saturating_sub(prefix.chars().count()).max(1);
        let (shown, cursor_col) = visible_input(&self.input, self.cursor, input_w);
        let input_cell = format!("{prefix}{shown}");
        let _ = write!(
            buf,
            "\x1b[{input_y};1H{cyan}{bold}{}{r}",
            pad_visible(&input_cell, lay.cols)
        );
        let _ = inv; // reserved for focused chrome
        // Footer help row (keys / auto).
        if lay.rows > input_y {
            let foot_y = lay.rows;
            let footer = self.footer.clone().unwrap_or_else(|| {
                "Esc exit · Enter send · Tab focus · Ctrl-arrows resize".into()
            });
            let _ = write!(
                buf,
                "\x1b[{foot_y};1H{dim}{}{r}",
                pad_visible(&footer, lay.cols)
            );
        }

        let screen_col = 1 + prefix.chars().count() + cursor_col;
        let _ = write!(buf, "\x1b[?25h\x1b[{input_y};{screen_col}H");

        let mut out = io::stdout().lock();
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
    }
}

#[derive(Debug, Clone)]
struct FrameLayout {
    cols: usize,
    rows: usize,
    body_rows: usize,
    /// leaf id → (x, y, w, h) 1-based screen coords
    panes: Vec<(String, usize, usize, usize, usize)>,
}

impl FrameLayout {
    fn compute(root: &Node) -> Self {
        let (rows, cols) = term::term_size();
        let cols = cols.clamp(40, term::MAX_COLS);
        let rows = rows.clamp(10, term::MAX_ROWS);
        // body + status + input + footer
        let body_rows = rows.saturating_sub(3).max(3);
        let mut lay = Self {
            cols,
            rows,
            body_rows,
            panes: Vec::new(),
        };
        lay.collect(root, 1, 1, cols, body_rows);
        lay
    }

    fn collect(&mut self, node: &Node, x: usize, y: usize, w: usize, h: usize) {
        match node {
            Node::Leaf(leaf) => {
                self.panes.push((leaf.id.clone(), x, y, w, h));
            }
            Node::Cols {
                ratio,
                left,
                right,
            } => {
                let left_w = ((w as f32) * ratio).round() as usize;
                let left_w = left_w.clamp(8, w.saturating_sub(8).max(8));
                let right_w = w.saturating_sub(left_w + 1).max(1); // 1 for divider
                self.collect(left, x, y, left_w, h);
                self.collect(right, x + left_w + 1, y, right_w, h);
            }
            Node::Rows {
                ratio,
                top,
                bottom,
            } => {
                let top_h = ((h as f32) * ratio).round() as usize;
                let top_h = top_h.clamp(2, h.saturating_sub(2).max(2));
                let bottom_h = h.saturating_sub(top_h + 1).max(1); // 1 for divider
                self.collect(top, x, y, w, top_h);
                self.collect(bottom, x, y + top_h + 1, w, bottom_h);
            }
        }
    }

    fn pane_height_for(&self, id: &str) -> Option<usize> {
        self.panes
            .iter()
            .find(|(pid, ..)| pid == id)
            .map(|(_, _, _, _, h)| *h)
    }

}

#[allow(clippy::too_many_arguments)]
fn paint_node(
    buf: &mut String,
    node: &Node,
    _lay: &FrameLayout,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    cyan: &str,
    dim: &str,
    bold: &str,
    reset: &str,
    focus: ColumnFocus,
) {
    use std::fmt::Write as _;
    match node {
        Node::Leaf(leaf) => {
            paint_leaf(buf, leaf, x, y, w, h, cyan, dim, bold, reset);
        }
        Node::Cols {
            ratio,
            left,
            right,
        } => {
            let left_w = ((w as f32) * ratio).round() as usize;
            let left_w = left_w.clamp(8, w.saturating_sub(8).max(8));
            let right_w = w.saturating_sub(left_w + 1).max(1);
            let div_x = x + left_w;
            let _ = focus;
            for row in 0..h {
                let gy = y + row;
                let ch = if row == 0 {
                    "┬"
                } else if row + 1 == h {
                    "┴"
                } else {
                    "│"
                };
                let _ = write!(buf, "\x1b[{gy};{div_x}H{cyan}{ch}{reset}");
            }
            paint_node(
                buf, left, _lay, x, y, left_w, h, cyan, dim, bold, reset, focus,
            );
            paint_node(
                buf,
                right,
                _lay,
                x + left_w + 1,
                y,
                right_w,
                h,
                cyan,
                dim,
                bold,
                reset,
                focus,
            );
        }
        Node::Rows {
            ratio,
            top,
            bottom,
        } => {
            let top_h = ((h as f32) * ratio).round() as usize;
            let top_h = top_h.clamp(2, h.saturating_sub(2).max(2));
            let bottom_h = h.saturating_sub(top_h + 1).max(1);
            let div_y = y + top_h;
            let line = "─".repeat(w);
            let _ = write!(buf, "\x1b[{div_y};{x}H{cyan}{line}{reset}");
            paint_node(
                buf, top, _lay, x, y, w, top_h, cyan, dim, bold, reset, focus,
            );
            paint_node(
                buf,
                bottom,
                _lay,
                x,
                y + top_h + 1,
                w,
                bottom_h,
                cyan,
                dim,
                bold,
                reset,
                focus,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_leaf(
    buf: &mut String,
    leaf: &Leaf,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    cyan: &str,
    dim: &str,
    bold: &str,
    reset: &str,
) {
    use std::fmt::Write as _;
    if w == 0 || h == 0 {
        return;
    }

    let mut content_y = y;
    let mut content_h = h;

    if let Some(header) = &leaf.header {
        let title = format!(" {header} ");
        let dash_n = w.saturating_sub(title.chars().count());
        let line = format!("{bold}{title}{reset}{cyan}{}{reset}", "─".repeat(dash_n));
        let _ = write!(
            buf,
            "\x1b[{content_y};{x}H{cyan}{}",
            pad_visible(&line, w)
        );
        content_y += 1;
        content_h = content_h.saturating_sub(1);
    }

    if leaf.footer.is_some() {
        content_h = content_h.saturating_sub(1);
    }

    let body_h = content_h.max(1);
    let max_scroll = leaf.lines.len().saturating_sub(body_h);
    let start = if leaf.follow_bottom {
        max_scroll
    } else {
        leaf.scroll.min(max_scroll)
    };
    for row in 0..body_h {
        let gy = content_y + row;
        let text = leaf
            .lines
            .get(start + row)
            .cloned()
            .unwrap_or_default();
        let cell = truncate_visible(&text, w);
        let _ = write!(buf, "\x1b[{gy};{x}H{}", pad_visible(&cell, w));
    }

    if let Some(footer) = &leaf.footer {
        let gy = y + h - 1;
        let cell = format!("{dim}{}{reset}", truncate_visible(footer, w));
        let _ = write!(buf, "\x1b[{gy};{x}H{}", pad_visible(&cell, w));
    }
}

pub fn clamp_ratio(r: f32) -> f32 {
    if !r.is_finite() {
        return 0.5;
    }
    r.clamp(RATIO_MIN, RATIO_MAX)
}

pub fn pad_visible(s: &str, width: usize) -> String {
    let visible = strip_ansi_len(s);
    if visible >= width {
        return truncate_visible(s, width);
    }
    format!("{s}{}", " ".repeat(width - visible))
}

pub fn truncate_visible(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            if chars.peek() == Some(&'[') {
                out.push(chars.next().unwrap());
                for c2 in chars.by_ref() {
                    out.push(c2);
                    if c2.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if n >= width {
            break;
        }
        out.push(c);
        n += 1;
    }
    out
}

fn strip_ansi_len(s: &str) -> usize {
    let mut n = 0;
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
        n += 1;
    }
    n
}

fn byte_index_at_char(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

pub fn visible_input(input: &str, cursor: usize, width: usize) -> (String, usize) {
    let chars: Vec<char> = input.chars().collect();
    if chars.len() <= width {
        return (input.to_string(), cursor.min(chars.len()));
    }
    let start = cursor.saturating_sub(width.saturating_sub(1));
    let end = (start + width).min(chars.len());
    let shown: String = chars[start..end].iter().collect();
    let cursor_col = cursor.saturating_sub(start);
    (shown, cursor_col)
}

pub fn poll_key(timeout: Duration) -> Option<Key> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut stdin = io::stdin();
        let mut b = [0u8; 1];
        match stdin.read(&mut b) {
            Ok(1) => return Some(decode_key(b[0], &mut stdin)),
            Ok(0) | Err(_) => {
                if std::time::Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(_) => unreachable!(),
        }
    }
}

pub fn decode_key(first: u8, stdin: &mut impl Read) -> Key {
    match first {
        0x01 => Key::CtrlA,
        0x03 => Key::CtrlC,
        0x0c => Key::CtrlL,
        0x15 => Key::CtrlU,
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x08 | 0x7f => Key::Backspace,
        0x1b => {
            std::thread::sleep(Duration::from_millis(20));
            let mut rest = [0u8; 8];
            let n = stdin.read(&mut rest).unwrap_or(0);
            if n == 0 {
                return Key::Esc;
            }
            if rest[0] == b'[' {
                // CSI: plain arrows / page, or modifier form ESC [ 1 ; 5 A
                if n >= 2 && rest[1] == b'A' {
                    return Key::Up;
                }
                if n >= 2 && rest[1] == b'B' {
                    return Key::Down;
                }
                if n >= 2 && rest[1] == b'C' {
                    return Key::Right;
                }
                if n >= 2 && rest[1] == b'D' {
                    return Key::Left;
                }
                if n >= 3 && rest[1] == b'5' && rest[2] == b'~' {
                    return Key::PageUp;
                }
                if n >= 3 && rest[1] == b'6' && rest[2] == b'~' {
                    return Key::PageDown;
                }
                // Home: ESC [ H  or  ESC [ 1 ~  or  ESC [ 7 ~
                if n >= 2 && rest[1] == b'H' {
                    return Key::Home;
                }
                if n >= 3 && rest[1] == b'1' && rest[2] == b'~' {
                    return Key::Home;
                }
                if n >= 3 && rest[1] == b'7' && rest[2] == b'~' {
                    return Key::Home;
                }
                // End: ESC [ F  or  ESC [ 4 ~  or  ESC [ 8 ~
                if n >= 2 && rest[1] == b'F' {
                    return Key::End;
                }
                if n >= 3 && rest[1] == b'4' && rest[2] == b'~' {
                    return Key::End;
                }
                if n >= 3 && rest[1] == b'8' && rest[2] == b'~' {
                    return Key::End;
                }
                // ESC [ 1 ; 5 A  (ctrl-up) etc.
                if n >= 5 && rest[1] == b'1' && rest[2] == b';' && rest[3] == b'5' {
                    return match rest[4] {
                        b'A' => Key::CtrlUp,
                        b'B' => Key::CtrlDown,
                        b'C' => Key::CtrlRight,
                        b'D' => Key::CtrlLeft,
                        _ => Key::Other,
                    };
                }
                // ESC [ 1 ; 5 A sometimes arrives with more padding
                if n >= 6 && rest[1].is_ascii_digit() {
                    // look for ';5' then letter
                    for i in 1..n.saturating_sub(1) {
                        if rest[i] == b';' && rest[i + 1] == b'5' {
                            let letter = rest.get(i + 2).copied().unwrap_or(0);
                            return match letter {
                                b'A' => Key::CtrlUp,
                                b'B' => Key::CtrlDown,
                                b'C' => Key::CtrlRight,
                                b'D' => Key::CtrlLeft,
                                _ => Key::Other,
                            };
                        }
                    }
                }
            }
            if rest[0] == 0x1b {
                return Key::Esc;
            }
            Key::Other
        }
        c if (0x20..0x7f).contains(&c) => Key::Char(c as char),
        _ => Key::Other,
    }
}

/// Resolve publish slot against a mapping where some sides may be unsplit.
#[allow(dead_code)] // exercised in unit tests; public slot API for future callers
pub fn resolve_slot_id(
    slot: Slot,
    left: Option<&str>,
    right: Option<&str>,
    left_top: Option<&str>,
    left_bottom: Option<&str>,
    right_top: Option<&str>,
    right_bottom: Option<&str>,
) -> Option<String> {
    let map = SlotMap {
        left: left.map(str::to_string),
        right: right.map(str::to_string),
        left_top: left_top.map(str::to_string),
        left_bottom: left_bottom.map(str::to_string),
        right_top: right_top.map(str::to_string),
        right_bottom: right_bottom.map(str::to_string),
    };
    map.resolve(slot).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_ratio_bounds() {
        assert!((clamp_ratio(0.0) - RATIO_MIN).abs() < f32::EPSILON);
        assert!((clamp_ratio(1.0) - RATIO_MAX).abs() < f32::EPSILON);
        assert!((clamp_ratio(0.5) - 0.5).abs() < f32::EPSILON);
        assert!((clamp_ratio(f32::NAN) - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn unsplit_left_maps_left_and_left_top() {
        let id = resolve_slot_id(
            Slot::LeftTop,
            Some("only_left"),
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(id.as_deref(), Some("only_left"));
        let id2 = resolve_slot_id(
            Slot::Left,
            Some("only_left"),
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(id2.as_deref(), Some("only_left"));
    }

    #[test]
    fn split_left_prefers_top_bottom() {
        let id = resolve_slot_id(
            Slot::LeftTop,
            Some("legacy"),
            None,
            Some("lt"),
            Some("lb"),
            None,
            None,
        );
        assert_eq!(id.as_deref(), Some("lt"));
        let id_b = resolve_slot_id(
            Slot::LeftBottom,
            Some("legacy"),
            None,
            Some("lt"),
            Some("lb"),
            None,
            None,
        );
        assert_eq!(id_b.as_deref(), Some("lb"));
    }

    #[test]
    fn publish_reaches_leaf() {
        let mut frame = SplitFrame::sine_layout();
        frame.publish(Slot::RightTop, "hello\nworld");
        let leaf = frame.leaf("right_top").unwrap();
        assert_eq!(leaf.lines, vec!["hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn clear_slot_empties_agent_and_chat() {
        let mut frame = SplitFrame::sine_layout();
        frame.publish(Slot::RightTop, "plan junk");
        frame.publish(Slot::RightBottom, "you: hi");
        frame.clear_slot(Slot::RightTop);
        frame.clear_slot(Slot::RightBottom);
        assert!(frame.leaf("right_top").unwrap().lines.is_empty());
        assert!(frame.leaf("right_bottom").unwrap().lines.is_empty());
        frame.set_slot_lines(Slot::RightTop, vec!["(cleared)".into()]);
        frame.set_slot_lines(Slot::RightBottom, vec!["sine: (context cleared)".into()]);
        assert_eq!(
            frame.leaf("right_top").unwrap().lines,
            vec!["(cleared)".to_string()]
        );
        assert_eq!(
            frame.leaf("right_bottom").unwrap().lines,
            vec!["sine: (context cleared)".to_string()]
        );
    }

    #[test]
    fn resize_clamps_vertical() {
        let mut frame = SplitFrame::sine_layout();
        for _ in 0..40 {
            frame.resize(ResizeTarget::Vertical, true);
        }
        assert!(frame.vertical_ratio() <= RATIO_MAX + f32::EPSILON);
        for _ in 0..40 {
            frame.resize(ResizeTarget::Vertical, false);
        }
        assert!(frame.vertical_ratio() >= RATIO_MIN - f32::EPSILON);
    }

    #[test]
    fn ctrl_arrow_keys_decode() {
        let seq = b"\x1b[1;5C";
        let mut body = &seq[1..];
        let key = decode_key(0x1b, &mut body);
        assert_eq!(key, Key::CtrlRight);
    }

    #[test]
    fn ctrl_a_decodes() {
        let mut empty: &[u8] = &[];
        assert_eq!(decode_key(0x01, &mut empty), Key::CtrlA);
    }

    #[test]
    fn ctrl_l_decodes() {
        let mut empty: &[u8] = &[];
        assert_eq!(decode_key(0x0c, &mut empty), Key::CtrlL);
    }
}
