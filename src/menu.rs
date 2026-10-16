//! Slip-in / slip-out feature navigation hub (ANSI + BusyBox stty).
//!
//! Opened from the REPL via `:menu` / `:m`. Selecting an item either runs a
//! meta-command / mode switch or returns a starter line to prefill.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

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
    /// `read` / `readwrite` / `dream` / process states.
    pub state: String,
    /// Document path for doc members; command line for process members.
    pub path: String,
}

impl ShomagentMemberBrief {
    pub fn is_document(&self) -> bool {
        matches!(
            self.state.as_str(),
            "read" | "readwrite" | "dream"
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
    /// Live roster snapshot (may be empty if daemon is down).
    pub shomagent_members: Vec<ShomagentMemberBrief>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Modes,
    Learn,
    SavCmd,
    Packages,
    Shomagent,
    Help,
    System,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Modes,
        Category::Learn,
        Category::SavCmd,
        Category::Packages,
        Category::Shomagent,
        Category::Help,
        Category::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Modes => "Modes",
            Category::Learn => "Learn",
            Category::SavCmd => "sav-cmd",
            Category::Packages => "Packages",
            Category::Shomagent => "shomagent",
            Category::Help => "Help",
            Category::System => "System",
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
    PromptFile {
        kind: FilePromptKind,
        buf: String,
    },
    PickMember {
        kind: MemberVerb,
    },
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
    allow_root_mode: bool,
    mode: Mode,
    pending_learn: bool,
    packages_path: PathBuf,
    shomagent_members: Vec<ShomagentMemberBrief>,
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
    TrackedPackage { name: String, status: PackageStatus },
    /// Prompt for a filename, then insert `shomagent … <file>`.
    ShomagentPromptFile(FilePromptKind),
    /// Enter member-picker submenu for ask/talk.
    ShomagentPickMember(MemberVerb),
    /// A concrete opened document chosen for ask/talk.
    ShomagentMember { id: u32, verb: MemberVerb },
    ExitShell,
    InfoOnly,
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
            allow_root_mode: ctx.allow_root_mode,
            mode: ctx.mode,
            pending_learn: ctx.pending_learn,
            packages_path: ctx.packages_path.clone(),
            shomagent_members: ctx.shomagent_members.clone(),
        }
    }

    pub fn category(&self) -> Category {
        Category::from_index(self.category_idx)
    }

    fn in_overlay(&self) -> bool {
        self.exit_confirm
            || self.saved_pick.is_some()
            || self.package_pick.is_some()
            || matches!(
                self.shomagent_sub,
                Some(ShomagentSub::PromptFile { .. })
            )
    }

    fn items(&self) -> Vec<Item> {
        if let Some(ShomagentSub::PickMember { kind }) = self.shomagent_sub {
            return member_pick_items(&self.shomagent_members, kind);
        }
        items_for(
            self.category(),
            self.mode,
            self.allow_root_mode,
            self.pending_learn,
            &self.packages_path,
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
        match focus {
            Focus::Categories => {
                if self.category_idx == 0 {
                    self.category_idx = Category::ALL.len() - 1;
                } else {
                    self.category_idx -= 1;
                }
                self.item_idx = 0;
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
        match focus {
            Focus::Categories => {
                self.category_idx = (self.category_idx + 1) % Category::ALL.len();
                self.item_idx = 0;
            }
            Focus::Items => {
                let n = self.items().len();
                if n == 0 {
                    return;
                }
                self.item_idx = (self.item_idx + 1) % n;
            }
        }
    }

    pub fn focus_left(&mut self) {
        if self.exit_confirm {
            self.exit_confirm = false;
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
    }

    pub fn toggle_pane(&mut self) {
        if self.exit_confirm {
            self.exit_confirm = false;
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
        if self.saved_pick.is_some() {
            self.saved_pick = None;
            return None;
        }
        if self.package_pick.is_some() {
            self.package_pick = None;
            return None;
        }
        if self.shomagent_sub.is_some() {
            self.clear_shomagent_sub();
            return None;
        }
        if self.focus == Focus::Items {
            self.focus = Focus::Categories;
            return None;
        }
        Some(MenuAction::Dismiss)
    }

    /// Enter / confirm on the current selection.
    pub fn activate(&mut self) -> Option<MenuAction> {
        if self.exit_confirm {
            return Some(MenuAction::ExitShell);
        }
        if self.saved_pick.is_some() {
            return self.confirm_saved(false);
        }
        if self.package_pick.is_some() {
            return self.confirm_package();
        }
        if matches!(self.shomagent_sub, Some(ShomagentSub::PromptFile { .. })) {
            return self.confirm_filename();
        }
        if self.focus == Focus::Categories {
            self.focus = Focus::Items;
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
            self.shomagent_sub = Some(ShomagentSub::PromptFile {
                kind,
                buf,
            });
            return None;
        }
        let line = match kind {
            FilePromptKind::ReadWrite => format!("shomagent readwrite {path}"),
            FilePromptKind::DreamSeed => format!("shomagent dream seed {path}"),
        };
        Some(MenuAction::InsertLine(line))
    }

    /// Double-Esc while open: always quit the menu.
    pub fn force_dismiss(&self) -> MenuAction {
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
}

fn items_for(
    cat: Category,
    mode: Mode,
    allow_root: bool,
    pending_learn: bool,
    packages_path: &PathBuf,
) -> Vec<Item> {
    match cat {
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
        Category::SavCmd => saved_command_items(),
        Category::Packages => package_items(packages_path),
        Category::Shomagent => shomagent_root_items(),
        Category::Help => vec![
            Item {
                label: "full help".into(),
                detail: "Show :help text".into(),
                kind: ItemKind::RunMeta(":help"),
            },
            Item {
                label: "this menu".into(),
                detail: ":menu / :m — modes, learn, packages, agents, system".into(),
                kind: ItemKind::InfoOnly,
            },
        ],
        Category::System => vec![
            Item {
                label: ":sys".into(),
                detail: "Identity (mode, model, tools, llm)".into(),
                kind: ItemKind::RunMeta(":sys"),
            },
            Item {
                label: ":splash".into(),
                detail: "Replay the startup splash art".into(),
                kind: ItemKind::RunMeta(":splash"),
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
            label: "stop / end <agent>".into(),
            detail: "Stop a watcher or detach a document (file kept)".into(),
            kind: ItemKind::ShomagentPickMember(MemberVerb::Stop),
        },
    ]
}

fn member_pick_items(members: &[ShomagentMemberBrief], verb: MemberVerb) -> Vec<Item> {
    let pool: Vec<_> = match verb {
        MemberVerb::Ask | MemberVerb::Talk => {
            members.iter().filter(|m| m.is_document()).collect()
        }
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
                kind: ItemKind::ShomagentMember {
                    id: m.id,
                    verb,
                },
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
        let typing_filename =
            matches!(state.shomagent_sub, Some(ShomagentSub::PromptFile { .. }));
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
            Key::Enter => {
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
                return MenuAction::Dismiss;
            }
            Key::Char('y') | Key::Char('Y') if state.exit_confirm => {
                return MenuAction::ExitShell;
            }
            Key::Char('n') | Key::Char('N') if state.exit_confirm => {
                state.exit_confirm = false;
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
            Key::Char('1') if state.shomagent_sub.is_none() => {
                esc_armed = false;
                state.category_idx = 0;
                state.item_idx = 0;
                state.focus = Focus::Items;
            }
            Key::Char('2') if state.shomagent_sub.is_none() => {
                esc_armed = false;
                state.category_idx = 1;
                state.item_idx = 0;
                state.focus = Focus::Items;
            }
            Key::Char('3') if state.shomagent_sub.is_none() => {
                esc_armed = false;
                state.category_idx = 2;
                state.item_idx = 0;
                state.focus = Focus::Items;
            }
            Key::Char('4') if state.shomagent_sub.is_none() => {
                esc_armed = false;
                state.category_idx = 3;
                state.item_idx = 0;
                state.focus = Focus::Items;
            }
            Key::Char('5') if state.shomagent_sub.is_none() => {
                esc_armed = false;
                state.category_idx = 4;
                state.item_idx = 0;
                state.focus = Focus::Items;
            }
            Key::Char('6') if state.shomagent_sub.is_none() => {
                esc_armed = false;
                state.category_idx = 5;
                state.item_idx = 0;
                state.focus = Focus::Items;
            }
            Key::Char('7') if state.shomagent_sub.is_none() => {
                esc_armed = false;
                state.category_idx = 6;
                state.item_idx = 0;
                state.focus = Focus::Items;
            }
            _ => {
                esc_armed = false;
            }
        }
    }
}

fn draw(state: &MenuState) {
    let (rows, cols) = term::term_size();
    let width = cols.clamp(40, 72).min(cols);
    let height = 16usize.min(rows.saturating_sub(2)).max(10);
    let left = ((cols.saturating_sub(width)) / 2).max(1);
    let top = ((rows.saturating_sub(height)) / 2).max(1);

    let color = ui::color_enabled();
    let bold = if color { "\x1b[1m" } else { "" };
    let dim = if color { "\x1b[2m" } else { "" };
    let inv = if color { "\x1b[7m" } else { "" };
    let cyan = if color { "\x1b[36m" } else { "" };
    let r = ui::reset();

    let items = state.items();
    let cat_w = 14usize;
    let item_w = width.saturating_sub(cat_w + 3);

    let mut buf = String::with_capacity(width * height * 4);
    use std::fmt::Write as _;

    let _ = write!(buf, "\x1b[2J\x1b[H");

    // Title
    let title = " Shomsky menu ";
    let _ = write!(
        buf,
        "\x1b[{};{}H{cyan}┌{bold}{title}{r}{cyan}{line}┐{r}",
        top,
        left,
        line = "─".repeat(width.saturating_sub(title.chars().count() + 1))
    );

    let body_rows = height.saturating_sub(4);
    for row in 0..body_rows {
        let y = top + 1 + row;
        let _ = write!(buf, "\x1b[{y};{left}H{cyan}│{r}");

        // Category column
        let cat_text = if row < Category::ALL.len() {
            let c = Category::from_index(row);
            let selected = row == state.category_idx;
            let focused = state.focus == Focus::Categories && selected;
            let label = format!(" {}", c.label());
            let padded = pad_visible(&label, cat_w);
            if focused {
                format!("{inv}{padded}{r}")
            } else if selected {
                format!("{bold}{padded}{r}")
            } else {
                format!("{dim}{padded}{r}")
            }
        } else {
            " ".repeat(cat_w)
        };
        buf.push_str(&cat_text);
        let _ = write!(buf, "{cyan}│{r}");

        // Item column. Scroll so a long sav-cmd list keeps the selection visible.
        let item_offset = state.item_idx.saturating_sub(body_rows.saturating_sub(1));
        let item_text = if let Some(item) = items.get(row + item_offset) {
            let selected = row + item_offset == state.item_idx;
            let focused = state.focus == Focus::Items && selected;
            let label = format!(" {}", item.label);
            let padded = pad_visible(&label, item_w);
            if focused {
                format!("{inv}{padded}{r}")
            } else if selected {
                format!("{bold}{padded}{r}")
            } else {
                padded
            }
        } else {
            " ".repeat(item_w)
        };
        buf.push_str(&item_text);
        let _ = write!(buf, "{cyan}│{r}");
    }

    // Detail + separator
    let detail_y = top + 1 + body_rows;
    let detail = if state.exit_confirm {
        "Exit the shell? [y]es / [n]o / Esc".to_string()
    } else if let Some(name) = &state.saved_pick {
        format!("`{name}`  [r]un / [d]elete / Esc")
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
            FilePromptKind::ReadWrite => "readwrite",
            FilePromptKind::DreamSeed => "dream seed",
        };
        format!("{tag} file: {buf}█  Enter · Esc")
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
    };
    let _ = write!(
        buf,
        "\x1b[{detail_y};{left}H{cyan}├{}┤{r}",
        "─".repeat(width.saturating_sub(2))
    );
    let detail_y2 = detail_y + 1;
    let _ = write!(
        buf,
        "\x1b[{detail_y2};{left}H{cyan}│{r}{dim} {}{r}{cyan}│{r}",
        pad_visible(&detail, width.saturating_sub(3))
    );

    // Footer
    let foot_y = detail_y2 + 1;
    let footer = " ↑↓ · Tab · Enter · Esc back · Esc Esc quit ";
    let _ = write!(
        buf,
        "\x1b[{foot_y};{left}H{cyan}└{dim}{}{r}{cyan}┘{r}",
        pad_visible(footer, width.saturating_sub(2))
    );

    let mut out = io::stdout().lock();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

fn pad_visible(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for ch in s.chars() {
        if n >= width {
            break;
        }
        out.push(ch);
        n += 1;
    }
    while n < width {
        out.push(' ');
        n += 1;
    }
    out
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
        // Modes category, first item = agent
        assert_eq!(s.activate(), Some(MenuAction::SetMode(Mode::Agent)));
    }

    #[test]
    fn activate_hallucination() {
        let mut s = MenuState::new(&ctx());
        s.move_down();
        assert_eq!(
            s.activate(),
            Some(MenuAction::SetMode(Mode::Hallucination))
        );
    }

    #[test]
    fn learn_inserts_comma() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Learn.index();
        s.item_idx = 0;
        assert_eq!(s.activate(), Some(MenuAction::InsertLine(", ".into())));
    }

    #[test]
    fn shomagent_category_is_listed() {
        assert!(Category::ALL.contains(&Category::Shomagent));
        assert_eq!(Category::Shomagent.label(), "shomagent");
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
                "ask <opened file>".to_string(),
                "talk <opened file>".to_string(),
                "dream seed <file>".to_string(),
                "stop / end <agent>".to_string(),
            ]
        );
    }

    #[test]
    fn shomagent_list_agents_runs_meta() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 0;
        assert_eq!(
            s.activate(),
            Some(MenuAction::RunMeta(":shomagent".into()))
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
            Some(MenuAction::InsertLine("shomagent readwrite notes.md".into()))
        );
        assert!(s.shomagent_sub.is_none());
    }

    #[test]
    fn shomagent_dream_seed_prompts_for_filename() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 4; // dream seed <file>
        assert_eq!(s.activate(), None);
        for c in "ideas.md".chars() {
            s.filename_push(c);
        }
        s.filename_backspace(); // ideas.m
        s.filename_push('d');
        assert_eq!(
            s.confirm_filename(),
            Some(MenuAction::InsertLine("shomagent dream seed ideas.md".into()))
        );
    }

    #[test]
    fn shomagent_dream_seed_empty_filename_stays_open() {
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::Shomagent.index();
        s.item_idx = 4;
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
        s.item_idx = 2; // ask
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
        s.item_idx = 3; // talk
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
        s.item_idx = 2;
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
        s.item_idx = 2;
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
        s.item_idx = 5; // stop / end
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
        s.item_idx = 5;
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
        let dir = std::env::temp_dir().join(format!(
            "engram-menu-pkgs-{}",
            std::process::id()
        ));
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
        let dir = std::env::temp_dir().join(format!(
            "engram-menu-pkgs2-{}",
            std::process::id()
        ));
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
        let dir = std::env::temp_dir().join(format!(
            "engram-menu-pkgs3-{}",
            std::process::id()
        ));
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
    fn sav_cmd_first_use_is_empty() {
        let _guard = crate::modes::engram_env_lock();
        let home = std::env::temp_dir().join(format!(
            "shomsky-menu-empty-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let prev = std::env::var_os("ENGRAM_USER_DIR");
        std::env::set_var("ENGRAM_USER_DIR", &home);
        let mut s = MenuState::new(&ctx());
        s.category_idx = Category::SavCmd.index();
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
        let home = std::env::temp_dir().join(format!(
            "shomsky-menu-{}",
            std::process::id()
        ));
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
        s.category_idx = Category::SavCmd.index();
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
        let home = std::env::temp_dir().join(format!(
            "shomsky-menu-del-{}",
            std::process::id()
        ));
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
        s.category_idx = Category::SavCmd.index();
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
