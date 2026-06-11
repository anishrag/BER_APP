// ui/events.rs
use crate::ui::pages::home::HomeEvent;
use crate::ui::pages::key_details::KeyDetailsEvent;
use crate::ui::pages::sketch::SketchEvent;

/// Top-level navigation state: the splash screen, the record-free home menu,
/// or the BER workspace (a loaded record + one of its three sections).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Splash,
    Home,
    Workspace(Section),
}

/// The three sections of the BER workspace, shown as tabs in the chrome.
/// Always all clickable — `next()` is only the Continue-button convenience
/// ordering, not a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    KeyDetails,
    Sketch,
    MoreDetails,
}

impl Section {
    /// The section the Continue button advances to (`None` on the last one).
    pub fn next(self) -> Option<Section> {
        match self {
            Section::KeyDetails => Some(Section::Sketch),
            Section::Sketch => Some(Section::MoreDetails),
            Section::MoreDetails => None,
        }
    }
}

#[derive(Debug)]
pub enum UiEvent {
    // navigation events (shared by all pages)
    NavigateTo(Page),
    /// Leave the workspace: flush pending edits, clear the loaded record,
    /// return Home.
    ExitBer,
    // page-specific events
    Sketch(SketchEvent),
    Home(HomeEvent),
    KeyDetails(KeyDetailsEvent),
}
