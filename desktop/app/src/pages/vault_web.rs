//! Web build stand-in for vault.rs: the browser keeps no copy on disk, so
//! there is nothing to encrypt or unlock.

use gpui_kit::*;

pub enum VaultEvent {
    Unlocked,
    Resync,
}

pub struct Vault;

impl EventEmitter<VaultEvent> for Vault {}

impl Vault {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Vault
    }
}

impl Vault {
    pub fn refresh(&mut self, _cx: &mut Context<Self>) {}
}

impl Render for Vault {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
