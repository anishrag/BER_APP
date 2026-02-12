// ui/events.rs
use crate::ui::pages::sketch::SketchEvent;

use crate::ui::pages::new_ber::{NewBerEvent, NewBerField};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Splash,
    Home,
    Sketch,
    NewBer,
    KeyDeets,
    MoreDeets,
}

#[derive(Debug)]
pub enum Field {
    NewBer(NewBerField),
}

#[derive(Debug)]
pub enum UiEvent {
    // navigation events (shared by all pages)
    NavigateTo(Page),
    SetField(Field),
    // page-specific events
    Sketch(SketchEvent),
    NewBer(NewBerEvent),
    // add more page events here as your app grows
}
