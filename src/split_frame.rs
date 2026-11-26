//! Reusable split-pane frame for hand-rolled ANSI TUIs.
//!
//! Layout tree: leaf panes with optional header/footer, horizontal (`Cols`) and
//! vertical (`Rows`) splits, optional status + command-line chrome. No agents —
//! callers publish lines into named slots. Resize via Ctrl-arrows; typed input
//! always goes to the command line. Recomputes from `term_size()` each draw.

use std::io::{self, Read, Write};
use std::time::Duration;

use crate::chrome::{self, Canvas, Rect, Style};
use crate::term;

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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    CtrlK,
    CtrlL,
    CtrlR,
    CtrlS,
    CtrlU,
    CtrlY,
    CtrlZ,
    Char(char),
    /// Bracketed-paste payload (`\r` / `\r\n` normalized to `\n`).
    Paste(String),
    Other,
}

/// Visible composer height cap (plan: grow 1–3 rows).
pub const COMPOSER_MAX_ROWS: usize = 3;

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
    /// Soft-wrap long lines to the pane width (agent / chat).
    pub wrap: bool,
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
            wrap: false,
        }
    }

    pub fn with_header(mut self, header: impl Into<String>) -> Self {
        self.header = Some(header.into());
        self
    }

    pub fn with_wrap(mut self) -> Self {
        self.wrap = true;
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

    /// Replace lines without forcing follow-bottom (roster refresh, etc.).
    pub fn set_lines_preserve_scroll(&mut self, lines: Vec<String>) {
        self.lines = lines;
        // scroll / follow_bottom clamped on next draw or scroll_by
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

    fn display_line_count(&self, pane_w: usize) -> usize {
        if self.wrap {
            wrap_pane_lines(&self.lines, pane_w.max(1)).len()
        } else {
            self.lines.len()
        }
    }

    #[allow(dead_code)]
    fn clamp_scroll(&mut self, pane_h: usize, pane_w: usize) {
        let body = self.content_height(pane_h);
        let max_scroll = self.display_line_count(pane_w).saturating_sub(body);
        if self.follow_bottom {
            self.scroll = max_scroll;
        } else {
            self.scroll = self.scroll.min(max_scroll);
        }
    }

    pub fn scroll_by(&mut self, delta: isize, pane_h: usize, pane_w: usize) {
        self.follow_bottom = false;
        let body = self.content_height(pane_h);
        let max_scroll = self.display_line_count(pane_w).saturating_sub(body) as isize;
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

/// Which pane receives Up/Down / PageUp/PageDown (typed chars still go to input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneFocus {
    Input,
    Files,
    Agent,
    Chat,
}

impl PaneFocus {
    pub fn label(self) -> &'static str {
        match self {
            PaneFocus::Input => "input",
            PaneFocus::Files => "files",
            PaneFocus::Agent => "agent",
            PaneFocus::Chat => "chat",
        }
    }

    fn next(self) -> Self {
        match self {
            PaneFocus::Input => PaneFocus::Files,
            PaneFocus::Files => PaneFocus::Agent,
            PaneFocus::Agent => PaneFocus::Chat,
            PaneFocus::Chat => PaneFocus::Input,
        }
    }

    fn is_left(self) -> bool {
        matches!(self, PaneFocus::Files)
    }
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
    pub pane_focus: PaneFocus,
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
        // No pane header — outer frame is titled "sine"; brand art fills the pane.
        let left_top = Leaf::new("left_top");
        // Roster is top-anchored (agents → loops → files); do not follow the
        // files tail the way agent/chat follow new lines.
        let mut left_bottom = Leaf::new("left_bottom").with_header("agents / loops / files");
        left_bottom.follow_bottom = false;
        let right_top = Leaf::new("right_top").with_header("agent").with_wrap();
        let right_bottom = Leaf::new("right_bottom").with_header("chat").with_wrap();

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
            pane_focus: PaneFocus::Input,
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

    /// Replace slot lines without snapping to the bottom (left roster refresh).
    pub fn set_slot_lines_preserve_scroll(&mut self, slot: Slot, lines: Vec<String>) {
        if let Some(id) = self.slot_map.resolve(slot).map(str::to_string) {
            if let Some(leaf) = self.root.find_leaf_mut(&id) {
                leaf.set_lines_preserve_scroll(lines);
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

    pub fn cycle_pane_focus(&mut self) {
        self.pane_focus = self.pane_focus.next();
    }

    pub fn handle_resize_key(&mut self, key: &Key) -> bool {
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
                let target = if self.pane_focus.is_left() {
                    ResizeTarget::LeftHorizontal
                } else {
                    ResizeTarget::RightHorizontal
                };
                self.resize(target, false);
                true
            }
            Key::CtrlDown => {
                let target = if self.pane_focus.is_left() {
                    ResizeTarget::LeftHorizontal
                } else {
                    ResizeTarget::RightHorizontal
                };
                self.resize(target, true);
                true
            }
            Key::Tab => {
                self.cycle_pane_focus();
                true
            }
            _ => false,
        }
    }

    pub fn insert_char(&mut self, c: char) {
        // Soft newlines only via insert_str / Enter handler — ignore bare \n here.
        if c == '\n' || c == '\r' {
            self.insert_str("\n");
            return;
        }
        let idx = byte_index_at_char(&self.input, self.cursor);
        self.input.insert(idx, c);
        self.cursor += 1;
        self.history_idx = None;
    }

    /// Insert text at the cursor (paste / newline). Normalizes `\r\n` and `\r` to `\n`.
    pub fn insert_str(&mut self, text: &str) {
        let normalized = normalize_paste(text);
        if normalized.is_empty() {
            return;
        }
        let idx = byte_index_at_char(&self.input, self.cursor);
        self.input.insert_str(idx, &normalized);
        self.cursor += normalized.chars().count();
        self.history_idx = None;
    }

    pub fn insert_newline(&mut self) {
        self.insert_str("\n");
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

    /// Rows the composer should occupy (1..=COMPOSER_MAX_ROWS) for current input/width.
    pub fn composer_rows_for(&self, cols: usize) -> usize {
        let input_w = cols
            .saturating_sub(self.prompt.chars().count())
            .max(1);
        wrap_composer_lines(&self.input, input_w)
            .len()
            .clamp(1, COMPOSER_MAX_ROWS)
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

    /// Leaf id for the focused content pane, if any (input has none).
    pub fn focused_leaf_id(&self) -> Option<&str> {
        match self.pane_focus {
            PaneFocus::Input => None,
            PaneFocus::Files => self
                .slot_map
                .left_bottom
                .as_deref()
                .or(self.slot_map.left.as_deref()),
            PaneFocus::Agent => self
                .slot_map
                .right_top
                .as_deref()
                .or(self.slot_map.right.as_deref()),
            PaneFocus::Chat => self
                .slot_map
                .right_bottom
                .as_deref()
                .or(self.slot_map.right.as_deref()),
        }
    }

    /// Scroll the focused content pane (agent / chat / files). No-op for input.
    pub fn scroll_focused(&mut self, delta: isize) {
        let Some(id) = self.focused_leaf_id().map(str::to_string) else {
            return;
        };
        let (term_rows, term_cols) = term::term_size();
        let cols = term_cols.clamp(40, term::MAX_COLS);
        let rows = term_rows.clamp(10, term::MAX_ROWS);
        let composer_rows = self.composer_rows_for(cols);
        let lay = FrameLayout::compute_sized(&self.root, rows, cols, composer_rows);
        let (pane_w, pane_h) = lay.pane_size_for(&id).unwrap_or((cols, 4));
        if let Some(leaf) = self.root.find_leaf_mut(&id) {
            leaf.scroll_by(delta, pane_h, pane_w);
        }
    }

    pub fn draw(&self) {
        let (term_rows, term_cols) = term::term_size();
        let cols = term_cols.clamp(40, term::MAX_COLS);
        let rows = term_rows.clamp(10, term::MAX_ROWS);
        let pal = chrome::palette();
        let border = pal.border;
        let title_st = pal.title;
        let dim = pal.dim;
        let text = pal.text;
        let accent = pal.accent;

        // Inner content width (inside outer frame).
        let inner_w = cols.saturating_sub(2).max(8);
        let prefix = &self.prompt;
        let prefix_w = prefix.chars().count();
        let input_w = inner_w.saturating_sub(prefix_w).max(1);
        let visual = wrap_composer_lines(&self.input, input_w);
        let composer_rows = visual.len().clamp(1, COMPOSER_MAX_ROWS);
        let lay = FrameLayout::compute_sized(&self.root, rows, cols, composer_rows);

        let mut canvas = Canvas::new(cols, rows);
        let outer = Rect::new(0, 0, cols, rows);
        let footer = self.footer.clone().unwrap_or_else(|| {
            "Esc exit · ^S send · Enter newline/send · Tab focus · Ctrl-arrows resize".into()
        });
        canvas.frame(outer, "sine", &footer, border, title_st);

        // Pane body rect (inside frame, above status + composer).
        let body = Rect::new(1, 1, lay.body_cols, lay.body_rows);
        paint_node_canvas(
            &mut canvas,
            &self.root,
            body,
            border,
            dim,
            title_st,
            text,
            pal.border_focus,
            self.pane_focus,
        );

        let status_y = 1 + lay.body_rows;
        let status = self.status.clone().unwrap_or_default();
        let focus_mark = self.pane_focus.label();
        let status_line = if status.is_empty() {
            format!(" tab focus:{focus_mark} · ^←^→ split · ^↑^↓ pane ")
        } else {
            format!(" {status} · focus:{focus_mark} ")
        };
        canvas.fill_row(1, status_y, inner_w, &status_line, dim);

        let (cursor_row, cursor_col) = cursor_visual_pos(&self.input, self.cursor, input_w);
        let scroll = composer_scroll(visual.len(), cursor_row, composer_rows);
        let composer_y0 = status_y + 1;
        for row in 0..composer_rows {
            let y = composer_y0 + row;
            if y >= rows.saturating_sub(1) {
                break;
            }
            let line_idx = scroll + row;
            let line = visual.get(line_idx).map(String::as_str).unwrap_or("");
            let cell = if line_idx == 0 {
                format!("{prefix}{line}")
            } else {
                format!("{:width$}{line}", "", width = prefix_w)
            };
            canvas.fill_row(1, y, inner_w, &cell, accent);
        }

        canvas.flush();

        // Place hardware cursor on the composer.
        let cursor_screen_row = composer_y0 + cursor_row.saturating_sub(scroll) + 1; // 1-based
        let screen_col = 2 + prefix_w + cursor_col; // 1-based, past left border
        let mut out = io::stdout().lock();
        let _ = write!(
            out,
            "\x1b[?25h\x1b[{cursor_screen_row};{screen_col}H"
        );
        let _ = out.flush();
    }
}

#[derive(Debug, Clone)]
struct FrameLayout {
    #[allow(dead_code)]
    cols: usize,
    #[allow(dead_code)]
    rows: usize,
    /// Inner width available to panes (cols − 2 for outer frame).
    body_cols: usize,
    body_rows: usize,
    /// leaf id → (x, y, w, h) 0-based canvas coords
    panes: Vec<(String, usize, usize, usize, usize)>,
}

impl FrameLayout {
    fn compute_sized(root: &Node, rows: usize, cols: usize, composer_rows: usize) -> Self {
        let composer_rows = composer_rows.clamp(1, COMPOSER_MAX_ROWS);
        // outer top + outer bottom + status + composer
        let chrome = 2 + 1 + composer_rows;
        let body_rows = rows.saturating_sub(chrome).max(3);
        let body_cols = cols.saturating_sub(2).max(8);
        let mut lay = Self {
            cols,
            rows,
            body_cols,
            body_rows,
            panes: Vec::new(),
        };
        // Panes start at (1,1) inside the outer frame.
        lay.collect(root, 1, 1, body_cols, body_rows);
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

    fn pane_size_for(&self, id: &str) -> Option<(usize, usize)> {
        self.panes
            .iter()
            .find(|(pid, ..)| pid == id)
            .map(|(_, _, _, w, h)| (*w, *h))
    }
}

fn leaf_is_focused(leaf: &Leaf, focus: PaneFocus) -> bool {
    match focus {
        PaneFocus::Input => false,
        PaneFocus::Files => leaf.id == "left_bottom",
        PaneFocus::Agent => leaf.id == "right_top",
        PaneFocus::Chat => leaf.id == "right_bottom",
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_node_canvas(
    canvas: &mut Canvas,
    node: &Node,
    rect: Rect,
    border: Style,
    dim: Style,
    title: Style,
    text: Style,
    focus_border: Style,
    focus: PaneFocus,
) {
    if rect.w == 0 || rect.h == 0 {
        return;
    }
    match node {
        Node::Leaf(leaf) => {
            paint_leaf_canvas(
                canvas,
                leaf,
                rect,
                dim,
                title,
                text,
                focus_border,
                leaf_is_focused(leaf, focus),
            );
        }
        Node::Cols {
            ratio,
            left,
            right,
        } => {
            let left_w = ((rect.w as f32) * *ratio).round() as usize;
            let left_w = left_w.clamp(8, rect.w.saturating_sub(8).max(8));
            let right_w = rect.w.saturating_sub(left_w + 1).max(1);
            let div_x = rect.x + left_w;
            canvas.split_col(rect, div_x, border);
            paint_node_canvas(
                canvas,
                left,
                Rect::new(rect.x, rect.y, left_w, rect.h),
                border,
                dim,
                title,
                text,
                focus_border,
                focus,
            );
            paint_node_canvas(
                canvas,
                right,
                Rect::new(rect.x + left_w + 1, rect.y, right_w, rect.h),
                border,
                dim,
                title,
                text,
                focus_border,
                focus,
            );
        }
        Node::Rows {
            ratio,
            top,
            bottom,
        } => {
            let top_h = ((rect.h as f32) * *ratio).round() as usize;
            let top_h = top_h.clamp(2, rect.h.saturating_sub(2).max(2));
            let bottom_h = rect.h.saturating_sub(top_h + 1).max(1);
            let div_y = rect.y + top_h;
            canvas.split_row(rect, div_y, border);
            paint_node_canvas(
                canvas,
                top,
                Rect::new(rect.x, rect.y, rect.w, top_h),
                border,
                dim,
                title,
                text,
                focus_border,
                focus,
            );
            paint_node_canvas(
                canvas,
                bottom,
                Rect::new(rect.x, rect.y + top_h + 1, rect.w, bottom_h),
                border,
                dim,
                title,
                text,
                focus_border,
                focus,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_leaf_canvas(
    canvas: &mut Canvas,
    leaf: &Leaf,
    rect: Rect,
    dim: Style,
    title: Style,
    text: Style,
    focus_border: Style,
    focused: bool,
) {
    if rect.w == 0 || rect.h == 0 {
        return;
    }
    let mut content_y = rect.y;
    let mut content_h = rect.h;
    let head_style = if focused { focus_border } else { title };

    if let Some(header) = &leaf.header {
        let label = format!(" {header} ");
        let used = label.chars().count().min(rect.w);
        let mut line: String = label.chars().take(used).collect();
        while line.chars().count() < rect.w {
            line.push('─');
        }
        canvas.fill_row(rect.x, content_y, rect.w, &line, head_style);
        content_y += 1;
        content_h = content_h.saturating_sub(1);
    }

    if leaf.footer.is_some() {
        content_h = content_h.saturating_sub(1);
    }

    let body_h = content_h.max(1);
    let display: Vec<String> = if leaf.wrap {
        wrap_pane_lines(&leaf.lines, rect.w)
    } else {
        leaf.lines.clone()
    };
    let max_scroll = display.len().saturating_sub(body_h);
    let start = if leaf.follow_bottom {
        max_scroll
    } else {
        leaf.scroll.min(max_scroll)
    };
    for row in 0..body_h {
        let gy = content_y + row;
        let line = display.get(start + row).cloned().unwrap_or_default();
        // Brand art and other panes may carry baked SGR.
        canvas.put_ansi_row(rect.x, gy, rect.w, &line, text);
    }

    if let Some(footer) = &leaf.footer {
        let gy = rect.y + rect.h - 1;
        canvas.fill_row(rect.x, gy, rect.w, footer, dim);
    }
}

pub fn clamp_ratio(r: f32) -> f32 {
    if !r.is_finite() {
        return 0.5;
    }
    r.clamp(RATIO_MIN, RATIO_MAX)
}

#[allow(dead_code)] // public helper; canvas path is preferred for new chrome
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

#[allow(dead_code)] // kept for single-line chrome callers / tests
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

/// Soft-wrap each logical pane line to `width` (agent / chat display).
pub fn wrap_pane_lines(lines: &[String], width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    if lines.is_empty() {
        return out;
    }
    for line in lines {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            out.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    out
}

/// Soft-wrap composer text into display rows (hard `\n` preserved).
pub fn wrap_composer_lines(input: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out: Vec<String> = Vec::new();
    // `split` keeps a trailing empty segment when input ends with `\n`.
    for logical in input.split('\n') {
        if logical.is_empty() {
            out.push(String::new());
            continue;
        }
        let chars: Vec<char> = logical.chars().collect();
        let mut start = 0;
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            out.push(chars[start..end].iter().collect());
            start = end;
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Cursor (row, col) within [`wrap_composer_lines`] for a character offset.
pub fn cursor_visual_pos(input: &str, cursor: usize, width: usize) -> (usize, usize) {
    let width = width.max(1);
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let lines = wrap_composer_lines(&before, width);
    // When `before` ends with `\n`, wrap yields a trailing empty row — cursor sits there.
    let row = lines.len().saturating_sub(1);
    let col = lines.last().map(|l| l.chars().count()).unwrap_or(0);
    (row, col)
}

fn composer_scroll(total_lines: usize, cursor_row: usize, view_rows: usize) -> usize {
    let view_rows = view_rows.max(1);
    if total_lines <= view_rows {
        return 0;
    }
    let max_scroll = total_lines - view_rows;
    if cursor_row < view_rows {
        0
    } else {
        (cursor_row + 1).saturating_sub(view_rows).min(max_scroll)
    }
}

/// Normalize pasted / multi-line text: `\r\n` and `\r` → `\n`; drop other C0 controls.
pub fn normalize_paste(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\n' | '\t' => out.push(c),
            c if c == '\u{7f}' || (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// Whether Enter should submit (vs insert a newline).
///
/// - Single-line non-empty → send
/// - Multi-line with cursor at end and buffer ending in `\n` → send (blank trailing line)
pub fn enter_should_send(input: &str, cursor: usize) -> bool {
    if input.is_empty() {
        return false;
    }
    if !input.contains('\n') {
        return true;
    }
    let at_end = cursor >= input.chars().count();
    at_end && input.ends_with('\n')
}

/// Enable or disable DECSET 2004 bracketed paste.
pub fn set_bracketed_paste(enabled: bool) {
    let mut out = io::stdout().lock();
    let seq = if enabled {
        "\x1b[?2004h"
    } else {
        "\x1b[?2004l"
    };
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
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

fn read_bracketed_paste(stdin: &mut impl Read, prefix: &[u8]) -> Key {
    let mut raw = Vec::from(prefix);
    let mut window = [0u8; 1];
    // End marker: ESC [ 2 0 1 ~
    let end = b"\x1b[201~";
    loop {
        match stdin.read(&mut window) {
            Ok(1) => {
                raw.push(window[0]);
                if raw.ends_with(end) {
                    raw.truncate(raw.len() - end.len());
                    break;
                }
                // Cap runaway pastes (512 KiB).
                if raw.len() > 512 * 1024 {
                    break;
                }
            }
            _ => break,
        }
    }
    let text = String::from_utf8_lossy(&raw).into_owned();
    Key::Paste(normalize_paste(&text))
}

pub fn decode_key(first: u8, stdin: &mut impl Read) -> Key {
    match first {
        0x01 => Key::CtrlA,
        0x03 => Key::CtrlC,
        0x0b => Key::CtrlK,
        0x0c => Key::CtrlL,
        0x12 => Key::CtrlR,
        0x13 => Key::CtrlS,
        0x15 => Key::CtrlU,
        0x19 => Key::CtrlY,
        0x1a => Key::CtrlZ,
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
                // Bracketed paste start: ESC [ 200 ~
                if n >= 5
                    && rest[1] == b'2'
                    && rest[2] == b'0'
                    && rest[3] == b'0'
                    && rest[4] == b'~'
                {
                    return read_bracketed_paste(stdin, &rest[5..n]);
                }
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

    #[test]
    fn ctrl_k_decodes() {
        let mut empty: &[u8] = &[];
        assert_eq!(decode_key(0x0b, &mut empty), Key::CtrlK);
    }

    #[test]
    fn ctrl_s_decodes() {
        let mut empty: &[u8] = &[];
        assert_eq!(decode_key(0x13, &mut empty), Key::CtrlS);
    }

    #[test]
    fn ctrl_r_decodes() {
        let mut empty: &[u8] = &[];
        assert_eq!(decode_key(0x12, &mut empty), Key::CtrlR);
    }

    #[test]
    fn ctrl_y_and_z_decode() {
        let mut empty: &[u8] = &[];
        assert_eq!(decode_key(0x19, &mut empty), Key::CtrlY);
        assert_eq!(decode_key(0x1a, &mut empty), Key::CtrlZ);
    }

    #[test]
    fn bracketed_paste_decodes_with_newlines() {
        // After ESC, remaining stream is `[200~…\x1b[201~`
        let mut body: &[u8] = b"[200~line1\r\nline2\x1b[201~";
        let key = decode_key(0x1b, &mut body);
        assert_eq!(key, Key::Paste("line1\nline2".into()));
    }

    #[test]
    fn normalize_paste_maps_crlf() {
        assert_eq!(normalize_paste("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(normalize_paste("x\0y"), "xy");
    }

    #[test]
    fn enter_should_send_single_and_trailing_blank() {
        assert!(!enter_should_send("", 0));
        assert!(enter_should_send("hello", 5));
        assert!(!enter_should_send("hello\nworld", 11));
        assert!(enter_should_send("hello\nworld\n", 12));
        assert!(!enter_should_send("hello\n", 3)); // cursor mid-buffer
        assert!(enter_should_send("hello\n", 6));
    }

    #[test]
    fn wrap_composer_hard_newlines_and_soft_wrap() {
        let lines = wrap_composer_lines("ab\nc", 10);
        assert_eq!(lines, vec!["ab".to_string(), "c".to_string()]);
        let soft = wrap_composer_lines("abcdef", 3);
        assert_eq!(
            soft,
            vec!["abc".to_string(), "def".to_string()]
        );
        let trailing = wrap_composer_lines("hi\n", 10);
        assert_eq!(trailing, vec!["hi".to_string(), String::new()]);
    }

    #[test]
    fn wrap_pane_lines_soft_wraps_long_rows() {
        let src = vec![
            "short".into(),
            "abcdefghij".into(),
            String::new(),
            "xy".into(),
        ];
        let wrapped = wrap_pane_lines(&src, 4);
        assert_eq!(
            wrapped,
            vec![
                "shor".to_string(),
                "t".to_string(),
                "abcd".to_string(),
                "efgh".to_string(),
                "ij".to_string(),
                String::new(),
                "xy".to_string(),
            ]
        );
    }

    #[test]
    fn sine_agent_and_chat_enable_wrap() {
        let frame = SplitFrame::sine_layout();
        assert!(frame.leaf("right_top").unwrap().wrap);
        assert!(frame.leaf("right_bottom").unwrap().wrap);
        assert!(!frame.leaf("left_top").unwrap().wrap);
    }

    #[test]
    fn composer_rows_clamp_to_three() {
        let mut frame = SplitFrame::sine_layout();
        frame.set_input("a\nb\nc\nd\ne");
        assert_eq!(frame.composer_rows_for(80), 3);
        frame.clear_input();
        assert_eq!(frame.composer_rows_for(80), 1);
    }

    #[test]
    fn insert_str_paste_does_not_submit() {
        let mut frame = SplitFrame::sine_layout();
        frame.insert_str("one\ntwo\nthree");
        assert_eq!(frame.input, "one\ntwo\nthree");
        assert_eq!(frame.cursor, "one\ntwo\nthree".chars().count());
        assert!(!enter_should_send(&frame.input, frame.cursor));
    }

    #[test]
    fn cursor_visual_pos_after_newline() {
        let (row, col) = cursor_visual_pos("hi\n", 3, 40);
        assert_eq!((row, col), (1, 0));
        let (row, col) = cursor_visual_pos("abcdef", 4, 3);
        assert_eq!((row, col), (1, 1)); // "abc" / "d|ef"
    }

    #[test]
    fn tab_cycles_pane_focus() {
        let mut frame = SplitFrame::sine_layout();
        assert_eq!(frame.pane_focus, PaneFocus::Input);
        frame.cycle_pane_focus();
        assert_eq!(frame.pane_focus, PaneFocus::Files);
        frame.cycle_pane_focus();
        assert_eq!(frame.pane_focus, PaneFocus::Agent);
        frame.cycle_pane_focus();
        assert_eq!(frame.pane_focus, PaneFocus::Chat);
        frame.cycle_pane_focus();
        assert_eq!(frame.pane_focus, PaneFocus::Input);
    }

    #[test]
    fn scroll_focused_targets_agent_not_chat() {
        let mut frame = SplitFrame::sine_layout();
        let many: Vec<String> = (0..40).map(|i| format!("agent-{i}")).collect();
        frame.set_slot_lines(Slot::RightTop, many);
        frame.set_slot_lines(Slot::RightBottom, vec!["chat-only".into()]);
        frame.pane_focus = PaneFocus::Agent;
        // Break follow-bottom so scroll sticks.
        if let Some(leaf) = frame.root.find_leaf_mut("right_top") {
            leaf.follow_bottom = false;
            leaf.scroll = 0;
        }
        frame.scroll_focused(5);
        let agent = frame.leaf("right_top").unwrap();
        let chat = frame.leaf("right_bottom").unwrap();
        assert_eq!(agent.scroll, 5);
        assert_eq!(chat.scroll, 0);
        assert!(chat.follow_bottom);
    }

    #[test]
    fn scroll_focused_targets_chat_not_agent() {
        let mut frame = SplitFrame::sine_layout();
        frame.set_slot_lines(Slot::RightTop, vec!["agent-only".into()]);
        let many: Vec<String> = (0..40).map(|i| format!("chat-{i}")).collect();
        frame.set_slot_lines(Slot::RightBottom, many);
        frame.pane_focus = PaneFocus::Chat;
        if let Some(leaf) = frame.root.find_leaf_mut("right_bottom") {
            leaf.follow_bottom = false;
            leaf.scroll = 0;
        }
        frame.scroll_focused(3);
        let agent = frame.leaf("right_top").unwrap();
        let chat = frame.leaf("right_bottom").unwrap();
        assert_eq!(chat.scroll, 3);
        assert_eq!(agent.scroll, 0);
        assert!(agent.follow_bottom);
    }

    #[test]
    fn set_slot_lines_preserve_scroll_keeps_position() {
        let mut frame = SplitFrame::sine_layout();
        frame.set_slot_lines(
            Slot::LeftBottom,
            (0..20).map(|i| format!("row-{i}")).collect(),
        );
        if let Some(leaf) = frame.root.find_leaf_mut("left_bottom") {
            leaf.follow_bottom = false;
            leaf.scroll = 4;
        }
        frame.set_slot_lines_preserve_scroll(
            Slot::LeftBottom,
            (0..20).map(|i| format!("new-{i}")).collect(),
        );
        let leaf = frame.leaf("left_bottom").unwrap();
        assert!(!leaf.follow_bottom);
        assert_eq!(leaf.scroll, 4);
        assert_eq!(leaf.lines[0], "new-0");
    }
}
