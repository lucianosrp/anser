//! The Downloads watcher, web build: a browser tab has no downloads folder
//! to look in, so there is nothing to suggest and no setting. Same public
//! surface as `downloads.rs` (the app wires both identically).

use std::path::PathBuf;

use gpui_kit::*;

pub enum DownloadsEvent {
    ImportAll(Vec<PathBuf>),
    Review(Vec<PathBuf>),
}

pub struct Downloads;

impl EventEmitter<DownloadsEvent> for Downloads {}

impl Downloads {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Downloads
    }
}

impl Render for Downloads {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub fn settings_card(_entity: &Entity<Downloads>, _cx: &App) -> Option<AnyElement> {
    None
}
