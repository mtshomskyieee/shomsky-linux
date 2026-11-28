//! Slip-in / slip-out feature navigation hub (ANSI + BusyBox stty).
//!
//! Opened from the REPL via `:menu` / `:m`. Selecting an item either runs a
//! meta-command / mode switch or returns a starter line to prefill.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::chrome::{self, Avatar, Canvas, Rect, Theme};
use crate::modes::Mode;
use crate::packages::PackageStatus;
use crate::term::{self, RawGuard};
use crate::ui;

/// Outcome of a menu session for the REPL to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    Dismiss,
    SetMode(Mode),
    RunMeta(String),
    InsertLine(String),
    ExitShell,
}

/// One shomagent roster entry for menu pickers (opened docs, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShomagentMemberBrief {
    pub id: u32,
    pub name: String,
    /// `read` / `readwrite` / `dream` / `refine` / process states.
    pub state: String,
    /// Document path for doc members; command line for process members.
    pub path: String,
}

impl ShomagentMemberBrief {
    pub fn is_document(&self) -> bool {
        matches!(
            self.state.as_str(),
            "read" | "readwrite" | "dream" | "refine" | "web"
        )
    }
}

/// Snapshot of REPL state the menu needs to draw / decide.
#[derive(Debug, Clone)]
pub struct MenuContext {
    pub mode: Mode,
    pub allow_root_mode: bool,
    pub pending_learn: bool,
    pub packages_path: PathBuf,
    pub web_history_path: PathBuf,
    pub web_history: Vec<crate::web_history::Visit>,
    pub edit_history_path: PathBuf,
    pub edit_history: Vec<crate::edit_history::EditVisit>,
    /// Live roster snapshot (may be empty if daemon is down).
    pub shomagent_members: Vec<ShomagentMemberBrief>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Editor,
    Help,
    Learn,
    Modes,
    Packages,
    Scripts,
    Shomagent,
    Shomloop,
    System,
    Web,
    /// After Web so digits 1–9 / 0 keep their old targets.
    Theme,
}

impl Category {
    /// Left column: first ten alphabetical, Theme last (no digit jump).
    pub const ALL: [Category; 11] = [
        Category::Editor,
        Category::Help,
        Category::Learn,
        Category::Modes,
        Category::Packages,
        Category::Scripts,
        Category::Shomagent,
        Category::Shomloop,
        Category::System,
        Category::Web,
        Category::Theme,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Editor => "Editor",
            Category::Help => "Help",
            Category::Learn => "Learn",
            Category::Modes => "Modes",
            Category::Packages => "Packages",
            Category::Scripts => "Scripts",
            Category::Shomagent => "shomagent",
            Category::Shomloop => "shomloop",
            Category::System => "System",
            Category::Web => "Web",
            Category::Theme => "Theme",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&c| c == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Category {
        Self::ALL[i % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Categories,
    Items,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilePromptKind {
    ReadWrite,
    DreamSeed,
    RefineIdea,
    /// Prompt for an http(s) URL → `shomagent web <url>`.
    WebUrl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberVerb {
    Ask,
    Talk,
    /// Stop a process or detach a document / dream member.
    Stop,
}

/// Nested shomagent flow: type a path, or pick a member.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ShomagentSub {
    PromptFile { kind: FilePromptKind, buf: String },
    PickMember { kind: MemberVerb },
}

/// Pure navigation state — unit-tested without a TTY.
#[derive(Debug, Clone)]
pub struct MenuState {
    pub category_idx: usize,
    pub item_idx: usize,
    focus: Focus,
    /// Exit confirm sub-prompt on System → Exit.
    exit_confirm: bool,
    /// Saved command waiting for run or delete.
    saved_pick: Option<String>,
    /// Tracked package waiting for uninstall / reinstall confirm.
    package_pick: Option<(String, PackageStatus)>,
    /// Nested shomagent filename / member picker.
    shomagent_sub: Option<ShomagentSub>,
    /// Planned run waiting for edit / enable / disable / delete / logs.
    loop_pick: Option<LoopPick>,
    web_clear_confirm: bool,
    web_history_path: PathBuf,
    web_history: Vec<crate::web_history::Visit>,
    edit_clear_confirm: bool,
    edit_history_path: PathBuf,
    edit_history: Vec<crate::edit_history::EditVisit>,
    allow_root_mode: bool,
    mode: Mode,
    pending_learn: bool,
    packages_path: PathBuf,
    shomagent_members: Vec<ShomagentMemberBrief>,
    /// Theme active when Theme category was entered (Esc restores).
    theme_backup: Option<Theme>,
    /// Preview theme while browsing Theme items (before Enter).
    theme_preview: Option<Theme>,
    /// Avatar active when Theme category was entered (Esc restores).
    avatar_backup: Option<Avatar>,
}

#[derive(Debug, Clone)]
struct Item {
    label: String,
    detail: String,
    kind: ItemKind,
}

#[derive(Debug, Clone)]
enum ItemKind {
    SetMode(Mode),
    RunMeta(&'static str),
    InsertLine(String),
    /// A saved command. Enter asks to run or delete it.
    Saved(String),
    /// Ledger package. Enter asks to uninstall or reinstall.
    TrackedPackage {
        name: String,
        status: PackageStatus,
    },
    /// Prompt for a filename, then insert `shomagent … <file>`.
    ShomagentPromptFile(FilePromptKind),
    /// Enter member-picker submenu for ask/talk.
    ShomagentPickMember(MemberVerb),
    /// A concrete opened document chosen for ask/talk.
    ShomagentMember {
        id: u32,
        verb: MemberVerb,
    },
    ExitShell,
    /// Confirm, then wipe `.web_history.json`.
    ClearWebHistory,
    /// Open `:web` already pointed at a saved URL.
    OpenWeb(String),
    /// Confirm, then wipe `.edit_history.json`.
    ClearEditHistory,
    /// Open `:edit` on a recent path.
    OpenEdit(String),
    /// A planned shomloop run. Enter asks how to manage it.
    Loop(LoopRef),
    /// Persist and apply a TUI color theme.
    SetTheme(Theme),
    /// Persist and apply a `:sine` avatar.
    SetAvatar(Avatar),
    InfoOnly,
}

/// One planned run shown in the shomloop section.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LoopRef {
    id: u32,
    enabled: bool,
    frequency: String,
}

/// Same fields as [`LoopRef`], held while the manage overlay is open.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LoopPick {
    id: u32,
    enabled: bool,
    frequency: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopVerb {
    Edit,
    Disable,
    Enable,
    Delete,
    Logs,
}

impl MenuState {
    pub fn new(ctx: &MenuContext) -> Self {
        Self {
            category_idx: 0,
            item_idx: 0,
            focus: Focus::Items,
            exit_confirm: false,
            saved_pick: None,
            package_pick: None,
            shomagent_sub: None,
            loop_pick: None,
            web_clear_confirm: false,
            web_history_path: ctx.web_history_path.clone(),
            web_history: ctx.web_history.clone(),
            edit_clear_confirm: false,
            edit_history_path: ctx.edit_history_path.clone(),
            edit_history: ctx.edit_history.clone(),
            allow_root_mode: ctx.allow_root_mode,
            mode: ctx.mode,
            pending_learn: ctx.pending_learn,
            packages_path: ctx.packages_path.clone(),
            shomagent_members: ctx.shomagent_members.clone(),
            theme_backup: None,
            theme_preview: None,
            avatar_backup: None,
        }
    }

    pub fn category(&self) -> Category {
        Category::from_index(self.category_idx)
    }

    fn in_overlay(&self) -> bool {
        self.exit_confirm
            || self.web_clear_confirm
            || self.edit_clear_confirm
            || self.saved_pick.is_some()
            || self.package_pick.is_some()
            || self.loop_pick.is_some()
            || matches!(self.shomagent_sub, Some(ShomagentSub::PromptFile { .. }))
    }

    fn items(&self) -> Vec<Item> {
        if let Some(ShomagentSub::PickMember { kind }) = self.shomagent_sub {
            return member_pick_items(&self.shomagent_members, kind);
        }
        if self.category() == Category::Theme {
            // ● marks the last committed pick; highlight only previews chrome.
            return theme_items(
                self.theme_backup.unwrap_or_else(chrome::active),
                self.avatar_backup.unwrap_or_else(chrome::active_avatar),
            );
        }
        items_for(
            self.category(),
            self.mode,
            self.allow_root_mode,
            self.pending_learn,
            &self.packages_path,
            &self.web_history,
            &self.edit_history,
        )
    }

    pub fn move_up(&mut self) {
        if self.in_overlay() {
            return;
        }
        // Member picker stays locked to the item list until Esc.
        let focus = if matches!(self.shomagent_sub, Some(ShomagentSub::PickMember { .. })) {
            Focus::Items
        } else {
            self.focus
        };
        let was_theme = self.category() == Category::Theme;
        match focus {
            Focus::Categories => {
                if self.category_idx == 0 {
                    self.category_idx = Category::ALL.len() - 1;
                } else {
                    self.category_idx -= 1;
                }
                self.item_idx = 0;
                self.on_category_change(was_theme);
            }
            Focus::Items => {
                let n = self.items().len();
                if n == 0 {
                    return;
                }
                if self.item_idx == 0 {
                    self.item_idx = n - 1;
                } else {
                    self.item_idx -= 1;
                }
                self.preview_theme_selection();
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.in_overlay() {
            return;
        }
        let focus = if matches!(self.shomagent_sub, Some(ShomagentSub::PickMember { .. })) {
            Focus::Items
        } else {
            self.focus
        };
        let was_theme = self.category() == Category::Theme;
        match focus {
            Focus::Categories => {
                self.category_idx = (self.category_idx + 1) % Category::ALL.len();
                self.item_idx = 0;
                self.on_category_change(was_theme);
            }
            Focus::Items => {
                let n = self.items().len();
                if n == 0 {
                    return;
                }
                self.item_idx = (self.item_idx + 1) % n;
                self.preview_theme_selection();
            }
        }
    }

    fn on_category_change(&mut self, was_theme: bool) {
        if self.category() == Category::Theme {
            self.enter_theme_category();
        } else if was_theme {
            self.restore_theme_backup();
        }
    }

    pub fn focus_left(&mut self) {
        if self.exit_confirm {
            self.exit_confirm = false;
            return;
        }
        if self.web_clear_confirm {
            self.web_clear_confirm = false;
            return;
        }
        if self.edit_clear_confirm {
            self.edit_clear_confirm = false;
            return;
        }
        if self.saved_pick.is_some() {
            self.saved_pick = None;
            return;
        }
        if self.package_pick.is_some() {
            self.package_pick = None;
            return;
        }
        if self.loop_pick.is_some() {
            self.loop_pick = None;
            return;
        }
        if self.shomagent_sub.is_some() {
            self.clear_shomagent_sub();
            return;
        }
        self.focus = Focus::Categories;
    }

    pub fn focus_right(&mut self) {
        if self.in_overlay() || self.shomagent_sub.is_some() {
            return;
        }
        self.focus = Focus::Items;
        let n = self.items().len();
        if self.item_idx >= n {
            self.item_idx = n.saturating_sub(1);
        }
        if self.category() == Category::Theme {
            self.enter_theme_category();
        }
    }

    pub fn toggle_pane(&mut self) {
        if self.exit_confirm {
            self.exit_confirm = false;
            return;
        }
        if self.web_clear_confirm {
            self.web_clear_confirm = false;
            return;
        }
        if self.edit_clear_confirm {
            self.edit_clear_confirm = false;
            return;
        }
        if self.saved_pick.is_some() {
            self.saved_pick = None;
            return;
        }
        if self.package_pick.is_some() {
            self.package_pick = None;
            return;
        }
        if self.loop_pick.is_some() {
            self.loop_pick = None;
            return;
        }
        if self.shomagent_sub.is_some() {
            self.clear_shomagent_sub();
            return;
        }
        self.focus = match self.focus {
            Focus::Categories => Focus::Items,
            Focus::Items => Focus::Categories,
        };
    }

    fn clear_shomagent_sub(&mut self) {
        self.shomagent_sub = None;
        self.item_idx = 0;
        self.focus = Focus::Items;
    }

    /// Esc: cancel exit confirm → categories → dismiss.
    pub fn escape(&mut self) -> Option<MenuAction> {
        if self.exit_confirm {
            self.exit_confirm = false;
            return None;
        }
        if self.web_clear_confirm {
            self.web_clear_confirm = false;
            return None;
        }
        if self.edit_clear_confirm {
            self.edit_clear_confirm = false;
            return None;
        }
        if self.saved_pick.is_some() {
            self.saved_pick = None;
            return None;
        }
        if self.package_pick.is_some() {
            self.package_pick = None;
            return None;
        }
        if self.loop_pick.is_some() {
            self.loop_pick = None;
            return None;
        }
        if self.shomagent_sub.is_some() {
            self.clear_shomagent_sub();
            return None;
        }
        if self.focus == Focus::Items {
            if self.category() == Category::Theme {
                self.restore_theme_backup();
            }
            self.focus = Focus::Categories;
            return None;
        }
        self.restore_theme_backup();
        Some(MenuAction::Dismiss)
    }

    fn restore_theme_backup(&mut self) {
        if let Some(t) = self.theme_backup.take() {
            chrome::set_active(t);
        }
        if let Some(a) = self.avatar_backup.take() {
            chrome::set_active_avatar(a);
        }
        self.theme_preview = None;
    }

    /// Enter Theme category: remember color + avatar to restore on Esc; preview selection.
    fn enter_theme_category(&mut self) {
        if self.theme_backup.is_none() {
            self.theme_backup = Some(chrome::active());
        }
        if self.avatar_backup.is_none() {
            self.avatar_backup = Some(chrome::active_avatar());
        }
        self.preview_theme_selection();
    }

    /// Live-preview chrome / avatar for the highlighted row. Does not persist;
    /// Esc restores [`Self::theme_backup`] / [`Self::avatar_backup`].
    fn preview_theme_selection(&mut self) {
        if self.category() != Category::Theme {
            return;
        }
        let items = self.items();
        match items.get(self.item_idx).map(|i| &i.kind) {
            Some(ItemKind::SetTheme(t)) => {
                chrome::set_active(*t);
                self.theme_preview = Some(*t);
            }
            Some(ItemKind::SetAvatar(a)) => {
                chrome::set_active_avatar(*a);
            }
            _ => {}
        }
    }

    /// Enter / confirm on the current selection.
    pub fn activate(&mut self) -> Option<MenuAction> {
        if self.exit_confirm {
            return Some(MenuAction::ExitShell);
        }
        if self.web_clear_confirm {
            self.confirm_web_clear();
            return None;
        }
        if self.edit_clear_confirm {
            self.confirm_edit_clear();
            return None;
        }
        if self.saved_pick.is_some() {
            return self.confirm_saved(false);
        }
        if self.package_pick.is_some() {
            return self.confirm_package();
        }
        if self.loop_pick.is_some() {
            return None;
        }
        if matches!(self.shomagent_sub, Some(ShomagentSub::PromptFile { .. })) {
            return self.confirm_filename();
        }
        if self.focus == Focus::Categories {
            self.focus = Focus::Items;
            if self.category() == Category::Theme {
                self.enter_theme_category();
            }
            return None;
        }
        let items = self.items();
        let item = items.get(self.item_idx)?;
        match &item.kind {
            ItemKind::SetMode(m) => Some(MenuAction::SetMode(*m)),
            ItemKind::RunMeta(cmd) => Some(MenuAction::RunMeta((*cmd).to_string())),
            ItemKind::InsertLine(s) => Some(MenuAction::InsertLine(s.clone())),
            ItemKind::Saved(name) => {
                self.saved_pick = Some(name.clone());
                None
            }
            ItemKind::TrackedPackage { name, status } => {
                self.package_pick = Some((name.clone(), *status));
                None
            }
            ItemKind::ShomagentPromptFile(kind) => {
                self.shomagent_sub = Some(ShomagentSub::PromptFile {
                    kind: *kind,
                    buf: String::new(),
                });
                None
            }
            ItemKind::ShomagentPickMember(kind) => {
                self.shomagent_sub = Some(ShomagentSub::PickMember { kind: *kind });
                self.item_idx = 0;
                self.focus = Focus::Items;
                None
            }
            ItemKind::ShomagentMember { id, verb } => {
                let line = match verb {
                    MemberVerb::Ask => format!("shomagent ask {id} "),
                    MemberVerb::Talk => format!("shomagent talk {id}"),
                    MemberVerb::Stop => format!("shomagent stop {id}"),
                };
                Some(MenuAction::InsertLine(line))
            }
            ItemKind::ExitShell => {
                self.exit_confirm = true;
                None
            }
            ItemKind::ClearWebHistory => {
                self.web_clear_confirm = true;
                None
            }
            ItemKind::OpenWeb(url) => Some(MenuAction::RunMeta(format!(":web {url}"))),
            ItemKind::ClearEditHistory => {
                self.edit_clear_confirm = true;
                None
            }
            ItemKind::OpenEdit(path) => Some(MenuAction::RunMeta(format!(":edit {path}"))),
            ItemKind::Loop(loop_ref) => {
                self.loop_pick = Some(LoopPick {
                    id: loop_ref.id,
                    enabled: loop_ref.enabled,
                    frequency: loop_ref.frequency.clone(),
                });
                None
            }
            ItemKind::SetTheme(t) => {
                // Commit color; stay in the menu so avatar can be picked too.
                let _ = chrome::save_theme(*t);
                chrome::set_active(*t);
                self.theme_backup = Some(*t);
                self.theme_preview = Some(*t);
                None
            }
            ItemKind::SetAvatar(a) => {
                let _ = chrome::save_avatar(*a);
                chrome::set_active_avatar(*a);
                self.avatar_backup = Some(*a);
                None
            }
            ItemKind::InfoOnly => None,
        }
    }

    /// Type into the filename prompt (printable ASCII).
    pub fn filename_push(&mut self, c: char) {
        if let Some(ShomagentSub::PromptFile { buf, .. }) = &mut self.shomagent_sub {
            if !c.is_control() && buf.len() < 200 {
                buf.push(c);
            }
        }
    }

    /// Backspace in the filename prompt.
    pub fn filename_backspace(&mut self) {
        if let Some(ShomagentSub::PromptFile { buf, .. }) = &mut self.shomagent_sub {
            buf.pop();
        }
    }

    /// Confirm a typed filename → insert the matching shomagent command.
    pub fn confirm_filename(&mut self) -> Option<MenuAction> {
        let Some(ShomagentSub::PromptFile { kind, buf }) = self.shomagent_sub.take() else {
            return None;
        };
        let path = buf.trim();
        if path.is_empty() {
            // Keep prompting until the user types something or Esc.
            self.shomagent_sub = Some(ShomagentSub::PromptFile { kind, buf });
            return None;
        }
        let line = match kind {
            FilePromptKind::ReadWrite => format!("shomagent readwrite {path}"),
            FilePromptKind::DreamSeed => format!("shomagent dream seed {path}"),
            FilePromptKind::RefineIdea => format!("shomagent refine idea {path}"),
            FilePromptKind::WebUrl => format!("shomagent web {path}"),
        };
        Some(MenuAction::InsertLine(line))
    }

    /// Double-Esc while open: always quit the menu.
    pub fn force_dismiss(&mut self) -> MenuAction {
        self.restore_theme_backup();
        MenuAction::Dismiss
    }

    /// `delete` removes the picked command and stays in the menu.
    /// Otherwise the command name is returned to run.
    pub fn confirm_saved(&mut self, delete: bool) -> Option<MenuAction> {
        let Some(name) = self.saved_pick.take() else {
            return None;
        };
        if !delete {
            return Some(MenuAction::InsertLine(name));
        }
        let _ = shomsky::remove_entry(&shomsky::LearnPaths::from_env(), &name);
        let n = self.items().len();
        if n == 0 {
            self.item_idx = 0;
        } else if self.item_idx >= n {
            self.item_idx = n - 1;
        }
        None
    }

    /// Confirm uninstall / reinstall for the picked package → run via REPL.
    pub fn confirm_package(&mut self) -> Option<MenuAction> {
        let Some((name, status)) = self.package_pick.take() else {
            return None;
        };
        let cmd = match status {
            PackageStatus::Installed => format!("apk del {name}"),
            PackageStatus::Removed => format!("apk add {name}"),
        };
        Some(MenuAction::InsertLine(cmd))
    }

    /// Overlay action for the picked planned run. Edit keeps a trailing space
    /// so the REPL prefills the line; the other verbs run immediately.
    fn confirm_loop(&mut self, verb: LoopVerb) -> Option<MenuAction> {
        let Some(pick) = self.loop_pick.take() else {
            return None;
        };
        let line = match verb {
            LoopVerb::Edit => {
                format!("shomloop edit {} {} ", pick.id, quote_freq(&pick.frequency))
            }
            LoopVerb::Disable => format!("shomloop disable {}", pick.id),
            LoopVerb::Enable => format!("shomloop enable {}", pick.id),
            LoopVerb::Delete => format!("shomloop delete {}", pick.id),
            LoopVerb::Logs => format!("shomloop logs {}", pick.id),
        };
        Some(MenuAction::InsertLine(line))
    }

    fn confirm_web_clear(&mut self) {
        let _ = crate::web_history::clear(&self.web_history_path);
        self.web_history.clear();
        self.web_clear_confirm = false;
        self.item_idx = 0;
    }

    fn confirm_edit_clear(&mut self) {
        let _ = crate::edit_history::clear(&self.edit_history_path);
        self.edit_history.clear();
        self.edit_clear_confirm = false;
        self.item_idx = 0;
    }
}

fn items_for(
    cat: Category,
    mode: Mode,
    allow_root: bool,
    pending_learn: bool,
    packages_path: &PathBuf,
    web_history: &[crate::web_history::Visit],
    edit_history: &[crate::edit_history::EditVisit],
) -> Vec<Item> {
    match cat {
        Category::Editor => editor_items(edit_history),
        Category::Modes => {
            let mut v = vec![
                mode_item(Mode::Agent, mode),
                mode_item(Mode::Hallucination, mode),
            ];
            if allow_root {
                v.push(mode_item(Mode::Root, mode));
            }
            v.push(Item {
                label: format!("current: {}", mode),
                detail: "Operating mode (client-side; model cannot escalate)".into(),
                kind: ItemKind::InfoOnly,
            });
            v
        }
        Category::Learn => {
            let mut v = vec![
                Item {
                    label: ", <intent>".into(),
                    detail: "Plan → confirm → run a real shell command".into(),
                    kind: ItemKind::InsertLine(", ".into()),
                },
                Item {
                    label: "?? <intent>".into(),
                    detail: "Same as comma prefix".into(),
                    kind: ItemKind::InsertLine("?? ".into()),
                },
            ];
            if pending_learn {
                v.push(Item {
                    label: "s / save".into(),
                    detail: "Promote the last learn run into ~/bin".into(),
                    kind: ItemKind::InsertLine("s".into()),
                });
            } else {
                v.push(Item {
                    label: "s / save".into(),
                    detail: "Available after a learn run completes".into(),
                    kind: ItemKind::InfoOnly,
                });
            }
            v
        }
        Category::Scripts => saved_command_items(),
        Category::Packages => package_items(packages_path),
        Category::Shomagent => shomagent_root_items(),
        Category::Web => web_items(web_history),
        Category::Shomloop => shomloop_items(),
        Category::Help => vec![
            Item {
                label: "full help".into(),
                detail: "Show :help text".into(),
                kind: ItemKind::RunMeta(":help"),
            },
            Item {
                label: "this menu".into(),
                detail: ":menu / :m — Editor, Help, Learn, Modes, …, Web, Theme".into(),
                kind: ItemKind::InfoOnly,
            },
        ],
        Category::Theme => theme_items(chrome::active(), chrome::active_avatar()),
        Category::System => vec![
            Item {
                label: ":sys".into(),
                detail: "Identity (mode, model, tools, llm)".into(),
                kind: ItemKind::RunMeta(":sys"),
            },
            Item {
                label: ":emerge".into(),
                detail: if crate::emerge::enabled() {
                    "Serve this shell at http://localhost:1337".into()
                } else {
                    "Disabled (ENGRAM_EMERGE=off)".into()
                },
                kind: if crate::emerge::enabled() {
                    ItemKind::RunMeta(":emerge")
                } else {
                    ItemKind::InfoOnly
                },
            },
            Item {
                label: ":splash".into(),
                detail: "Replay the startup splash art".into(),
                kind: ItemKind::RunMeta(":splash"),
            },
            Item {
                label: ":quickyn".into(),
                detail: "Split-screen yes/no TUI (question + file → check/X)".into(),
                kind: ItemKind::RunMeta(":quickyn"),
            },
            Item {
                label: ":chat".into(),
                detail: "Framed chat with the current model (ctx, compactify, save)".into(),
                kind: ItemKind::RunMeta(":chat"),
            },
            Item {
                label: ":sine".into(),
                detail: "SINE plan-then-approve assistant (roster, chat, idle playbook)".into(),
                kind: ItemKind::RunMeta(":sine"),
            },
            Item {
                label: ":edit".into(),
                detail: "File editor + AI chat (picker if no path)".into(),
                kind: ItemKind::RunMeta(":edit"),
            },
            Item {
                label: ":model".into(),
                detail: "Active model / LLM endpoint".into(),
                kind: ItemKind::RunMeta(":model"),
            },
            Item {
                label: ":skills".into(),
                detail: "List skills under /engram/skills".into(),
                kind: ItemKind::RunMeta(":skills"),
            },
            Item {
                label: ":top".into(),
                detail: "Most-used shell commands".into(),
                kind: ItemKind::RunMeta(":top"),
            },
            Item {
                label: ":packages".into(),
                detail: "apk install / uninstall ledger".into(),
                kind: ItemKind::RunMeta(":packages"),
            },
            Item {
                label: ":tools".into(),
                detail: "Show tool style".into(),
                kind: ItemKind::RunMeta(":tools"),
            },
            Item {
                label: ":logs".into(),
                detail: "Tail mistralrs log".into(),
                kind: ItemKind::RunMeta(":logs"),
            },
            Item {
                label: ":clear".into(),
                detail: "Clear conversation / new halluc session".into(),
                kind: ItemKind::RunMeta(":clear"),
            },
            Item {
                label: "exit".into(),
                detail: "Leave the shell (confirm)".into(),
                kind: ItemKind::ExitShell,
            },
        ],
    }
}

fn shomloop_items() -> Vec<Item> {
    let mut v = vec![
        Item {
            label: "new loop".into(),
            detail: "Prefill shomloop — type a frequency and a command".into(),
            kind: ItemKind::InsertLine("shomloop ".into()),
        },
        Item {
            label: "logs".into(),
            detail: "Show every log under /tmp/shomloop".into(),
            kind: ItemKind::InsertLine("shomloop logs".into()),
        },
    ];
    match shomloop::list_runs() {
        Ok(runs) if runs.is_empty() => {
            v.push(Item {
                label: "(no planned runs)".into(),
                detail: "shomloop \"daily at 5pm\" <command>".into(),
                kind: ItemKind::InfoOnly,
            });
        }
        Ok(runs) => {
            for run in runs {
                let state = if run.enabled { "on " } else { "off" };
                v.push(Item {
                    label: format!("{state} #{}  {}", run.id, run.frequency),
                    detail: format!(
                        "{}  — Enter: edit / disable / enable / delete / logs",
                        run.command
                    ),
                    kind: ItemKind::Loop(LoopRef {
                        id: run.id,
                        enabled: run.enabled,
                        frequency: run.frequency,
                    }),
                });
            }
        }
        Err(e) => {
            v.push(Item {
                label: "(could not read planned runs)".into(),
                detail: e.to_string(),
                kind: ItemKind::InfoOnly,
            });
        }
    }
    v
}

/// Digits `1`–`9` → category indices 0–8; `0` → index 9 (10th category).
fn digit_category_index(c: char) -> Option<usize> {
    let idx = match c {
        '1' => 0,
        '2' => 1,
        '3' => 2,
        '4' => 3,
        '5' => 4,
        '6' => 5,
        '7' => 6,
        '8' => 7,
        '9' => 8,
        '0' => 9,
        _ => return None,
    };
    if idx < Category::ALL.len() {
        Some(idx)
    } else {
        None
    }
}

fn quote_freq(freq: &str) -> String {
    let mut out = String::from("\"");
    for c in freq.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn web_items(history: &[crate::web_history::Visit]) -> Vec<Item> {
    let mut v = vec![
        Item {
            label: "open browser".into(),
            detail: "Split text browser — page left, chat right".into(),
            kind: ItemKind::RunMeta(":web"),
        },
        Item {
            label: "clear history".into(),
            detail: "Forget saved sites".into(),
            kind: ItemKind::ClearWebHistory,
        },
    ];
    if history.is_empty() {
        v.push(Item {
            label: "(no sites yet — open browser and visit a url)".into(),
            detail: "Visits are saved under .web_history.json".into(),
            kind: ItemKind::InfoOnly,
        });
    } else {
        for visit in history {
            let label = if visit.title.is_empty() {
                visit.url.clone()
            } else {
                format!("{}  {}", visit.title, visit.url)
            };
            v.push(Item {
                label,
                detail: visit.url.clone(),
                kind: ItemKind::OpenWeb(visit.url.clone()),
            });
        }
    }
    v
}

fn theme_items(committed_theme: Theme, committed_avatar: Avatar) -> Vec<Item> {
    let mut v = vec![Item {
        label: "── color ──".into(),
        detail: "Highlight previews · Space/Enter commits · Esc restores".into(),
        kind: ItemKind::InfoOnly,
    }];
    for t in Theme::ALL {
        let mark = if t == committed_theme { " ●" } else { "" };
        v.push(Item {
            label: format!("  {}{mark}", t.name()),
            detail: t.detail().into(),
            kind: ItemKind::SetTheme(t),
        });
    }
    v.push(Item {
        label: "── avatar ──".into(),
        detail: "Highlight previews · Space/Enter commits · Esc restores".into(),
        kind: ItemKind::InfoOnly,
    });
    for a in Avatar::ALL {
        let mark = if a == committed_avatar { " ●" } else { "" };
        v.push(Item {
            label: format!("  {}{mark}", a.label()),
            detail: a.detail().into(),
            kind: ItemKind::SetAvatar(a),
        });
    }
    v
}

fn editor_items(history: &[crate::edit_history::EditVisit]) -> Vec<Item> {
    let mut v = vec![
        Item {
            label: "open editor".into(),
            detail: "Pick a file or create one, then edit with AI chat".into(),
            kind: ItemKind::RunMeta(":edit"),
        },
        Item {
            label: "clear history".into(),
            detail: "Forget recent edit paths".into(),
            kind: ItemKind::ClearEditHistory,
        },
    ];
    if history.is_empty() {
        v.push(Item {
            label: "(no recent files — open editor to start)".into(),
            detail: "Paths are saved under .edit_history.json".into(),
            kind: ItemKind::InfoOnly,
        });
    } else {
        for visit in history {
            v.push(Item {
                label: visit.path.clone(),
                detail: visit.path.clone(),
                kind: ItemKind::OpenEdit(visit.path.clone()),
            });
        }
    }
    v
}

fn shomagent_root_items() -> Vec<Item> {
    vec![
        Item {
            label: "list agents".into(),
            detail: "Show the live shomagent roster".into(),
            kind: ItemKind::RunMeta(":shomagent"),
        },
        Item {
            label: "readwrite <file>".into(),
            detail: "Attach a read/write document — type a filename".into(),
            kind: ItemKind::ShomagentPromptFile(FilePromptKind::ReadWrite),
        },
        Item {
            label: "web <url>".into(),
            detail: "Attach a live headless-browser page — type a URL".into(),
            kind: ItemKind::ShomagentPromptFile(FilePromptKind::WebUrl),
        },
        Item {
            label: "ask <opened file>".into(),
            detail: "One-shot question to an attached document".into(),
            kind: ItemKind::ShomagentPickMember(MemberVerb::Ask),
        },
        Item {
            label: "talk <opened file>".into(),
            detail: "Interactive chat with an attached document".into(),
            kind: ItemKind::ShomagentPickMember(MemberVerb::Talk),
        },
        Item {
            label: "dream seed <file>".into(),
            detail: "Plant your own seed, then dream — type a filename".into(),
            kind: ItemKind::ShomagentPromptFile(FilePromptKind::DreamSeed),
        },
        Item {
            label: "refine idea <file>".into(),
            detail: "Rewrite an idea file from a standing task — type a filename".into(),
            kind: ItemKind::ShomagentPromptFile(FilePromptKind::RefineIdea),
        },
        Item {
            label: "stop / end <agent>".into(),
            detail: "Stop a watcher or detach a document (file kept)".into(),
            kind: ItemKind::ShomagentPickMember(MemberVerb::Stop),
        },
    ]
}

fn member_pick_items(members: &[ShomagentMemberBrief], verb: MemberVerb) -> Vec<Item> {
    let pool: Vec<_> = match verb {
        MemberVerb::Ask | MemberVerb::Talk => members.iter().filter(|m| m.is_document()).collect(),
        MemberVerb::Stop => members.iter().collect(),
    };
    if pool.is_empty() {
        let hint = match verb {
            MemberVerb::Ask | MemberVerb::Talk => {
                "No opened documents — use readwrite <file> first"
            }
            MemberVerb::Stop => "No agents — nothing to stop",
        };
        return vec![Item {
            label: "(none)".into(),
            detail: hint.into(),
            kind: ItemKind::InfoOnly,
        }];
    }
    let verb_word = match verb {
        MemberVerb::Ask => "ask",
        MemberVerb::Talk => "talk",
        MemberVerb::Stop => "stop",
    };
    pool.into_iter()
        .map(|m| {
            let glyph = match m.state.as_str() {
                "read" => "◇",
                "readwrite" => "◆",
                "dream" => "~",
                "refine" => "✦",
                "running" => "▶",
                "paused" => "⏸",
                "stopped" | "exited" => "■",
                _ => "■",
            };
            let detail = match verb {
                MemberVerb::Stop if m.is_document() => {
                    format!("detach → {} (file kept)", m.path)
                }
                _ => format!("{verb_word} → {}", m.path),
            };
            Item {
                label: format!("#{id} {glyph} {name}", id = m.id, name = m.name),
                detail,
                kind: ItemKind::ShomagentMember { id: m.id, verb },
            }
        })
        .collect()
}

fn saved_command_items() -> Vec<Item> {
    let entries = shomsky::list_entries(&shomsky::LearnPaths::from_env());
    if entries.is_empty() {
        return vec![Item {
            label: "(none)".into(),
            detail: "No saved commands yet. Learn one, then save it.".into(),
            kind: ItemKind::InfoOnly,
        }];
    }
    entries
        .into_iter()
        .map(|e| {
            let detail = if e.description.is_empty() {
                e.command
            } else {
                e.description
            };
            Item {
                label: e.name.clone(),
                detail,
                kind: ItemKind::Saved(e.name),
            }
        })
        .collect()
}

fn package_items(path: &PathBuf) -> Vec<Item> {
    let store = crate::packages::PackageStore::load(path).unwrap_or_default();
    let mut items = vec![Item {
        label: "list all".into(),
        detail: "Run :packages — full install/uninstall ledger".into(),
        kind: ItemKind::RunMeta(":packages"),
    }];
    let listed = store.list_sorted();
    if listed.is_empty() {
        items.push(Item {
            label: "(none)".into(),
            detail: "Successful apk add / apk del are recorded here.".into(),
            kind: ItemKind::InfoOnly,
        });
        return items;
    }
    for (name, entry) in listed {
        let essential = crate::packages::is_essential(name);
        let (label, detail) = match entry.status {
            PackageStatus::Installed if essential => (
                format!("● {name}"),
                format!(
                    "installed (essential) — Enter to uninstall ({})",
                    entry.updated_at
                ),
            ),
            PackageStatus::Installed => (
                format!("● {name}"),
                format!("installed — Enter to uninstall ({})", entry.updated_at),
            ),
            PackageStatus::Removed => (
                format!("○ {name}"),
                format!("removed — Enter to reinstall ({})", entry.updated_at),
            ),
        };
        items.push(Item {
            label,
            detail,
            kind: ItemKind::TrackedPackage {
                name: name.to_string(),
                status: entry.status,
            },
        });
    }
    items
}

fn mode_item(target: Mode, current: Mode) -> Item {
    let mark = if target == current { "● " } else { "  " };
    Item {
        label: format!("{mark}{target}"),
        detail: match target {
            Mode::Agent => "LLM + real tools (default)".into(),
            Mode::Hallucination => "Guided virtual shell; no disk writes".into(),
            Mode::Root => "Bypass LLM; BusyBox ash".into(),
        },
        kind: ItemKind::SetMode(target),
    }
}

/// Whether the interactive overlay should run (TTY + color-capable term).
pub fn menu_available() -> bool {
    ui::splash_enabled() && ui::color_enabled()
}

/// Run the overlay. Returns [`MenuAction::Dismiss`] if the TTY is unavailable.
pub fn run(ctx: &MenuContext) -> MenuAction {
    if !menu_available() {
        println!("menu: needs a color TTY — use :mode / :help / :shomagent, or , intent");
        return MenuAction::Dismiss;
    }

    let guard = RawGuard::enter_blocking();
    if !guard.active() {
        println!("menu: could not enter raw mode — try :menu from a real TTY");
        return MenuAction::Dismiss;
    }

    let mut out = io::stdout().lock();
    let _ = write!(out, "\x1b[?1049h\x1b[?25l");
    let _ = out.flush();

    let mut state = MenuState::new(ctx);
    let action = event_loop(&mut state);

    let _ = write!(out, "\x1b[?1049l\x1b[?25h\x1b[0m");
    let _ = out.flush();
    // RawGuard restores stty on drop.
    drop(guard);
    action
}

fn event_loop(state: &mut MenuState) -> MenuAction {
    let mut esc_armed = false;
    loop {
        draw(state);
        let typing_filename = matches!(state.shomagent_sub, Some(ShomagentSub::PromptFile { .. }));
        match read_key() {
            Key::Up if !typing_filename => {
                esc_armed = false;
                state.move_up();
            }
            Key::Char('k') | Key::Char('K') if !typing_filename => {
                esc_armed = false;
                state.move_up();
            }
            Key::Down if !typing_filename => {
                esc_armed = false;
                state.move_down();
            }
            Key::Char('j') | Key::Char('J') if !typing_filename => {
                esc_armed = false;
                state.move_down();
            }
            Key::Left if !typing_filename => {
                esc_armed = false;
                state.focus_left();
            }
            Key::Char('h') | Key::Char('H') if !typing_filename => {
                esc_armed = false;
                state.focus_left();
            }
            Key::Right if !typing_filename => {
                esc_armed = false;
                state.focus_right();
            }
            Key::Char('l') | Key::Char('L') if !typing_filename => {
                esc_armed = false;
                state.focus_right();
            }
            Key::Tab if !typing_filename => {
                esc_armed = false;
                state.toggle_pane();
            }
            Key::Char(c)
                if state.loop_pick.is_some()
                    && matches!(c, 'e' | 'E' | 'd' | 'D' | 'n' | 'N' | 'x' | 'X' | 'l' | 'L') =>
            {
                esc_armed = false;
                let verb = match c.to_ascii_lowercase() {
                    'e' => LoopVerb::Edit,
                    'd' => LoopVerb::Disable,
                    'n' => LoopVerb::Enable,
                    'x' => LoopVerb::Delete,
                    _ => LoopVerb::Logs,
                };
                if let Some(a) = state.confirm_loop(verb) {
                    return a;
                }
            }
            Key::Enter => {
                esc_armed = false;
                if let Some(a) = state.activate() {
                    return a;
                }
            }
            Key::Char(' ') if !typing_filename => {
                // Space commits Theme color/avatar (and activates other items).
                esc_armed = false;
                if let Some(a) = state.activate() {
                    return a;
                }
            }
            Key::Backspace if typing_filename => {
                esc_armed = false;
                state.filename_backspace();
            }
            Key::Char(c) if typing_filename => {
                esc_armed = false;
                state.filename_push(c);
            }
            Key::Esc => {
                if esc_armed {
                    return state.force_dismiss();
                }
                esc_armed = true;
                if let Some(a) = state.escape() {
                    return a;
                }
            }
            Key::Char('q') | Key::Char('Q') if !typing_filename => {
                return state.force_dismiss();
            }
            Key::Char('y') | Key::Char('Y') if state.exit_confirm => {
                return MenuAction::ExitShell;
            }
            Key::Char('n') | Key::Char('N') if state.exit_confirm => {
                state.exit_confirm = false;
                esc_armed = false;
            }
            Key::Char('y') | Key::Char('Y') if state.web_clear_confirm => {
                state.confirm_web_clear();
                esc_armed = false;
            }
            Key::Char('n') | Key::Char('N') if state.web_clear_confirm => {
                state.web_clear_confirm = false;
                esc_armed = false;
            }
            Key::Char('y') | Key::Char('Y') if state.edit_clear_confirm => {
                state.confirm_edit_clear();
                esc_armed = false;
            }
            Key::Char('n') | Key::Char('N') if state.edit_clear_confirm => {
                state.edit_clear_confirm = false;
                esc_armed = false;
            }
            Key::Char('u') | Key::Char('U')
                if state
                    .package_pick
                    .as_ref()
                    .is_some_and(|(_, s)| *s == PackageStatus::Installed) =>
            {
                esc_armed = false;
                if let Some(a) = state.confirm_package() {
                    return a;
                }
            }
            Key::Char('r') | Key::Char('R')
                if state
                    .package_pick
                    .as_ref()
                    .is_some_and(|(_, s)| *s == PackageStatus::Removed) =>
            {
                esc_armed = false;
                if let Some(a) = state.confirm_package() {
                    return a;
                }
            }
            Key::Char('r') | Key::Char('R') if state.saved_pick.is_some() => {
                esc_armed = false;
                if let Some(a) = state.confirm_saved(false) {
                    return a;
                }
            }
            Key::Char('d') | Key::Char('D') if state.saved_pick.is_some() => {
                esc_armed = false;
                let _ = state.confirm_saved(true);
            }
            Key::Char(d)
                if state.shomagent_sub.is_none()
                    && !state.in_overlay()
                    && digit_category_index(d).is_some() =>
            {
                esc_armed = false;
                let was_theme = state.category() == Category::Theme;
                state.category_idx = digit_category_index(d).unwrap();
                state.item_idx = 0;
                state.focus = Focus::Items;
                state.on_category_change(was_theme);
            }
            _ => {
                esc_armed = false;
            }
        }
    }
}

fn draw(state: &MenuState) {
    let (rows, cols) = term::term_size();
    let cols = cols.clamp(40, term::MAX_COLS);
    let rows = rows.clamp(12, term::MAX_ROWS);
    let pal = chrome::palette();
    let border = pal.border;
    let title_st = pal.title;
    let dim = pal.dim;
    let text = pal.text;
    let sel = pal.selection();
    let bold = title_st;

    let mut canvas = Canvas::new(cols, rows);
    let outer = Rect::new(0, 0, cols, rows);
    let footer = if state.category() == Category::Theme {
        "↑↓ preview · Space/Enter select · Esc restore · Esc Esc quit"
    } else {
        "↑↓ · Tab · Enter · Esc back · Esc Esc quit"
    };
    canvas.frame(outer, "shomsky menu", footer, border, title_st);

    let cat_w = 14usize.min(cols.saturating_sub(20)).max(10);
    // Inner area: row 1..rows-2, col 1..cols-2
    let inner = outer.inner();
    // Detail row + detail rule take 2 rows at bottom of inner.
    let list_h = inner.h.saturating_sub(2).max(4);
    let show_avatar_preview = state.category() == Category::Theme && cols >= 80;
    // Brand art is up to 48 cols; keep a usable item list beside it.
    let preview_w = if show_avatar_preview {
        48usize.min(inner.w.saturating_sub(cat_w + 14)).max(28)
    } else {
        0
    };
    let mid_w = inner.w.saturating_sub(cat_w + 1 + if preview_w > 0 { preview_w + 1 } else { 0 });
    let div_x = inner.x + cat_w;
    let preview_div_x = if preview_w > 0 {
        Some(div_x + 1 + mid_w)
    } else {
        None
    };
    let list_rect = Rect::new(inner.x, inner.y, inner.w, list_h + 1); // include detail rule row
    canvas.split_col(list_rect, div_x, border);
    if let Some(px) = preview_div_x {
        canvas.split_col(list_rect, px, border);
    }
    let detail_rule_y = inner.y + list_h;
    canvas.split_row(
        Rect::new(inner.x, inner.y, inner.w, list_h + 1),
        detail_rule_y,
        border,
    );

    let items = state.items();
    let item_w = mid_w.max(8);
    let item_x = div_x + 1;
    let body_rows = list_h;

    for row in 0..body_rows {
        let y = inner.y + row;
        // Categories
        if row < Category::ALL.len() {
            let c = Category::from_index(row);
            let selected = row == state.category_idx;
            let focused = state.focus == Focus::Categories && selected;
            let label = format!(" {}", c.label());
            let style = if focused {
                sel
            } else if selected {
                bold
            } else {
                dim
            };
            canvas.fill_row(inner.x, y, cat_w, &label, style);
        } else {
            canvas.fill_row(inner.x, y, cat_w, "", text);
        }

        // Items
        let item_offset = state.item_idx.saturating_sub(body_rows.saturating_sub(1));
        if let Some(item) = items.get(row + item_offset) {
            let selected = row + item_offset == state.item_idx;
            let focused = state.focus == Focus::Items && selected;
            let mark = if focused { "▶" } else { " " };
            let label = format!("{mark}{}", item.label);
            let style = if focused {
                sel
            } else if selected {
                bold
            } else {
                text
            };
            canvas.fill_row(item_x, y, item_w, &label, style);
        } else {
            canvas.fill_row(item_x, y, item_w, "", text);
        }
    }

    // Live avatar preview while browsing Theme (updates as ↑↓ moves).
    if let Some(px) = preview_div_x {
        let preview_x = px + 1;
        let avatar = theme_preview_avatar(state, &items);
        let art = crate::sine_tui::preview_avatar_lines(avatar);
        let head = format!(" {}", avatar.label());
        canvas.fill_row(preview_x, inner.y, preview_w, &head, title_st);
        for (i, line) in art.iter().enumerate() {
            let y = inner.y + 1 + i;
            if y >= detail_rule_y {
                break;
            }
            canvas.put_ansi_row(preview_x, y, preview_w, line, text);
        }
    }

    let detail = menu_detail(state, &items);
    let detail_y = detail_rule_y + 1;
    if detail_y < rows.saturating_sub(1) {
        canvas.fill_row(inner.x, detail_y, inner.w, &format!(" {detail}"), dim);
    }

    // Overlay confirms (floating box)
    if state.in_overlay() {
        let lines = [detail.as_str()];
        canvas.overlay(
            outer,
            (inner.w * 3 / 4).max(24).min(inner.w),
            5,
            "confirm",
            &lines,
            pal.overlay_border,
            title_st,
            text,
        );
    }

    canvas.flush();
}

/// Avatar shown in the Theme preview pane for the highlighted row.
fn theme_preview_avatar(state: &MenuState, items: &[Item]) -> Avatar {
    match items.get(state.item_idx).map(|i| &i.kind) {
        Some(ItemKind::SetAvatar(a)) => *a,
        _ => chrome::active_avatar(),
    }
}

fn menu_detail(state: &MenuState, items: &[Item]) -> String {
    if state.exit_confirm {
        "Exit the shell? [y]es / [n]o / Esc".to_string()
    } else if state.web_clear_confirm {
        "Clear visit history? [y]es / [n]o / Esc".to_string()
    } else if state.edit_clear_confirm {
        "Clear edit history? [y]es / [n]o / Esc".to_string()
    } else if let Some(name) = &state.saved_pick {
        format!("`{name}`  [r]un / [d]elete / Esc")
    } else if let Some(pick) = &state.loop_pick {
        let state_word = if pick.enabled { "on" } else { "off" };
        format!(
            "#{} {state_word} {}  [e]dit [d]isable [n] enable [x] delete [l]ogs",
            pick.id, pick.frequency
        )
    } else if let Some((name, status)) = &state.package_pick {
        match status {
            PackageStatus::Installed if crate::packages::is_essential(name) => {
                format!("ESSENTIAL `{name}`  [u]ninstall anyway / Esc")
            }
            PackageStatus::Installed => {
                format!("`{name}`  [u]ninstall / Enter / Esc")
            }
            PackageStatus::Removed => {
                format!("`{name}`  [r]einstall / Enter / Esc")
            }
        }
    } else if let Some(ShomagentSub::PromptFile { kind, buf }) = &state.shomagent_sub {
        let tag = match kind {
            FilePromptKind::ReadWrite => "readwrite file",
            FilePromptKind::DreamSeed => "dream seed file",
            FilePromptKind::RefineIdea => "refine idea file",
            FilePromptKind::WebUrl => "web url",
        };
        format!("{tag}: {buf}█  Enter · Esc")
    } else if let Some(ShomagentSub::PickMember { kind }) = &state.shomagent_sub {
        match kind {
            MemberVerb::Stop => "pick an agent to stop / detach · Esc back".to_string(),
            _ => "pick an opened document · Esc back".to_string(),
        }
    } else {
        items
            .get(state.item_idx)
            .map(|i| i.detail.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Tab,
    Backspace,
    Char(char),
    Other,
}

fn read_key() -> Key {
    let mut stdin = io::stdin();
    let mut b = [0u8; 1];
    if stdin.read(&mut b).ok() != Some(1) {
        return Key::Other;
    }
    match b[0] {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x08 | 0x7f => Key::Backspace,
        0x1b => {
            // Either bare Esc or a CSI sequence. Peek briefly for '['.
            // Switch to non-blocking poll for the rest of the sequence.
            term::stty(&["raw", "-echo", "min", "0", "time", "0"]);
            std::thread::sleep(Duration::from_millis(30));
            let mut rest = [0u8; 8];
            let n = stdin.read(&mut rest).unwrap_or(0);
            term::stty(&["raw", "-echo", "min", "1", "time", "0"]);
            if n == 0 {
                return Key::Esc;
            }
            if rest[0] == b'[' && n >= 2 {
                return match rest[1] {
                    b'A' => Key::Up,
                    b'B' => Key::Down,
                    b'C' => Key::Right,
                    b'D' => Key::Left,
                    _ => Key::Other,
                };
            }
            // Second Esc in a quick succession arrives as another 0x1b.
            if rest[0] == 0x1b {
                return Key::Esc;
            }
            Key::Other
        }
        c if c >= 0x20 && c < 0x7f => Key::Char(c as char),
        _ => Key::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> MenuContext {
        MenuContext {
            mode: Mode::Agent,
            allow_root_mode: true,
            pending_learn: false,
            packages_path: PathBuf::from("/engram/user/.packages.json"),
            web_history_path: PathBuf::from("/engram/user/.web_history.json"),
            web_history: vec![],
            edit_history_path: PathBuf::from("/engram/user/.edit_history.json"),
            edit_history: vec![],
            shomagent_members: vec![],
        }
    }

    fn ctx_with_docs(docs: Vec<ShomagentMemberBrief>) -> MenuContext {
        let mut c = ctx();
        c.shomagent_members = docs;
        c
    }

    #[test]
    fn activate_sets_mode() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Modes.index();
        // Modes category, first item = agent
        assert_eq!(s.activate(), Some(MenuAction::SetMode(Mode::Agent)));
    }

    #[test]
    fn activate_hallucination() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Modes.index();
        s.move_down();
        assert_eq!(s.activate(), Some(MenuAction::SetMode(Mode::Hallucination)));
    }

    #[test]
    fn learn_inserts_comma() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Learn.index();
        s.item_idx = 0;
        assert_eq!(s.activate(), Some(MenuAction::InsertLine(", ".into())));
    }

    #[test]
    fn categories_are_alphabetical() {
        assert_eq!(Category::Scripts.label(), "Scripts");
        assert!(Category::ALL.contains(&Category::Editor));
        assert_eq!(Category::Editor.label(), "Editor");
        assert_eq!(Category::ALL.len(), 11);
        assert_eq!(Category::Theme.label(), "Theme");
        // First ten stay alphabetical; Theme is appended (no digit jump).
        let labels: Vec<_> = Category::ALL[..10]
            .iter()
            .map(|c| c.label().to_ascii_lowercase())
            .collect();
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted);
        assert_eq!(Category::ALL[10], Category::Theme);
        assert_eq!(digit_category_index('1'), Some(0));
        assert_eq!(digit_category_index('0'), Some(9));
        assert!(digit_category_index('0').unwrap() < Category::Theme.index());
    }

    #[test]
    fn theme_preview_avatar_follows_highlight() {
        chrome::set_active_avatar(Avatar::Navi);
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Theme.index();
        s.focus = Focus::Items;
        let items = s.items();
        // Highlight laptop avatar row.
        s.item_idx = items
            .iter()
            .position(|i| matches!(i.kind, ItemKind::SetAvatar(Avatar::Laptop)))
            .unwrap();
        assert_eq!(theme_preview_avatar(&s, &items), Avatar::Laptop);
        // Color row keeps the active avatar for the preview pane.
        s.item_idx = items
            .iter()
            .position(|i| matches!(i.kind, ItemKind::SetTheme(Theme::Coffee)))
            .unwrap();
        assert_eq!(theme_preview_avatar(&s, &items), Avatar::Navi);
    }

    #[test]
    fn theme_category_lists_palettes_and_avatars() {
        let _g = chrome::theme_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("shomsky-menu-theme-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("ENGRAM_USER_DIR", &dir);
        std::env::remove_var("ENGRAM_THEME");
        std::env::remove_var("ENGRAM_AVATAR");
        chrome::set_active(Theme::Harbor);
        chrome::set_active_avatar(Avatar::Navi);
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Theme.index();
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert!(labels.iter().any(|l| l.contains("color")));
        assert!(labels.iter().any(|l| l.contains("avatar")));
        assert!(labels.iter().any(|l| l.contains("harbor")));
        assert!(labels.iter().any(|l| l.contains("frutiger-aero-green")));
        assert!(labels.iter().any(|l| l.contains("hacker-amber")));
        assert!(labels.iter().any(|l| l.contains("navi")));
        assert!(labels.iter().any(|l| l.contains("coffee mug")));
        assert!(labels.iter().any(|l| l.contains("mana flame")));
        s.enter_theme_category();
        s.focus = Focus::Items;
        let items = s.items();
        s.item_idx = items
            .iter()
            .position(|i| matches!(i.kind, ItemKind::SetTheme(Theme::Coffee)))
            .unwrap();
        // Highlight previews without committing (● stays on harbor).
        s.preview_theme_selection();
        assert_eq!(chrome::active(), Theme::Coffee);
        assert!(s
            .items()
            .iter()
            .any(|i| i.label.contains("harbor") && i.label.contains('●')));
        assert!(!s
            .items()
            .iter()
            .any(|i| i.label.contains("coffee") && i.label.contains('●')));
        // Space/Enter commits and stays in the menu.
        assert_eq!(s.activate(), None);
        assert_eq!(chrome::active(), Theme::Coffee);
        assert!(s
            .items()
            .iter()
            .any(|i| i.label.contains("coffee") && i.label.contains('●')));
        let items = s.items();
        s.item_idx = items
            .iter()
            .position(|i| matches!(i.kind, ItemKind::SetAvatar(Avatar::Laptop)))
            .unwrap();
        assert_eq!(s.activate(), None);
        assert_eq!(chrome::active_avatar(), Avatar::Laptop);
        // Color stayed coffee — avatar is independent.
        assert_eq!(chrome::active(), Theme::Coffee);
        // Preview another color then Esc restores the committed coffee.
        s.item_idx = s
            .items()
            .iter()
            .position(|i| matches!(i.kind, ItemKind::SetTheme(Theme::Purple)))
            .unwrap();
        s.preview_theme_selection();
        assert_eq!(chrome::active(), Theme::Purple);
        assert_eq!(s.escape(), None); // back to categories, restores coffee
        assert_eq!(chrome::active(), Theme::Coffee);
        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var("ENGRAM_USER_DIR");
    }

    #[test]
    fn editor_items_open_and_recent() {
        let mut c = ctx();
        c.edit_history = vec![crate::edit_history::EditVisit {
            path: "/engram/user/fun.py".into(),
            visited_at: 1,
        }];
        let mut s = MenuState::new(&c);
        s.category_idx = Category::Editor.index();
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels[0], "open editor");
        assert_eq!(labels[1], "clear history");
        assert_eq!(labels[2], "/engram/user/fun.py");
        s.item_idx = 0;
        assert_eq!(s.activate(), Some(MenuAction::RunMeta(":edit".into())));
        s.item_idx = 2;
        assert_eq!(
            s.activate(),
            Some(MenuAction::RunMeta(":edit /engram/user/fun.py".into()))
        );
        s.item_idx = 1;
        assert!(s.activate().is_none());
        assert!(s.edit_clear_confirm);
    }

    #[test]
    fn shomagent_category_is_listed() {
        assert!(Category::ALL.contains(&Category::Shomagent));
        assert_eq!(Category::Shomagent.label(), "shomagent");
        assert!(Category::ALL.contains(&Category::Shomloop));
        assert_eq!(Category::Shomloop.label(), "shomloop");
        let s = MenuState::new(&ctx());
        // Jump to category and confirm root actions exist.
        let mut s = s;
        s.category_idx = Category::Shomagent.index();
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(
            labels,
            vec![
                "list agents".to_string(),
                "readwrite <file>".to_string(),
                "web <url>".to_string(),
                "ask <opened file>".to_string(),
                "talk <opened file>".to_string(),
                "dream seed <file>".to_string(),
                "refine idea <file>".to_string(),
                "stop / end <agent>".to_string(),
            ]
        );
    }

    #[test]
    fn shomagent_list_agents_runs_meta() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 0;
        assert_eq!(s.activate(), Some(MenuAction::RunMeta(":shomagent".into())));
    }

    #[test]
    fn shomloop_new_loop_prefills() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomloop.index();
        s.item_idx = 0;
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels[0], "new loop");
        assert_eq!(labels[1], "logs");
        assert_eq!(
            s.activate(),
            Some(MenuAction::InsertLine("shomloop ".into()))
        );
    }

    #[test]
    fn shomloop_overlay_actions() {
        let mut s = MenuState::new(&ctx());
        s.loop_pick = Some(LoopPick {
            id: 3,
            enabled: true,
            frequency: "daily at 5pm".into(),
        });
        assert_eq!(
            s.confirm_loop(LoopVerb::Edit),
            Some(MenuAction::InsertLine(
                "shomloop edit 3 \"daily at 5pm\" ".into()
            ))
        );
        s.loop_pick = Some(LoopPick {
            id: 3,
            enabled: true,
            frequency: "hourly".into(),
        });
        assert_eq!(
            s.confirm_loop(LoopVerb::Disable),
            Some(MenuAction::InsertLine("shomloop disable 3".into()))
        );
        s.loop_pick = Some(LoopPick {
            id: 3,
            enabled: false,
            frequency: "hourly".into(),
        });
        assert_eq!(
            s.confirm_loop(LoopVerb::Enable),
            Some(MenuAction::InsertLine("shomloop enable 3".into()))
        );
        s.loop_pick = Some(LoopPick {
            id: 3,
            enabled: true,
            frequency: "hourly".into(),
        });
        assert_eq!(
            s.confirm_loop(LoopVerb::Delete),
            Some(MenuAction::InsertLine("shomloop delete 3".into()))
        );
        s.loop_pick = Some(LoopPick {
            id: 3,
            enabled: true,
            frequency: "hourly".into(),
        });
        assert_eq!(
            s.confirm_loop(LoopVerb::Logs),
            Some(MenuAction::InsertLine("shomloop logs 3".into()))
        );
        s.loop_pick = Some(LoopPick {
            id: 3,
            enabled: true,
            frequency: "say \"hi\"".into(),
        });
        assert_eq!(
            s.confirm_loop(LoopVerb::Edit),
            Some(MenuAction::InsertLine(
                "shomloop edit 3 \"say \\\"hi\\\"\" ".into()
            ))
        );
    }

    #[test]
    fn shomagent_readwrite_prompts_for_filename() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 1; // readwrite <file>
        assert_eq!(s.activate(), None);
        assert!(matches!(
            s.shomagent_sub,
            Some(ShomagentSub::PromptFile {
                kind: FilePromptKind::ReadWrite,
                ..
            })
        ));
        for c in "notes.md".chars() {
            s.filename_push(c);
        }
        assert_eq!(
            s.confirm_filename(),
            Some(MenuAction::InsertLine(
                "shomagent readwrite notes.md".into()
            ))
        );
        assert!(s.shomagent_sub.is_none());
    }

    #[test]
    fn shomagent_dream_seed_prompts_for_filename() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 5; // dream seed <file>
        assert_eq!(s.activate(), None);
        for c in "ideas.md".chars() {
            s.filename_push(c);
        }
        s.filename_backspace(); // ideas.m
        s.filename_push('d');
        assert_eq!(
            s.confirm_filename(),
            Some(MenuAction::InsertLine(
                "shomagent dream seed ideas.md".into()
            ))
        );
    }

    #[test]
    fn shomagent_dream_seed_empty_filename_stays_open() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 5;
        assert_eq!(s.activate(), None);
        assert_eq!(s.confirm_filename(), None);
        assert!(matches!(
            s.shomagent_sub,
            Some(ShomagentSub::PromptFile {
                kind: FilePromptKind::DreamSeed,
                ..
            })
        ));
    }

    #[test]
    fn shomagent_refine_idea_prompts_for_filename() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 6; // refine idea <file>
        assert_eq!(s.activate(), None);
        for c in "idea.md".chars() {
            s.filename_push(c);
        }
        assert_eq!(
            s.confirm_filename(),
            Some(MenuAction::InsertLine(
                "shomagent refine idea idea.md".into()
            ))
        );
    }

    #[test]
    fn shomagent_ask_picks_opened_document() {
        let mut s = MenuState::new(&ctx_with_docs(vec![
            ShomagentMemberBrief {
                id: 2,
                name: "notes.md".into(),
                state: "readwrite".into(),
                path: "/engram/user/notes.md".into(),
            },
            ShomagentMemberBrief {
                id: 9,
                name: "watcher".into(),
                state: "running".into(),
                path: "npm run dev".into(),
            },
        ]));
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 3; // ask
        assert_eq!(s.activate(), None);
        assert!(matches!(
            s.shomagent_sub,
            Some(ShomagentSub::PickMember {
                kind: MemberVerb::Ask
            })
        ));
        // Process members are filtered out — only the document remains.
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels.len(), 1);
        assert!(labels[0].contains("#2"));
        s.item_idx = 0;
        assert_eq!(
            s.activate(),
            Some(MenuAction::InsertLine("shomagent ask 2 ".into()))
        );
    }

    #[test]
    fn shomagent_talk_picks_opened_document() {
        let mut s = MenuState::new(&ctx_with_docs(vec![ShomagentMemberBrief {
            id: 3,
            name: "doc".into(),
            state: "read".into(),
            path: "doc.md".into(),
        }]));
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 4; // talk
        assert_eq!(s.activate(), None);
        s.item_idx = 0;
        assert_eq!(
            s.activate(),
            Some(MenuAction::InsertLine("shomagent talk 3".into()))
        );
    }

    #[test]
    fn shomagent_ask_empty_roster_is_info_only() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 3;
        assert_eq!(s.activate(), None);
        assert_eq!(s.items()[0].label, "(none)");
        s.item_idx = 0;
        assert_eq!(s.activate(), None);
    }

    #[test]
    fn shomagent_escape_leaves_member_picker() {
        let mut s = MenuState::new(&ctx_with_docs(vec![ShomagentMemberBrief {
            id: 1,
            name: "a.md".into(),
            state: "readwrite".into(),
            path: "a.md".into(),
        }]));
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 3;
        assert_eq!(s.activate(), None);
        assert!(s.shomagent_sub.is_some());
        assert_eq!(s.escape(), None);
        assert!(s.shomagent_sub.is_none());
        assert_eq!(s.items()[0].label, "list agents");
    }

    #[test]
    fn shomagent_stop_picks_any_member() {
        let mut s = MenuState::new(&ctx_with_docs(vec![
            ShomagentMemberBrief {
                id: 1,
                name: "notes.md".into(),
                state: "readwrite".into(),
                path: "notes.md".into(),
            },
            ShomagentMemberBrief {
                id: 4,
                name: "dev".into(),
                state: "running".into(),
                path: "npm run dev".into(),
            },
        ]));
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 7; // stop / end
        assert_eq!(s.activate(), None);
        assert!(matches!(
            s.shomagent_sub,
            Some(ShomagentSub::PickMember {
                kind: MemberVerb::Stop
            })
        ));
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels.len(), 2);
        assert!(labels.iter().any(|l| l.contains("#1")));
        assert!(labels.iter().any(|l| l.contains("#4")));
        // Stop the document member.
        s.item_idx = 0;
        assert_eq!(
            s.activate(),
            Some(MenuAction::InsertLine("shomagent stop 1".into()))
        );
    }

    #[test]
    fn shomagent_stop_empty_roster_is_info_only() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 7;
        assert_eq!(s.activate(), None);
        assert_eq!(s.items()[0].label, "(none)");
        assert!(s.items()[0].detail.contains("nothing to stop"));
        s.item_idx = 0;
        assert_eq!(s.activate(), None);
    }

    #[test]
    fn help_runs_help() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Help.index();
        s.item_idx = 0;
        assert_eq!(s.activate(), Some(MenuAction::RunMeta(":help".into())));
    }

    #[test]
    fn exit_requires_confirm() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::System.index();
        // last item is exit
        s.item_idx = s.items().len() - 1;
        assert_eq!(s.activate(), None);
        assert!(s.exit_confirm);
        assert_eq!(s.activate(), Some(MenuAction::ExitShell));
    }

    #[test]
    fn escape_hierarchy() {
        let mut s = MenuState::new(&ctx());
        s.focus = Focus::Items;
        assert_eq!(s.escape(), None);
        assert_eq!(s.focus, Focus::Categories);
        assert_eq!(s.escape(), Some(MenuAction::Dismiss));
    }

    #[test]
    fn root_hidden_when_disallowed() {
        let mut c = ctx();
        c.allow_root_mode = false;
        let s = MenuState::new(&c);
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert!(!labels.iter().any(|l| l.contains("root")));
    }

    #[test]
    fn pending_learn_enables_save() {
        let mut c = ctx();
        c.pending_learn = true;
        let mut s = MenuState::new(&c);
        s.category_idx = Category::Learn.index();
        s.item_idx = 2;
        assert_eq!(s.activate(), Some(MenuAction::InsertLine("s".into())));
    }

    #[test]
    fn packages_category_lists_empty() {
        let dir = std::env::temp_dir().join(format!("engram-menu-pkgs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(".packages.json");
        let mut c = ctx();
        c.packages_path = path;
        let mut s = MenuState::new(&c);
        s.category_idx = Category::Packages.index();
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert!(labels.iter().any(|l| l == "list all"));
        assert!(labels.iter().any(|l| l == "(none)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn packages_category_confirms_uninstall() {
        let dir = std::env::temp_dir().join(format!("engram-menu-pkgs2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(".packages.json");
        crate::packages::record_from_command(&path, "apk add neovim");
        let mut c = ctx();
        c.packages_path = path;
        let mut s = MenuState::new(&c);
        s.category_idx = Category::Packages.index();
        // item 0 = list all; item 1 = neovim
        s.item_idx = 1;
        assert_eq!(s.activate(), None);
        assert!(s.package_pick.is_some());
        assert_eq!(
            s.confirm_package(),
            Some(MenuAction::InsertLine("apk del neovim".into()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn packages_category_confirms_reinstall() {
        let dir = std::env::temp_dir().join(format!("engram-menu-pkgs3-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(".packages.json");
        crate::packages::record_from_command(&path, "apk add htop");
        crate::packages::record_from_command(&path, "apk del htop");
        let mut c = ctx();
        c.packages_path = path;
        let mut s = MenuState::new(&c);
        s.category_idx = Category::Packages.index();
        s.item_idx = 1;
        assert_eq!(s.activate(), None);
        assert_eq!(
            s.confirm_package(),
            Some(MenuAction::InsertLine("apk add htop".into()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn web_category_lists_history_and_clears() {
        let dir = std::env::temp_dir().join(format!("menu-web-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(".web_history.json");
        crate::web_history::record(&path, "https://news.ycombinator.com", "HN").unwrap();
        let mut c = ctx();
        c.web_history_path = path.clone();
        c.web_history = crate::web_history::load(&path);
        let mut s = MenuState::new(&c);
        s.category_idx = Category::Web.index();
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert_eq!(labels[0], "open browser");
        assert_eq!(labels[1], "clear history");
        assert!(labels[2].contains("news.ycombinator.com"));
        assert_eq!(s.activate(), Some(MenuAction::RunMeta(":web".into())));
        s.item_idx = 2;
        assert_eq!(
            s.activate(),
            Some(MenuAction::RunMeta(
                ":web https://news.ycombinator.com".into()
            ))
        );
        s.item_idx = 1;
        assert_eq!(s.activate(), None);
        assert!(s.web_clear_confirm);
        s.confirm_web_clear();
        assert!(s.web_history.is_empty());
        assert!(crate::web_history::load(&path).is_empty());
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        assert!(labels.iter().any(|l| l.contains("no sites yet")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sav_cmd_first_use_is_empty() {
        let _guard = crate::modes::engram_env_lock();
        let home = std::env::temp_dir().join(format!("shomsky-menu-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let prev = std::env::var_os("ENGRAM_USER_DIR");
        std::env::set_var("ENGRAM_USER_DIR", &home);
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Scripts.index();
        s.item_idx = 0;
        let labels: Vec<_> = s.items().iter().map(|i| i.label.clone()).collect();
        let action = s.activate();
        match prev {
            Some(v) => std::env::set_var("ENGRAM_USER_DIR", v),
            None => std::env::remove_var("ENGRAM_USER_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(labels, vec!["(none)".to_string()]);
        assert_eq!(action, None);
    }

    #[test]
    fn sav_cmd_runs_saved_command() {
        let _guard = crate::modes::engram_env_lock();
        let home = std::env::temp_dir().join(format!("shomsky-menu-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let paths = shomsky::LearnPaths::new(&home);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.command_path("say-hi"), "#!/bin/sh\necho hi\n").unwrap();
        shomsky::save_entry(
            &paths,
            &shomsky::LearnEntry {
                name: "say-hi".into(),
                description: "say hi".into(),
                version: "0.1.0".into(),
                intent: "say hi".into(),
                command: "echo hi".into(),
                flags: vec![],
                script_path: paths.command_path("say-hi").display().to_string(),
                man_path: paths.man_path("say-hi").display().to_string(),
                bats_path: paths.bats_path("say-hi").display().to_string(),
                completion_path: String::new(),
            },
        )
        .unwrap();
        let prev = std::env::var_os("ENGRAM_USER_DIR");
        std::env::set_var("ENGRAM_USER_DIR", &home);
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Scripts.index();
        s.item_idx = 0;
        assert_eq!(s.activate(), None);
        let action = s.confirm_saved(false);
        match prev {
            Some(v) => std::env::set_var("ENGRAM_USER_DIR", v),
            None => std::env::remove_var("ENGRAM_USER_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(action, Some(MenuAction::InsertLine("say-hi".into())));
    }

    #[test]
    fn sav_cmd_delete_removes_command() {
        let _guard = crate::modes::engram_env_lock();
        let home = std::env::temp_dir().join(format!("shomsky-menu-del-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let paths = shomsky::LearnPaths::new(&home);
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.command_path("oops"), "#!/bin/sh\n").unwrap();
        shomsky::save_entry(
            &paths,
            &shomsky::LearnEntry {
                name: "oops".into(),
                description: "accidental".into(),
                version: "0.1.0".into(),
                intent: "oops".into(),
                command: "true".into(),
                flags: vec![],
                script_path: paths.command_path("oops").display().to_string(),
                man_path: paths.man_path("oops").display().to_string(),
                bats_path: paths.bats_path("oops").display().to_string(),
                completion_path: String::new(),
            },
        )
        .unwrap();
        let prev = std::env::var_os("ENGRAM_USER_DIR");
        std::env::set_var("ENGRAM_USER_DIR", &home);
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Scripts.index();
        s.item_idx = 0;
        assert_eq!(s.activate(), None);
        assert_eq!(s.confirm_saved(true), None);
        assert!(!paths.command_path("oops").exists());
        assert!(!paths.registry_path("oops").exists());
        assert_eq!(s.items()[0].label, "(none)");
        match prev {
            Some(v) => std::env::set_var("ENGRAM_USER_DIR", v),
            None => std::env::remove_var("ENGRAM_USER_DIR"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn navigation_wraps_categories() {
        let mut s = MenuState::new(&ctx());
        s.focus = Focus::Categories;
        s.category_idx = 0;
        s.move_up();
        assert_eq!(s.category_idx, Category::ALL.len() - 1);
        s.move_down();
        assert_eq!(s.category_idx, 0);
    }
}
