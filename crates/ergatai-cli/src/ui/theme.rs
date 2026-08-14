//! Centralized theme for the Ergatai TUI.
//!
//! Wraps `ratatui_interact::theme::Theme` and exposes semantic accessors
//! (`muted`, `accent`, `success`, `error`, `warning`, `diff_add_bg`, …) so
//! widget code never hard-codes `Color::*` values.
//!
//! The palette is cached in a process-wide [`LazyLock`] — `Theme::dark()`
//! allocates a `String` for its name, but [`ColorPalette`] is plain `Color`
//! values (Copy), so we cache only the palette and pay the allocation once.
//!
//! The diff-specific backgrounds (`diff_add_bg`, `diff_del_bg`) are pulled
//! from the theme rather than hard-coded, so swapping to `Theme::light()`
//! in the future will automatically re-skin diff rendering.

use std::sync::LazyLock;

use ratatui::style::Color;
use ratatui_interact::theme::{ColorPalette, Theme};

/// Cached color palette (dark by default). All fields are `Color` (Copy),
/// so accessors are zero-cost field reads after the one-time init.
static PALETTE: LazyLock<ColorPalette> = LazyLock::new(|| Theme::dark().palette);

/// Secondary / dim text (borders, hints, placeholders, timestamps).
///
/// Previously hard-coded as `Color::DarkGray` in ~25 places.
pub fn muted() -> Color {
    PALETTE.text_disabled
}

/// Accent / interactive highlight (tool names, selected items, session ids).
///
/// Previously hard-coded as `Color::Cyan`.
pub fn accent() -> Color {
    PALETTE.secondary
}

/// Success state (tool card "Ran" bullet, agent "Done", diff add foreground).
///
/// Previously hard-coded as `Color::Green`.
pub fn success() -> Color {
    PALETTE.success
}

/// Error state (tool card "Failed" bullet, agent "Error", diff del foreground,
/// error output).
///
/// Previously hard-coded as `Color::Red`.
pub fn error() -> Color {
    PALETTE.error
}

/// Warning / active state (tool card "Running", agent "Busy", spinner).
///
/// Previously hard-coded as `Color::Yellow`.
pub fn warning() -> Color {
    PALETTE.warning
}

/// Thinking-phase accent (spinner, thinking-block headers).
///
/// Previously hard-coded as `Color::Magenta`. Mapped to `info` (Cyan in dark
/// theme) because the palette has no dedicated "thinking" role; `info` is
/// the closest semantic match for "auxiliary phase indicator".
pub fn thinking() -> Color {
    PALETTE.info
}

/// Background tint for user chat bubbles.
///
/// Preserved from the previous hard-coded value; deliberately not themed
/// because user bubbles need to read as distinct from assistant content even
/// across theme swaps.
pub fn user_msg_bg() -> Color {
    Color::Rgb(31, 31, 31)
}

/// Diff add-line background.
pub fn diff_add_bg() -> Color {
    PALETTE.diff_add_bg
}

/// Diff delete-line background.
pub fn diff_del_bg() -> Color {
    PALETTE.diff_del_bg
}

/// Diff add-line foreground.
pub fn diff_add_fg() -> Color {
    PALETTE.diff_add_fg
}

/// Diff delete-line foreground.
pub fn diff_del_fg() -> Color {
    PALETTE.diff_del_fg
}

/// Default foreground (plain text, no special meaning).
///
/// We prefer `Reset` so the terminal's default fg is respected.
pub fn default_fg() -> Color {
    Color::Reset
}

/// Dim body text (slightly brighter than `muted`).
///
/// Maps to `text_dim` (Gray in dark theme). Use for text that should read
/// as "content" rather than "chrome" — e.g., tool output, json previews.
pub fn dim() -> Color {
    PALETTE.text_dim
}

/// Highlighted foreground (text on top of `highlight_bg`).
pub fn highlight_fg() -> Color {
    PALETTE.highlight_fg
}
