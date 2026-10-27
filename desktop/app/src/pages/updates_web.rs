//! Updates, web build: a browser tab is always the latest deploy, so there
//! is nothing to update. Same public surface as `updates.rs`.

use gpui_kit::*;

pub struct Updates;

impl Updates {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Updates
    }

    pub fn start(&mut self, _cx: &mut Context<Self>) {}
}

pub fn settings_card(_entity: &Entity<Updates>, _cx: &App) -> Option<AnyElement> {
    None
}
