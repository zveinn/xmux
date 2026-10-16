//! Config, loaded from `$HOME/.config/xmux/config.yaml`:
//!
//! ```yaml
//! start_dir: /home/you/code
//! commands:
//!   alt+h: htop
//!   alt+g: lazygit
//! keybindings:
//!   session-manager: ctrl+o
//!   next-finished: ctrl+a
//!   split-vertical: alt+v
//! sessions:
//!   1: { name: meow, key: F1 }
//!   5: { name: scratch, key: F5 }
//! ```
//!
//! `start_dir` is where new shells start (unset = your home directory);
//! `commands` type a program into the shell; `keybindings` rebind the
//! server's own controls (any action not listed keeps its default);
//! `sessions` pin sessions to slots in the session list — the slot
//! number orders them (gaps collapse in the display) and the key opens
//! the session from anywhere, starting it if it isn't running.
//! Keys are `[ctrl+][alt+]<char>` or `F1`-`F12`; ctrl+letter becomes the
//! matching C0 control byte (0x01-0x1a), an alt prefix a leading ESC,
//! and F-keys their escape sequences.

use std::collections::HashMap;

use crossterm::style::Color;

use crate::input::{InputAction, Overlay};
use crate::model::NavDir;

/// What a bound key does.
pub enum BindingAction {
    /// Type this program into the shell and press Enter.
    Run(String),
    /// One of the server's own controls.
    Control(InputAction),
}

pub struct Binding {
    /// Byte sequence the client's terminal sends for this chord.
    pub seq: Vec<u8>,
    pub action: BindingAction,
}

/// A session pinned to a slot in the session list. Pins are kept in
/// slot order; the slot numbers themselves only order the list (gaps
/// collapse in the display).
pub struct Pin {
    pub name: String,
}

pub struct Config {
    /// All key bindings (controls, command bindings, session keys), longest
    /// sequence first.
    pub bindings: Vec<Binding>,
    /// Pinned sessions in slot order.
    pub pins: Vec<Pin>,
    /// Focused-pane dividers, manager selectors, and the attached mark
    /// in `xmux list` (`accent: "#7aa2f7"`). Defaults to the terminal
    /// palette's cyan so it follows the theme.
    pub accent: Color,
    /// Fill of the open session and open tab chips
    /// (`active_background: "#7aa2f7"`). The brighter of this and
    /// `active_foreground` names the open entry in the session and tab
    /// managers. Absent or blank uses
    /// [`accent`](Self::accent), so a config that only sets `accent`
    /// keeps one color for both.
    pub active_background: Color,
    /// Text on the open session and open tab chips
    /// (`active_foreground: "#1a1b26"`). Absent or blank uses the bar's
    /// font color, or reversed terminal text when that is unset too.
    pub active_foreground: Option<Color>,
    /// Fill of a session chip after that session's agent finishes a
    /// turn, until the session is viewed
    /// (`agent_finished_background: "#a855f7"`). The brighter of this and
    /// `agent_finished_foreground` colors the letter that walks a working
    /// session's name and finished names in the managers. Defaults to
    /// this purple when the key is absent.
    pub agent_finished_background: Color,
    /// Text on a just-finished agent chip
    /// (`agent_finished_foreground: "#1a1b26"`). Same fallback as
    /// [`active_foreground`](Self::active_foreground).
    pub agent_finished_foreground: Option<Color>,
    /// Fill of a session chip while its agent is working
    /// (`agent_working_background: "#24283b"`). Absent leaves the chip
    /// on the bar background, like any other.
    pub agent_working_background: Option<Color>,
    /// Text on a working agent's chip
    /// (`agent_working_foreground: "#7dcfff"`). Absent uses the bar's
    /// font color.
    pub agent_working_foreground: Option<Color>,
    /// A working session's name in the session menu
    /// (`agent_menu_working_foreground: "#7dcfff"`). Absent keeps the
    /// terminal's own text color. The open entry and just-finished
    /// entries keep their own colors.
    pub agent_menu_working_foreground: Option<Color>,
    /// A just-finished session's name in the session menu
    /// (`agent_menu_finished_foreground: "#a855f7"`). Absent uses the
    /// brighter of `agent_finished_background` /
    /// `agent_finished_foreground`.
    pub agent_menu_finished_foreground: Option<Color>,
    /// Environment variables set in every spawned shell
    /// (`terminal_envs:`). Defaults to just `TERM=xterm-256color` when
    /// the section is absent; when present, it is used exactly as given.
    pub envs: Vec<(String, String)>,
    /// Shell to spawn (`shell: /usr/bin/fish`). When unset, falls back
    /// to $SHELL, then the passwd entry, then /bin/sh.
    pub shell: Option<String>,
    /// Directory new shells start in (`start_dir: /home/you/code`).
    /// When unset, the user's home directory.
    pub start_dir: Option<String>,
    /// Lines of scrollback kept per pane (`scrollback_lines: 5000`).
    /// Applies to shells spawned after a change.
    pub scrollback_lines: usize,
    /// Mouse select-to-copy (`select_copy: true`): drag selects text in
    /// a pane, releasing copies it to the client's clipboard via OSC 52.
    pub select_copy: bool,
    /// Status bar at the top of the screen (`bar_position: top`) instead
    /// of the default bottom. Sessions always sit on the left and tabs
    /// on the right; there is no alignment setting.
    pub bar_top: bool,
    /// Agent bar (sessions created with `xmux agent`) at the top
    /// (`agent_bar_position: top`) or bottom. Defaults to the status
    /// bar's edge. Sharing an edge, the agent bar sits on the pane side
    /// of the status bar.
    pub agent_bar_top: bool,
    /// Background of the status bar, the one that lists sessions and
    /// tabs (`bar_background: "#1a1b26"`). Absent leaves the terminal's
    /// own background. Open and just-finished chips still paint their
    /// own chip color on top.
    pub bar_background: Option<Color>,
    /// Background of the agent bar
    /// (`agent_bar_background: "#24283b"`). Absent leaves the terminal's
    /// own background.
    pub agent_bar_background: Option<Color>,
    /// Text on the status bar (`bar_foreground: "#c0caf5"`). Absent
    /// leaves the terminal's own foreground, dimmed, and open or
    /// finished chips keep reversed text. When set, this is the text
    /// color of every chip on that bar.
    pub bar_foreground: Option<Color>,
    /// Text on the agent bar (`agent_bar_foreground: "#c0caf5"`). Same
    /// rules as [`bar_foreground`](Self::bar_foreground).
    pub agent_bar_foreground: Option<Color>,
}

impl Config {
    /// The open session and open tab chip.
    pub fn active_chip(&self) -> crate::render::ChipColor {
        crate::render::ChipColor {
            background: self.active_background,
            foreground: self.active_foreground,
        }
    }

    /// A just-finished agent session's chip.
    pub fn finished_chip(&self) -> crate::render::ChipColor {
        crate::render::ChipColor {
            background: self.agent_finished_background,
            foreground: self.agent_finished_foreground,
        }
    }

    /// A just-finished session's name in the session menu.
    pub fn menu_finished_color(&self) -> Color {
        self.agent_menu_finished_foreground
            .unwrap_or_else(|| self.finished_chip().signal())
    }

    /// A session's chip while its agent is working.
    pub fn working_chip(&self) -> crate::render::ChipStyle {
        crate::render::ChipStyle {
            background: self.agent_working_background,
            foreground: self.agent_working_foreground,
        }
    }
}

/// The server's controls: config name, default key, action.
const ACTIONS: [(&str, &str, InputAction); 13] = [
    (
        "session-manager",
        "ctrl+o",
        InputAction::Manager(Overlay::Sessions { agents: false }),
    ),
    ("tab-manager", "ctrl+n", InputAction::Manager(Overlay::Tabs)),
    ("split-horizontal", "ctrl+k", InputAction::SplitH),
    ("split-vertical", "ctrl+l", InputAction::SplitV),
    ("focus-next", "ctrl+t", InputAction::FocusNext),
    ("next-finished", "ctrl+a", InputAction::NextFinished),
    ("focus-left", "ctrl+q", InputAction::FocusDir(NavDir::Left)),
    (
        "focus-right",
        "ctrl+w",
        InputAction::FocusDir(NavDir::Right),
    ),
    ("focus-up", "ctrl+e", InputAction::FocusDir(NavDir::Up)),
    ("focus-down", "ctrl+r", InputAction::FocusDir(NavDir::Down)),
    ("detach", "ctrl+g", InputAction::Detach),
    ("fullscreen", "ctrl+f", InputAction::Fullscreen),
    ("terminal-settings", "ctrl+s", InputAction::PaneSettings),
];

#[derive(serde::Deserialize)]
struct RawPin {
    name: String,
    key: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    commands: HashMap<String, String>,
    #[serde(default)]
    keybindings: HashMap<String, String>,
    #[serde(default)]
    sessions: HashMap<u32, RawPin>,
    #[serde(default)]
    accent: Option<String>,
    /// Open session and tab chip fill. Absent or blank uses `accent`.
    #[serde(default)]
    active_background: Option<String>,
    #[serde(default)]
    active_foreground: Option<String>,
    /// Obsolete name of [`active_background`](Self::active_background).
    /// Accepted so a config written before the rename still loads. The
    /// new key wins when both are set.
    #[serde(default)]
    active_color: Option<String>,
    #[serde(default)]
    agent_finished_background: Option<String>,
    #[serde(default)]
    agent_finished_foreground: Option<String>,
    #[serde(default)]
    agent_working_background: Option<String>,
    #[serde(default)]
    agent_working_foreground: Option<String>,
    #[serde(default)]
    agent_menu_working_foreground: Option<String>,
    #[serde(default)]
    agent_menu_finished_foreground: Option<String>,
    /// Obsolete name of
    /// [`agent_finished_foreground`](Self::agent_finished_foreground).
    /// The new key wins when both are set.
    #[serde(default)]
    agent_foreground: Option<String>,
    /// Obsolete names of
    /// [`agent_finished_background`](Self::agent_finished_background),
    /// newest first. Accepted so a config written before a rename still
    /// loads. The new key wins when several are set.
    #[serde(default)]
    agent_background: Option<String>,
    #[serde(default)]
    agent_color_highlight: Option<String>,
    #[serde(default)]
    agent_finished: Option<String>,
    /// Obsolete. Accepted so a config written while the working symbol
    /// was a setting still loads.
    #[serde(default)]
    agent_working: Option<String>,
    /// Obsolete. Finished sessions use the highlight color as a chip
    /// background. Accepted so older configs still load.
    #[serde(default)]
    agent_finished_symbol: Option<String>,
    #[serde(default)]
    terminal_envs: Option<HashMap<String, String>>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    start_dir: Option<String>,
    #[serde(default)]
    scrollback_lines: Option<usize>,
    #[serde(default)]
    select_copy: Option<bool>,
    #[serde(default)]
    bar_position: Option<String>,
    /// Obsolete. Sessions are always on the left and tabs on the right.
    /// Accepted so a config written while alignment was a setting still
    /// loads.
    #[serde(default)]
    bar_alignment: Option<String>,
    /// Edge of the agent bar; absent follows `bar_position`.
    #[serde(default)]
    agent_bar_position: Option<String>,
    /// Obsolete. Agent chips are always left-aligned. Accepted so older
    /// config files still load.
    #[serde(default)]
    agent_bar_alignment: Option<String>,
    /// Status-bar background. Absent or blank leaves the terminal background.
    #[serde(default)]
    bar_background: Option<String>,
    /// Agent-bar background. Absent or blank leaves the terminal background.
    #[serde(default)]
    agent_bar_background: Option<String>,
    /// Status-bar text. Absent or blank leaves the terminal foreground.
    #[serde(default)]
    bar_foreground: Option<String>,
    /// Agent-bar text. Absent or blank leaves the terminal foreground.
    #[serde(default)]
    agent_bar_foreground: Option<String>,
}

pub fn load() -> Result<Config, String> {
    let raw = match read_config()? {
        Some((raw, path)) => {
            // Reject unknown action names early with the file context.
            for name in raw.keybindings.keys() {
                if !ACTIONS.iter().any(|(n, ..)| n == name) {
                    let known: Vec<&str> = ACTIONS.iter().map(|(n, ..)| *n).collect();
                    return Err(format!(
                        "unknown keybinding \"{name}\" in {path} (known: {})",
                        known.join(", ")
                    ));
                }
            }
            raw
        }
        None => RawConfig {
            commands: HashMap::new(),
            keybindings: HashMap::new(),
            sessions: HashMap::new(),
            accent: None,
            active_background: None,
            active_foreground: None,
            active_color: None,
            agent_finished_background: None,
            agent_finished_foreground: None,
            agent_working_background: None,
            agent_working_foreground: None,
            agent_menu_working_foreground: None,
            agent_menu_finished_foreground: None,
            agent_foreground: None,
            agent_background: None,
            agent_color_highlight: None,
            agent_finished: None,
            agent_working: None,
            agent_finished_symbol: None,
            terminal_envs: None,
            shell: None,
            start_dir: None,
            scrollback_lines: None,
            select_copy: None,
            bar_position: None,
            bar_alignment: None,
            agent_bar_position: None,
            agent_bar_alignment: None,
            bar_background: None,
            agent_bar_background: None,
            bar_foreground: None,
            agent_bar_foreground: None,
        },
    };
    load_raw(raw)
}

/// Resolve a parsed config file into a [`Config`]: defaults, renamed
/// keys, colors, and bindings.
fn load_raw(raw: RawConfig) -> Result<Config, String> {
    let accent = match &raw.accent {
        Some(spec) => parse_color(spec, "accent")?,
        None => Color::Cyan,
    };
    let active_background = active_background_color(&raw, accent)?;
    let active_foreground = optional_color(raw.active_foreground.as_deref(), "active_foreground")?;
    let agent_finished_background = agent_finished_background_color(&raw)?;
    let agent_finished_foreground = first_color(&[
        (&raw.agent_finished_foreground, "agent_finished_foreground"),
        (&raw.agent_foreground, "agent_foreground"),
    ])?;
    let agent_working_background = optional_color(
        raw.agent_working_background.as_deref(),
        "agent_working_background",
    )?;
    let agent_working_foreground = optional_color(
        raw.agent_working_foreground.as_deref(),
        "agent_working_foreground",
    )?;
    let agent_menu_working_foreground = optional_color(
        raw.agent_menu_working_foreground.as_deref(),
        "agent_menu_working_foreground",
    )?;
    let agent_menu_finished_foreground = optional_color(
        raw.agent_menu_finished_foreground.as_deref(),
        "agent_menu_finished_foreground",
    )?;
    // Old mark keys are ignored. Mentioning them keeps
    // `deny_unknown_fields` from rejecting a config that still has them.
    let _ = raw.agent_working;
    let _ = raw.agent_finished_symbol;

    let envs: Vec<(String, String)> = match raw.terminal_envs {
        Some(map) => {
            for key in map.keys() {
                if key.is_empty() || key.contains('=') || key.contains('\0') {
                    return Err(format!("invalid terminal_envs name \"{key}\""));
                }
            }
            let mut envs: Vec<(String, String)> = map.into_iter().collect();
            envs.sort();
            envs
        }
        None => vec![("TERM".to_string(), "xterm-256color".to_string())],
    };

    let mut bindings = Vec::new();

    // Controls: user's key if rebound, else the default.
    for (name, default_key, action) in ACTIONS {
        let key = raw
            .keybindings
            .get(name)
            .map_or(default_key, |k| k.as_str());
        let seqs = parse_key_multi(key)
            .map_err(|e| format!("invalid key \"{key}\" for keybinding \"{name}\": {e}"))?;
        for seq in seqs {
            bindings.push(Binding {
                seq,
                action: BindingAction::Control(action),
            });
        }
    }

    // Command bindings: the chord types a program into the shell.
    for (key, run) in raw.commands {
        let seqs =
            parse_key_multi(&key).map_err(|e| format!("invalid command binding \"{key}\": {e}"))?;
        for seq in seqs {
            bindings.push(Binding {
                seq,
                action: BindingAction::Run(run.clone()),
            });
        }
    }

    // Pinned sessions, sorted by slot; each gets an OpenSession binding.
    let mut slots: Vec<(&u32, &RawPin)> = raw.sessions.iter().collect();
    slots.sort_by_key(|(slot, _)| **slot);
    let mut pins = Vec::new();
    for (slot, pin) in slots {
        if pin.name.trim().is_empty() {
            return Err(format!("session slot {slot}: name must not be empty"));
        }
        if pins.iter().any(|p: &Pin| p.name == pin.name.trim()) {
            return Err(format!(
                "session \"{}\" is pinned to more than one slot",
                pin.name.trim()
            ));
        }
        let seqs = parse_key_multi(&pin.key)
            .map_err(|e| format!("invalid key \"{}\" for session slot {slot}: {e}", pin.key))?;
        for seq in seqs {
            bindings.push(Binding {
                seq,
                action: BindingAction::Control(InputAction::OpenSession(pins.len())),
            });
        }
        pins.push(Pin {
            name: pin.name.trim().to_string(),
        });
    }

    // The same chord bound twice would be ambiguous.
    for (i, a) in bindings.iter().enumerate() {
        if bindings[i + 1..].iter().any(|b| b.seq == a.seq) {
            return Err(format!(
                "duplicate key binding: {} is bound more than once",
                describe_seq(&a.seq)
            ));
        }
    }

    // Longest sequence first, so e.g. ctrl+alt+h wins over ctrl+h when
    // both could match at the same input position.
    bindings.sort_by_key(|b| std::cmp::Reverse(b.seq.len()));
    let shell = match raw.shell {
        Some(s) => {
            let s = s.trim().to_string();
            if s.is_empty() {
                return Err("shell must not be empty".to_string());
            }
            // Absolute paths can be checked up front; bare names are
            // resolved via PATH at spawn time.
            if s.contains('/') && !std::path::Path::new(&s).exists() {
                return Err(format!("shell \"{s}\" does not exist"));
            }
            Some(s)
        }
        None => None,
    };

    let start_dir = match raw.start_dir {
        Some(s) => {
            let s = s.trim();
            if s.is_empty() {
                None // same as unset: the home directory
            } else {
                // Allow `~/...` for convenience.
                let expanded = match (s.strip_prefix("~"), std::env::var_os("HOME")) {
                    (Some(rest), Some(home)) => {
                        format!("{}{rest}", home.to_string_lossy())
                    }
                    _ => s.to_string(),
                };
                if !std::path::Path::new(&expanded).is_dir() {
                    return Err(format!("start_dir \"{s}\" is not a directory"));
                }
                Some(expanded)
            }
        }
        None => None,
    };

    let scrollback_lines = match raw.scrollback_lines {
        Some(n) if n > 1_000_000 => {
            return Err(format!("scrollback_lines {n} is too large (max 1000000)"));
        }
        Some(n) => n,
        None => 5000,
    };

    // Absent means the bottom edge.
    let bar_top = parse_edge(raw.bar_position.as_deref(), false, "bar_position")?;
    // Absent means the same edge as the status bar.
    let agent_bar_top = parse_edge(
        raw.agent_bar_position.as_deref(),
        bar_top,
        "agent_bar_position",
    )?;
    // Obsolete keys. Mentioning them keeps `deny_unknown_fields` from
    // rejecting a config written when alignment was a setting.
    let _ = raw.bar_alignment;
    let _ = raw.agent_bar_alignment;
    let bar_background = optional_color(raw.bar_background.as_deref(), "bar_background")?;
    let agent_bar_background =
        optional_color(raw.agent_bar_background.as_deref(), "agent_bar_background")?;
    let bar_foreground = optional_color(raw.bar_foreground.as_deref(), "bar_foreground")?;
    let agent_bar_foreground =
        optional_color(raw.agent_bar_foreground.as_deref(), "agent_bar_foreground")?;

    Ok(Config {
        bindings,
        pins,
        accent,
        active_background,
        active_foreground,
        agent_finished_background,
        agent_finished_foreground,
        agent_working_background,
        agent_working_foreground,
        agent_menu_working_foreground,
        agent_menu_finished_foreground,
        envs,
        shell,
        start_dir,
        scrollback_lines,
        select_copy: raw.select_copy.unwrap_or(true),
        bar_top,
        agent_bar_top,
        bar_background,
        agent_bar_background,
        bar_foreground,
        agent_bar_foreground,
    })
}

/// `top` or `bottom`. An absent or blank value uses `default_top`.
fn parse_edge(value: Option<&str>, default_top: bool, field: &str) -> Result<bool, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(default_top),
        Some("bottom") => Ok(false),
        Some("top") => Ok(true),
        Some(other) => Err(format!(
            "invalid {field} \"{other}\" (expected top or bottom)"
        )),
    }
}

/// The first of `keys` that is set and not blank, parsed.
fn first_color(keys: &[(&Option<String>, &str)]) -> Result<Option<Color>, String> {
    for (spec, field) in keys {
        if let Some(color) = optional_color(spec.as_deref(), field)? {
            return Ok(Some(color));
        }
    }
    Ok(None)
}

/// Open-chip fill: `active_background`, else the old `active_color`,
/// else `accent`.
fn active_background_color(raw: &RawConfig, accent: Color) -> Result<Color, String> {
    Ok(first_color(&[
        (&raw.active_background, "active_background"),
        (&raw.active_color, "active_color"),
    ])?
    .unwrap_or(accent))
}

/// Finished-agent fill: `agent_finished_background`, else the old
/// `agent_background`, `agent_color_highlight` or `agent_finished`,
/// else the shipped purple.
fn agent_finished_background_color(raw: &RawConfig) -> Result<Color, String> {
    Ok(first_color(&[
        (&raw.agent_finished_background, "agent_finished_background"),
        (&raw.agent_background, "agent_background"),
        (&raw.agent_color_highlight, "agent_color_highlight"),
        (&raw.agent_finished, "agent_finished"),
    ])?
    .unwrap_or_else(default_agent_finished_background))
}

/// Purple used when no finished-agent background is configured. Matches the hex
/// shipped in the default config.
fn default_agent_finished_background() -> Color {
    Color::Rgb {
        r: 0xa8,
        g: 0x55,
        b: 0xf7,
    }
}

/// A bar background. Absent or blank means the terminal's own background.
fn optional_color(spec: Option<&str>, field: &str) -> Result<Option<Color>, String> {
    match spec.map(str::trim) {
        None | Some("") => Ok(None),
        Some(spec) => Ok(Some(parse_color(spec, field)?)),
    }
}

/// Parse `#rrggbb` (or shorthand `#rgb`) into an RGB color.
fn parse_color(spec: &str, field: &str) -> Result<Color, String> {
    let hex = spec.trim().trim_start_matches('#');
    let expand = |h: &str| -> Option<(u8, u8, u8)> {
        let full: String = match h.len() {
            3 => h.chars().flat_map(|c| [c, c]).collect(),
            6 => h.to_string(),
            _ => return None,
        };
        let byte = |i| u8::from_str_radix(&full[i..i + 2], 16).ok();
        Some((byte(0)?, byte(2)?, byte(4)?))
    };
    match expand(hex) {
        Some((r, g, b)) => Ok(Color::Rgb { r, g, b }),
        None => Err(format!(
            "invalid {field} \"{spec}\" (expected hex like #7aa2f7)"
        )),
    }
}

/// Directory override from `--config <dir>` (config.yaml and
/// layout.json both live in it).
static CONFIG_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Set the config directory (from the `--config` flag); first call wins.
pub fn set_dir(dir: std::path::PathBuf) {
    let _ = CONFIG_DIR.set(dir);
}

/// Where the config lives: `<--config dir>/config.yaml` when the flag
/// was given, else `~/.config/xmux/config.yaml`.
pub fn path() -> Option<std::path::PathBuf> {
    if let Some(dir) = CONFIG_DIR.get() {
        return Some(dir.join("config.yaml"));
    }
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join(".config/xmux/config.yaml"))
}

/// Written to `~/.config/xmux/config.yaml` when no config exists, so
/// there is always a file to edit.
const DEFAULT_CONFIG: &str = include_str!("../default-config.yaml");

fn read_config() -> Result<Option<(RawConfig, String)>, String> {
    let Some(path) = path() else {
        return Ok(None);
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // No config file: create the default one, then run with it. The
        // write is best-effort — a read-only $HOME still gets defaults.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::write(&path, DEFAULT_CONFIG) {
                Ok(()) => eprintln!("created default config at {}", path.display()),
                Err(e) => eprintln!("could not create default config at {}: {e}", path.display()),
            }
            DEFAULT_CONFIG.to_string()
        }
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };

    let raw: RawConfig = serde_yaml::from_str(&text)
        .map_err(|e| format!("invalid config {}: {e}", path.display()))?;
    Ok(Some((raw, path.display().to_string())))
}

/// Human-readable form of a chord's byte sequence for error messages.
fn describe_seq(seq: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut rest = seq;
    if rest.first() == Some(&0x1b) && rest.len() > 1 {
        parts.push("alt".to_string());
        rest = &rest[1..];
    }
    match rest {
        [b] if *b < 0x20 => parts.push(format!("ctrl+{}", (b'a' + (b - 1)) as char)),
        _ => parts.push(String::from_utf8_lossy(rest).into_owned()),
    }
    parts.join("+")
}

/// Like `parse_key`, but also accepts `F1`-`F12`. F-keys have two
/// encodings in the wild (SS3 for F1-F4 in most terminals, plus the
/// legacy CSI form), so a spec may map to several byte sequences.
fn parse_key_multi(spec: &str) -> Result<Vec<Vec<u8>>, String> {
    let lower = spec.trim().to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|r| r.parse::<u8>().ok()) {
        let seqs: &[&[u8]] = match n {
            1 => &[b"\x1bOP", b"\x1b[11~"],
            2 => &[b"\x1bOQ", b"\x1b[12~"],
            3 => &[b"\x1bOR", b"\x1b[13~"],
            4 => &[b"\x1bOS", b"\x1b[14~"],
            5 => &[b"\x1b[15~"],
            6 => &[b"\x1b[17~"],
            7 => &[b"\x1b[18~"],
            8 => &[b"\x1b[19~"],
            9 => &[b"\x1b[20~"],
            10 => &[b"\x1b[21~"],
            11 => &[b"\x1b[23~"],
            12 => &[b"\x1b[24~"],
            _ => return Err(format!("F{n} is not supported (F1-F12)")),
        };
        return Ok(seqs.iter().map(|s| s.to_vec()).collect());
    }
    Ok(vec![parse_key(spec)?])
}

/// Translate a `[ctrl+][alt+]<char>` spec into the bytes the client's
/// terminal sends for that chord.
fn parse_key(spec: &str) -> Result<Vec<u8>, String> {
    let parts: Vec<&str> = spec.split('+').collect();
    let (mods, key) = parts.split_at(parts.len() - 1);

    let (mut ctrl, mut alt) = (false, false);
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" => ctrl = true,
            "alt" => alt = true,
            other => return Err(format!("unknown modifier \"{other}\" (use ctrl/alt)")),
        }
    }

    let mut chars = key[0].chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        return Err(format!(
            "key must be a single character, got \"{}\"",
            key[0]
        ));
    };

    // A binding without modifiers would hijack ordinary typing.
    if !ctrl && !alt {
        return Err("key needs at least one modifier (ctrl/alt)".into());
    }

    let mut seq = Vec::with_capacity(5);
    if alt {
        seq.push(0x1b); // ESC prefix
    }
    if ctrl {
        if !ch.is_ascii_alphabetic() {
            return Err("ctrl+ bindings only support letters a-z".into());
        }
        seq.push(ch.to_ascii_lowercase() as u8 & 0x1f);
    } else {
        let mut buf = [0u8; 4];
        seq.extend_from_slice(ch.to_ascii_lowercase().encode_utf8(&mut buf).as_bytes());
    }
    Ok(seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_edges_default_to_the_bottom() {
        assert_eq!(parse_edge(None, false, "bar_position").unwrap(), false);
        assert_eq!(parse_edge(Some(""), false, "bar_position").unwrap(), false);
        assert_eq!(
            parse_edge(Some("bottom"), false, "agent_bar_position").unwrap(),
            false
        );
        assert_eq!(
            parse_edge(Some("top"), false, "bar_position").unwrap(),
            true
        );
        assert!(parse_edge(Some("left"), false, "agent_bar_position").is_err());
    }

    #[test]
    fn colors_are_hex() {
        assert_eq!(
            parse_color("#7aa2f7", "accent").unwrap(),
            Color::Rgb {
                r: 0x7a,
                g: 0xa2,
                b: 0xf7
            }
        );
        assert_eq!(
            parse_color("#a5f", "agent_finished").unwrap(),
            Color::Rgb {
                r: 0xaa,
                g: 0x55,
                b: 0xff
            }
        );
        let err = parse_color("purple", "agent_finished").unwrap_err();
        assert!(err.contains("agent_finished"), "{err}");
        assert!(err.contains("purple"), "{err}");
    }

    #[test]
    fn bar_backgrounds_are_optional_hex() {
        assert_eq!(optional_color(None, "bar_background").unwrap(), None);
        assert_eq!(
            optional_color(Some("  "), "agent_bar_background").unwrap(),
            None
        );
        assert_eq!(
            optional_color(Some("#1a1b26"), "bar_background").unwrap(),
            Some(Color::Rgb {
                r: 0x1a,
                g: 0x1b,
                b: 0x26
            })
        );
        let err = optional_color(Some("navy"), "agent_bar_background").unwrap_err();
        assert!(err.contains("agent_bar_background"), "{err}");
        assert!(err.contains("navy"), "{err}");
        assert!(DEFAULT_CONFIG.contains("bar_background: \"#1a1b26\""));
        assert!(DEFAULT_CONFIG.contains("agent_bar_background: \"#24283b\""));
        assert_eq!(optional_color(None, "bar_foreground").unwrap(), None);
        assert_eq!(
            optional_color(Some("#c0caf5"), "bar_foreground").unwrap(),
            Some(Color::Rgb {
                r: 0xc0,
                g: 0xca,
                b: 0xf5
            })
        );
        assert!(DEFAULT_CONFIG.contains("bar_foreground: \"#c0caf5\""));
        assert!(DEFAULT_CONFIG.contains("agent_bar_foreground: \"#c0caf5\""));
    }

    fn blank_raw() -> RawConfig {
        serde_yaml::from_str("{}").unwrap()
    }

    #[test]
    fn active_background_follows_accent_until_it_is_set() {
        let accent = parse_color("#7aa2f7", "accent").unwrap();
        let zinc = parse_color("#d4d4d8", "active_background").unwrap();
        let raw = |spec: &str| RawConfig {
            active_background: Some(spec.into()),
            ..blank_raw()
        };
        assert_eq!(
            active_background_color(&blank_raw(), accent).unwrap(),
            accent
        );
        assert_eq!(active_background_color(&raw("  "), accent).unwrap(), accent);
        assert_eq!(
            active_background_color(&raw("#d4d4d8"), accent).unwrap(),
            zinc
        );
        let err = active_background_color(&raw("white"), accent).unwrap_err();
        assert!(err.contains("active_background"), "{err}");
        let shipped: RawConfig = serde_yaml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(shipped.active_background.as_deref(), Some("#7aa2f7"));
        assert_eq!(shipped.active_color, None);
    }

    #[test]
    fn the_old_active_color_key_still_supplies_the_fill() {
        let accent = parse_color("#7aa2f7", "accent").unwrap();
        let old = RawConfig {
            active_color: Some("#112233".into()),
            ..blank_raw()
        };
        let navy = parse_color("#112233", "active_color").unwrap();
        assert_eq!(active_background_color(&old, accent).unwrap(), navy);
        let renamed = RawConfig {
            active_background: Some("#d4d4d8".into()),
            ..old
        };
        assert_eq!(
            active_background_color(&renamed, accent).unwrap(),
            parse_color("#d4d4d8", "active_background").unwrap()
        );
    }

    #[test]
    fn chip_foregrounds_are_optional_hex() {
        let raw: RawConfig = serde_yaml::from_str(
            "active_foreground: \"#1a1b26\"\nagent_finished_foreground: \"\"\n",
        )
        .unwrap();
        assert_eq!(
            optional_color(raw.active_foreground.as_deref(), "active_foreground").unwrap(),
            Some(parse_color("#1a1b26", "active_foreground").unwrap())
        );
        assert_eq!(
            optional_color(
                raw.agent_finished_foreground.as_deref(),
                "agent_finished_foreground"
            )
            .unwrap(),
            None
        );
        assert!(DEFAULT_CONFIG.contains("active_foreground:"));
        assert!(DEFAULT_CONFIG.contains("agent_finished_foreground:"));
    }

    #[test]
    fn agent_working_colors_are_optional_and_load() {
        let unset = load_raw(blank_raw()).unwrap();
        assert_eq!(unset.agent_working_background, None);
        assert_eq!(unset.agent_working_foreground, None);
        let raw: RawConfig = serde_yaml::from_str(
            "agent_working_background: \"#24283b\"\nagent_working_foreground: \"#7dcfff\"\n",
        )
        .unwrap();
        let config = load_raw(raw).unwrap();
        assert_eq!(
            config.working_chip(),
            crate::render::ChipStyle {
                background: Some(parse_color("#24283b", "x").unwrap()),
                foreground: Some(parse_color("#7dcfff", "x").unwrap()),
            }
        );
        let bad: RawConfig = serde_yaml::from_str("agent_working_foreground: teal\n").unwrap();
        let err = load_raw(bad).err().unwrap();
        assert!(err.contains("agent_working_foreground"), "{err}");
        assert!(DEFAULT_CONFIG.contains("agent_working_background:"));
        assert!(DEFAULT_CONFIG.contains("agent_working_foreground:"));
    }

    #[test]
    fn menu_colors_are_optional_and_finished_falls_back_to_the_chip() {
        let gold = parse_color("#ffd88a", "x").unwrap();
        let mint = parse_color("#7ee0d0", "x").unwrap();
        let unset = load_raw(blank_raw()).unwrap();
        assert_eq!(unset.agent_menu_working_foreground, None);
        assert_eq!(unset.agent_menu_finished_foreground, None);
        assert_eq!(unset.menu_finished_color(), unset.finished_chip().signal());
        let menu: RawConfig = serde_yaml::from_str(
            "agent_menu_working_foreground: \"#7ee0d0\"\n\
             agent_menu_finished_foreground: \"#ffd88a\"\n",
        )
        .unwrap();
        let menu = load_raw(menu).unwrap();
        assert_eq!(menu.agent_menu_working_foreground, Some(mint));
        assert_eq!(menu.menu_finished_color(), gold);
        for key in [
            "agent_menu_working_foreground",
            "agent_menu_finished_foreground",
        ] {
            let bad: RawConfig = serde_yaml::from_str(&format!("{key}: teal\n")).unwrap();
            let err = load_raw(bad).err().unwrap();
            assert!(err.contains(key), "{err}");
            assert!(DEFAULT_CONFIG.contains(&format!("{key}:")), "{key}");
        }
        // The walking-letter keys are gone.
        for key in [
            "agent_working_foreground_animation",
            "agent_menu_working_foreground_animation",
        ] {
            assert!(serde_yaml::from_str::<RawConfig>(&format!("{key}: \"#ffffff\"\n")).is_err());
            assert!(!DEFAULT_CONFIG.contains(key), "{key}");
        }
    }

    #[test]
    fn agent_finished_background_defaults_to_the_shipped_purple() {
        assert_eq!(
            default_agent_finished_background(),
            parse_color("#a855f7", "agent_finished_background").unwrap()
        );
        assert_eq!(
            agent_finished_background_color(&blank_raw()).unwrap(),
            default_agent_finished_background()
        );
        assert!(DEFAULT_CONFIG.contains("agent_finished_background: \"#a855f7\""));
        for old in [
            "agent_finished:",
            "agent_background:",
            "agent_foreground:",
            "agent_color_highlight",
            "active_color",
        ] {
            assert!(!DEFAULT_CONFIG.contains(old), "{old}");
        }
    }

    #[test]
    fn old_agent_keys_still_supply_the_finished_chip() {
        let old = RawConfig {
            agent_finished: Some("#112233".into()),
            agent_working: Some("?".into()),
            agent_finished_symbol: Some("!".into()),
            ..blank_raw()
        };
        let navy = Color::Rgb {
            r: 0x11,
            g: 0x22,
            b: 0x33,
        };
        assert_eq!(agent_finished_background_color(&old).unwrap(), navy);
        let highlight = RawConfig {
            agent_color_highlight: Some("#445566".into()),
            ..old
        };
        assert_eq!(
            agent_finished_background_color(&highlight).unwrap(),
            parse_color("#445566", "agent_color_highlight").unwrap()
        );
        let background = RawConfig {
            agent_background: Some("#778899".into()),
            ..highlight
        };
        assert_eq!(
            agent_finished_background_color(&background).unwrap(),
            parse_color("#778899", "agent_background").unwrap()
        );
        let renamed = RawConfig {
            agent_finished_background: Some("#a855f7".into()),
            ..background
        };
        assert_eq!(
            agent_finished_background_color(&renamed).unwrap(),
            default_agent_finished_background()
        );

        // The old foreground key still loads, and the new one wins.
        let fg: RawConfig = serde_yaml::from_str("agent_foreground: \"#112233\"\n").unwrap();
        assert_eq!(load_raw(fg).unwrap().agent_finished_foreground, Some(navy));
        let both: RawConfig = serde_yaml::from_str(
            "agent_foreground: \"#112233\"\nagent_finished_foreground: \"#445566\"\n",
        )
        .unwrap();
        assert_eq!(
            load_raw(both).unwrap().agent_finished_foreground,
            Some(parse_color("#445566", "x").unwrap())
        );
    }

    #[test]
    fn next_finished_defaults_to_ctrl_a() {
        let (name, key, action) = ACTIONS
            .iter()
            .find(|(name, ..)| *name == "next-finished")
            .expect("next-finished action");
        assert_eq!(*name, "next-finished");
        assert_eq!(*key, "ctrl+a");
        assert!(matches!(action, InputAction::NextFinished));
        assert!(DEFAULT_CONFIG.contains("next-finished: ctrl+a"));
    }

    #[test]
    fn old_mark_keys_still_parse() {
        let raw: RawConfig = serde_yaml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(raw.agent_working, None);
        assert_eq!(raw.agent_finished_symbol, None);
        assert_eq!(raw.agent_finished, None);
        assert_eq!(raw.agent_color_highlight, None);
        assert_eq!(raw.agent_background, None);
        assert_eq!(raw.agent_finished_background.as_deref(), Some("#a855f7"));
        // Leftover mark keys still parse.
        let old: RawConfig = serde_yaml::from_str(
            "agent_working: \"?\"\nagent_finished_symbol: \"!\"\nagent_finished: \"#112233\"\n",
        )
        .unwrap();
        assert_eq!(old.agent_working.as_deref(), Some("?"));
        assert_eq!(old.agent_finished_symbol.as_deref(), Some("!"));
        assert_eq!(old.agent_finished.as_deref(), Some("#112233"));
    }
}
