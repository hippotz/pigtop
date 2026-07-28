use std::time::Duration;

use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

use pigtop::rates::ProcessBandwidth;

/// A peak isn't worth calling out separately from the current rate unless it's noticeably
/// higher and old enough that it's clearly a past burst rather than this instant's reading.
const PEAK_NOTEWORTHY_RATIO: f64 = 1.05;
const PEAK_NOTEWORTHY_AGE: Duration = Duration::from_secs(2);

/// Only processes moving at least this much bandwidth (right now, or within PEAK_WINDOW) count
/// as a "bandwidth pig" worth surfacing — anything below this is just background chatter.
const PIG_THRESHOLD_BYTES_PER_SEC: f64 = 1024.0 * 1024.0;

/// Number of process rows in the dropdown. Fixed so the menu's item list never needs rebuilding
/// (see MenuBar's doc comment for why that matters).
const SLOT_COUNT: usize = 8;

pub const QUIT_MENU_ID: &str = "quit";

/// Owns the status item and a fixed pool of menu item "slots" that get their text swapped in
/// place every tick, rather than the menu being torn down and rebuilt. Replacing the NSMenu
/// object (via `TrayIcon::set_menu`) dismisses it if it's currently open on screen; mutating an
/// existing item's text via `MenuItem::set_text` updates it live without closing the dropdown.
/// Must only ever be touched from the main thread, like all NSStatusItem/NSMenu mutation.
pub struct MenuBar {
    tray: TrayIcon,
    slots: Vec<MenuItem>,
}

impl MenuBar {
    /// Must be called on the main thread, after the event loop has started running
    /// (tray-icon's own docs: creating it any earlier can misbehave around fullscreen apps).
    pub fn build() -> tray_icon::Result<Self> {
        let tray = TrayIconBuilder::new()
            .with_title("PigTop")
            .with_tooltip("PigTop — per-process bandwidth")
            .build()?;

        let menu = Menu::new();
        let mut slots = Vec::with_capacity(SLOT_COUNT);
        for _ in 0..SLOT_COUNT {
            let item = MenuItem::new("", false, None);
            menu.append(&item).expect("append menu slot");
            slots.push(item);
        }
        menu.append(&PredefinedMenuItem::separator()).expect("append separator");
        menu.append(&MenuItem::with_id(MenuId::new(QUIT_MENU_ID), "Quit PigTop", true, None))
            .expect("append quit menu item");
        tray.set_menu(Some(Box::new(menu)));

        Ok(Self { tray, slots })
    }

    /// Sets the title to the current top bandwidth consumer and updates each dropdown slot's
    /// text in place. `boosted` reflects the click-to-toggle fast-poll mode (see main.rs); it
    /// only changes the "⚡" prefix shown here, not anything poll-related in this file.
    pub fn update(&self, ranked: &[ProcessBandwidth], boosted: bool) {
        // Bandwidth pigs = processes moving at least PIG_THRESHOLD_BYTES_PER_SEC, either right
        // now or within the last PEAK_WINDOW (rates.rs already sorts by max(current, peak), so
        // this slice is in display order).
        let pigs: Vec<&ProcessBandwidth> = ranked
            .iter()
            .filter(|p| {
                p.total_rate() >= PIG_THRESHOLD_BYTES_PER_SEC
                    || p.peak_rate >= PIG_THRESHOLD_BYTES_PER_SEC
            })
            .collect();

        let live = ranked.iter().find(|p| p.total_rate() >= PIG_THRESHOLD_BYTES_PER_SEC);
        let title = match live {
            Some(top) => format!(
                "{} {} {}",
                direction_arrow(top.rx_rate, top.tx_rate),
                truncate(&top.name, 20),
                format_rate(top.total_rate())
            ),
            None => match pigs.first() {
                Some(top) => format!(
                    "recently: {} {} ({}s ago)",
                    truncate(&top.name, 20),
                    format_rate(top.peak_rate),
                    top.peak_age.as_secs()
                ),
                None => "🐷".to_string(),
            },
        };
        self.tray.set_title(Some(if boosted { format!("⚡ {title}") } else { title }));

        if pigs.is_empty() {
            self.slots[0].set_text("No bandwidth pigs (≥ 1 MB/s)");
            for slot in &self.slots[1..] {
                slot.set_text("");
            }
        } else {
            for (slot, p) in self
                .slots
                .iter()
                .zip(pigs.iter().map(Some).chain(std::iter::repeat(None)).take(SLOT_COUNT))
            {
                slot.set_text(p.map(|p| format_menu_line(p)).unwrap_or_default());
            }
        }
    }
}

fn format_menu_line(p: &ProcessBandwidth) -> String {
    let now = format!("down {}  up {}", format_rate(p.rx_rate), format_rate(p.tx_rate));
    let peak_is_noteworthy = p.peak_rate > p.total_rate() * PEAK_NOTEWORTHY_RATIO
        && p.peak_age > PEAK_NOTEWORTHY_AGE;

    if peak_is_noteworthy {
        format!(
            "{:<24} {}   peak {} ({}s ago)",
            truncate(&p.name, 24),
            now,
            format_rate(p.peak_rate),
            p.peak_age.as_secs()
        )
    } else {
        format!("{:<24} {}", truncate(&p.name, 24), now)
    }
}

/// Which direction dominates a process's current traffic, for the menu bar title's arrow.
fn direction_arrow(rx_rate: f64, tx_rate: f64) -> char {
    if tx_rate > rx_rate { '↑' } else { '↓' }
}

fn format_rate(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1024.0 * 1024.0 {
        format!("{:.1} MB/s", bytes_per_sec / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KB/s", bytes_per_sec / 1024.0)
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars.saturating_sub(1)).collect::<String>() + "…"
    }
}
