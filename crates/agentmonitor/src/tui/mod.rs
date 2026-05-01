pub mod dashboard;
pub mod detail;
pub mod help;
pub mod process;
pub mod render;
pub mod sessions;
pub mod settings;
pub mod stats;
pub mod theme;
pub mod viewer;
pub mod widgets;

// Responsive layout thresholds. Centralized so all three tabs degrade
// at consistent widths/heights — change one knob, the whole UI follows.
//
// Picked from observed breakage points: a list row needs ~70 cols once
// status+agent+time+id+tokens+cwd are laid out; the dashboard's middle
// row needs ~70 cols before the activity bars get visually crammed
// against an over-narrow projects column; the dashboard's tokens strip
// adds 4+N rows of essentially "advanced" data that should yield to
// the primary panels when the terminal is short.

/// Below this width, Sessions falls back to a single-pane list (detail
/// hides; press `o`/Enter to inspect via the full-screen Viewer).
pub const SESSIONS_TWO_PANE_MIN_WIDTH: u16 = 110;

/// Below this width, Dashboard's middle row stacks Activity over Top
/// Projects vertically instead of side-by-side.
pub const DASHBOARD_HSTACK_MIN_WIDTH: u16 = 70;

/// Below this height, Dashboard drops the Tokens-by-agent strip from
/// the bottom — its data is partially mirrored in the Overview row.
pub const DASHBOARD_TOKENS_STRIP_MIN_HEIGHT: u16 = 28;
