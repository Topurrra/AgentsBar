//! CLI `status` subcommand.
//!
//! The binary is a GUI-subsystem exe, so it has no console of its own. When invoked as
//! `agentsbar status` from a terminal we attach to that terminal and print a snapshot of
//! every enabled provider, reading the same numbers the tray shows (persisted to
//! `snapshots.json` on every refresh). `--json` prints the raw display snapshots for
//! scripting.
//!
//! The visual language (palette, glyphs, spacing) mirrors the Voli CLI so the two tools
//! read as one family.

use std::io::Write;

use crate::config::Config;
use crate::state::DisplaySnapshot;

const USAGE: &str = "\
AgentsBar — AI coding usage limits in your tray

USAGE:
    agentsbar                 Launch the tray app (default)
    agentsbar status          Show every provider's current usage
    agentsbar status --json   Same, as JSON for scripting
    agentsbar --version       Print the version
    agentsbar --help          This help
";

/// True when the first argument is a CLI subcommand rather than "run the GUI".
pub fn wants_cli() -> bool {
    matches!(
        std::env::args().nth(1).as_deref(),
        Some("status" | "--version" | "-V" | "help" | "--help" | "-h")
    )
}

/// CLI entry point. Called from `main` before the GUI starts; never returns to it.
pub fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("") {
        "--version" | "-V" => {
            if let Some(mut out) = output() {
                let _ = writeln!(out.writer, "AgentsBar {}", env!("CARGO_PKG_VERSION"));
            }
        }
        "help" | "--help" | "-h" => {
            if let Some(mut out) = output() {
                let _ = write!(out.writer, "{USAGE}");
            }
        }
        "status" => {
            let json = args.iter().skip(2).any(|a| a == "--json" || a == "-j");
            let Some(mut out) = output() else { return };
            if let Err(e) = status(&mut out, json) {
                let _ = writeln!(out.writer, "error: {e}");
                std::process::exit(1);
            }
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ console plumbing

struct CliOut {
    writer: Box<dyn Write>,
    /// False when stdout is redirected to a file or pipe, so ANSI codes are suppressed.
    color: bool,
}

/// Open somewhere for the output to go.
///
/// A release build is a GUI-subsystem process with no console of its own; a debug build
/// is a console process that already has one. Two cases:
/// - stdout was redirected (`status > out.txt`, `status --json | jq`): the inherited
///   handle is a disk/pipe handle, so write to stdout directly and honour the redirect,
///   with no colour.
/// - otherwise (a terminal, or a GUI app launched from one): write to the console screen
///   buffer (CONOUT$) with colour. A GUI app attaches to the invoking terminal first; a
///   debug build already has a console so the attach is a harmless no-op.
///
/// Returns None when there is no console to write to (the GUI exe was double-clicked), so
/// the CLI silently does nothing rather than panicking on a missing stdout.
fn output() -> Option<CliOut> {
    use windows::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_DISK, FILE_TYPE_PIPE};
    use windows::Win32::System::Console::{
        AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE,
    };

    unsafe {
        if let Ok(handle) = GetStdHandle(STD_OUTPUT_HANDLE) {
            if !handle.is_invalid() {
                let ty = GetFileType(handle);
                if ty == FILE_TYPE_DISK || ty == FILE_TYPE_PIPE {
                    return Some(CliOut {
                        writer: Box::new(std::io::stdout()),
                        color: false,
                    });
                }
            }
        }
        // Console output. Attach a GUI app to the invoking terminal; ignore the error a
        // debug build gets for already having one. Then write to the screen buffer.
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        let conout = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .open("CONOUT$")
            .ok()?;
        enable_vt();
        Some(CliOut {
            writer: Box::new(conout),
            color: true,
        })
    }
}

/// Turn on ANSI escape processing for the attached console so the palette renders as
/// colour instead of literal `[38;2;…m` text. A failure is harmless: output stays plain.
fn enable_vt() {
    use windows::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let Ok(handle) = GetStdHandle(STD_OUTPUT_HANDLE) else {
            return;
        };
        let mut mode = CONSOLE_MODE::default();
        if GetConsoleMode(handle, &mut mode).is_err() {
            return;
        }
        let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
    }
}

// ------------------------------------------------------------------ status command

fn status(out: &mut CliOut, json: bool) -> Result<(), String> {
    let snapshots = load_snapshots()?;
    if json {
        let text = serde_json::to_string_pretty(&snapshots).map_err(|e| e.to_string())?;
        writeln!(out.writer, "{text}").map_err(|e| e.to_string())?;
        return Ok(());
    }
    let config = Config::load();
    render_human(out, &config, &snapshots)
}

/// Read the display snapshots the app persisted on its last refresh.
fn load_snapshots() -> Result<Vec<DisplaySnapshot>, String> {
    let path = crate::state::snapshots_path();
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "no data yet — open AgentsBar once so it can refresh (expected {})",
            path.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|e| format!("could not read {}: {e}", path.display()))
}

// ------------------------------------------------------------------ human rendering

/// The palette, mirrored from the Voli CLI so the two tools read as one family.
/// Voli uses no colour crate and no hex palette — just the standard 8-colour ANSI set,
/// applied sparingly: bold for labels, dim for metadata, colour only for state. These
/// are suppressed entirely when output is redirected.
mod pal {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const GREEN: &str = "\x1b[32m"; // ok / plenty left
    pub const YELLOW: &str = "\x1b[33m"; // watch / getting low
    pub const RED: &str = "\x1b[31m"; // low / spent
    pub const BOLD_RED: &str = "\x1b[1;31m"; // error
    pub const CYAN: &str = "\x1b[36m"; // info (spend / balance)
    pub const BOLD_CYAN: &str = "\x1b[1;36m"; // emphasis numbers
}

/// Wrap `text` in `code`…RESET when colour is on, otherwise return it untouched.
fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("{code}{text}{}", pal::RESET)
    } else {
        text.to_string()
    }
}

/// The most constrained window the tray speaks for, same call the tray and tile use.
fn lead(ds: &DisplaySnapshot) -> Option<crate::state::DisplayWindow> {
    crate::state::lead_window(&ds.snapshot)
}

/// Coarse countdown to a reset instant: "2h 14m", "1d 3h", "45m". Mirrors the tray.
fn countdown(
    resets_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let secs = (resets_at - now).num_seconds();
    if secs <= 0 {
        return "now".to_string();
    }
    if secs >= 86_400 {
        format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3_600)
    } else if secs >= 3_600 {
        format!("{}h {}m", secs / 3_600, (secs % 3_600) / 60)
    } else {
        format!("{}m", (secs / 60).max(1))
    }
}

/// One provider's state, for colour and glyph. Glyphs and colours follow Voli: ✓ ok,
/// ! warning, × stop, ◆ info (a dollar figure rather than a quota), - nothing to show.
enum State {
    Calm,
    Watch,
    Low,
    Spent,
    /// No quota window, but the provider reports a dollar figure (spend or balance).
    Info,
    Unknown,
    Error,
}

impl State {
    fn color(&self) -> &'static str {
        match self {
            State::Calm => pal::GREEN,
            State::Watch => pal::YELLOW,
            State::Low => pal::RED,
            State::Spent => pal::RED,
            State::Info => pal::CYAN,
            State::Unknown => pal::DIM,
            State::Error => pal::BOLD_RED,
        }
    }

    fn glyph(&self) -> &'static str {
        match self {
            State::Calm => "✓",
            State::Watch => "!",
            State::Low => "×",
            State::Spent => "×",
            State::Info => "◆",
            State::Error => "×",
            State::Unknown => "-",
        }
    }
}

fn classify(ds: &DisplaySnapshot, window: &Option<crate::state::DisplayWindow>) -> State {
    if ds.snapshot.error.is_some() {
        return State::Error;
    }
    if let Some(used) = window.as_ref().and_then(|w| w.used_percent) {
        let left = 100.0 - used;
        return if left <= 0.0 {
            State::Spent
        } else if left < 20.0 {
            State::Low
        } else if left <= 50.0 {
            State::Watch
        } else {
            State::Calm
        };
    }
    // No quota window. A dollar figure is still a real reading, just not a quota.
    let has_usd = ds.snapshot.credits_unit.as_deref() == Some("USD")
        && ds.snapshot.credits.is_some_and(|v| v.is_finite());
    if ds.snapshot.spend_usd.is_some_and(|v| v.is_finite()) || has_usd {
        State::Info
    } else {
        State::Unknown
    }
}

/// Sort key: providers with a number first (most urgent = least remaining at the top),
/// then the data-less, then errors — the same priority the tile list uses.
fn sort_key(ds: &DisplaySnapshot) -> (u8, i64) {
    if ds.snapshot.error.is_some() {
        return (2, 0);
    }
    match lead(ds).and_then(|w| w.used_percent) {
        Some(used) => (0, (100.0 - used).round() as i64),
        None => (1, 0),
    }
}

fn render_human(
    out: &mut CliOut,
    _config: &Config,
    snapshots: &[DisplaySnapshot],
) -> Result<(), String> {
    // `color_on` is a Copy bool, so painting never re-borrows `out` while `w` holds the
    // writer; that keeps the two from fighting over the same `CliOut`.
    let color_on = out.color;
    let w = &mut out.writer;
    let now = chrono::Utc::now();

    // Header: bold name and a dim tagline on one line — Voli has no banner art.
    writeln!(w)
        .and_then(|_| {
            writeln!(
                w,
                "  {} {}",
                paint(color_on, pal::BOLD, "AgentsBar"),
                paint(color_on, pal::DIM, "— AI coding usage limits")
            )
        })
        .and_then(|_| writeln!(w))
        .map_err(|e| e.to_string())?;

    if snapshots.is_empty() {
        writeln!(
            w,
            "  {}",
            paint(color_on, pal::DIM, "No providers enabled.")
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    }

    // Name column width: the longest provider name, so the columns line up.
    let name_w = snapshots
        .iter()
        .map(|ds| crate::providers::provider_name(&ds.snapshot.provider_id).len())
        .max()
        .unwrap_or(0)
        .max(4);

    let mut rows: Vec<&DisplaySnapshot> = snapshots.iter().collect();
    rows.sort_by_key(|ds| sort_key(ds));

    for ds in rows {
        let name = crate::providers::provider_name(&ds.snapshot.provider_id);
        let window = lead(ds);
        let state = classify(ds, &window);
        let color = state.color();
        let glyph = paint(color_on, color, state.glyph());

        // Headline: the one number that matters, padded on its VISIBLE length and only
        // then coloured, so the ANSI escapes never count toward the column width. Quota
        // states carry their state colour; a dollar figure is bold cyan (Voli's emphasis
        // number); "no data" is dim.
        let (headline_plain, headline_code): (String, &str) = if ds.snapshot.error.is_some() {
            ("error".to_string(), color)
        } else if let Some(used) = window.as_ref().and_then(|wnd| wnd.used_percent) {
            let left = (100.0 - used).round() as i64;
            if left <= 0 {
                ("exhausted".to_string(), color)
            } else {
                (format!("{left}% left"), color)
            }
        } else if let Some(s) = ds.snapshot.spend_usd.filter(|v| v.is_finite()) {
            (format!("{} spent", usd(s)), pal::BOLD_CYAN)
        } else if ds.snapshot.credits_unit.as_deref() == Some("USD") {
            match ds.snapshot.credits.filter(|v| v.is_finite()) {
                Some(b) => (format!("{} bal", usd(b)), pal::BOLD_CYAN),
                None => ("no data".to_string(), pal::DIM),
            }
        } else {
            ("no data".to_string(), pal::DIM)
        };
        let headline = paint(color_on, headline_code, &format!("{:<14}", headline_plain));

        // Detail: the error message, or reset countdown + plan. Prefixed with an em dash
        // (Voli's separator before detail) only when there is something to say.
        let detail_plain = if let Some(err) = &ds.snapshot.error {
            truncate(err, 52)
        } else {
            let mut parts = Vec::new();
            if let Some(at) = window.as_ref().and_then(|wnd| wnd.resets_at) {
                parts.push(format!("resets in {}", countdown(at, now)));
            }
            if let Some(plan) = &ds.snapshot.plan {
                parts.push(plan.clone());
            }
            parts.join(" · ")
        };
        let detail = if detail_plain.is_empty() {
            String::new()
        } else {
            format!(
                "  {}  {}",
                paint(color_on, pal::DIM, "—"),
                paint(color_on, pal::DIM, &detail_plain)
            )
        };

        writeln!(
            w,
            "  {glyph}  {}  {headline}{detail}",
            paint(color_on, pal::BOLD, &format!("{:<name_w$}", name)),
        )
        .map_err(|e| e.to_string())?;
    }

    // Cost summary, only when something reports USD.
    let (mut spend, mut balance) = (0.0f64, 0.0f64);
    let (mut has_spend, mut has_balance) = (false, false);
    for ds in snapshots {
        if let Some(s) = ds.snapshot.spend_usd.filter(|v| v.is_finite()) {
            spend += s;
            has_spend = true;
        }
        if ds.snapshot.credits_unit.as_deref() == Some("USD") {
            if let Some(b) = ds.snapshot.credits.filter(|v| v.is_finite()) {
                balance += b;
                has_balance = true;
            }
        }
    }
    if has_spend || has_balance {
        let mut parts = Vec::new();
        if has_spend {
            parts.push(format!(
                "{} {}",
                paint(color_on, pal::BOLD_CYAN, &usd(spend)),
                paint(color_on, pal::DIM, "spent")
            ));
        }
        if has_balance {
            parts.push(format!(
                "{} {}",
                paint(color_on, pal::BOLD_CYAN, &usd(balance)),
                paint(color_on, pal::DIM, "balance")
            ));
        }
        let sep = format!("  {}  ", paint(color_on, pal::DIM, "·"));
        writeln!(w)
            .and_then(|_| writeln!(w, "  {}", parts.join(&sep)))
            .map_err(|e| e.to_string())?;
    }

    writeln!(w).map_err(|e| e.to_string())?;
    Ok(())
}

/// "$12.34", two places.
fn usd(value: f64) -> String {
    format!("${value:.2}")
}

/// Keep a long error message to one line in the status column.
fn truncate(s: &str, max: usize) -> String {
    let first = s.lines().next().unwrap_or(s);
    if first.chars().count() <= max {
        first.to_string()
    } else {
        let mut t: String = first.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}
