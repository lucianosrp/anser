//! The owner's avatar, drawn wherever the app shows who it belongs to: the
//! sidebar brand (in place of the "F" tile, same size and rounding), the
//! collapsed rail, the phone header (top right), Settings next to the name.
//! The picture when there is one (the replica's avatar.png on the desktop,
//! so it works offline; fetched with the data in the browser), else the
//! initials on their colour. A tooltip names the owner.
//!
//! A global, filled from every bundle's profile (`refresh`) and right away
//! when Settings changes it (`set_spec`, `set_picture`).

use std::sync::Arc;

use finance_core::avatar::{Color, Spec};
use gpui_kit::component::{tooltip::Tooltip, ActiveTheme as _};
use gpui_kit::*;

use crate::zoom::px;

#[derive(Default)]
pub struct Avatar {
    pub spec: Spec,
    picture: Option<Arc<Image>>,
    /// a profile has been seen (before that the brand shows the app's "F")
    known: bool,
}

impl Global for Avatar {}

fn image_of(png: Vec<u8>) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Png, png))
}

/// From a bundle's (or /api/profile's) profile, and the cached picture.
pub fn refresh(profile: &serde_json::Value, cx: &mut App) {
    let spec = Spec::from_profile(profile);
    let picture = if spec.has_picture() { finance_core::cache::avatar_bytes().map(image_of) } else { None };
    cx.set_global(Avatar { known: !profile.is_null(), spec, picture });
}

/// The profile changed (Settings, the wizard): the new spec, keeping the picture.
pub fn set_spec(profile: &serde_json::Value, cx: &mut App) {
    let picture = cx.try_global::<Avatar>().and_then(|a| a.picture.clone());
    let spec = Spec::from_profile(profile);
    let picture = if spec.has_picture() { picture } else { None };
    cx.set_global(Avatar { known: true, spec, picture });
}

/// A new picture arrived (an upload, a Gravatar); None = back to initials.
/// Kept in the replica / the page too, so the next start has it.
pub fn set_picture(png: Option<Vec<u8>>, cx: &mut App) {
    let _ = finance_core::cache::set_avatar(png.as_deref());
    let mut a = cx.try_global::<Avatar>().map(|a| Avatar { spec: a.spec.clone(), picture: None, known: a.known }).unwrap_or_default();
    a.picture = png.map(image_of);
    cx.set_global(a);
}

/// The picture in use, if any.
pub fn picture(cx: &App) -> Option<Arc<Image>> {
    cx.try_global::<Avatar>().and_then(|a| a.picture.clone())
}

/// The initials' background, resolved against the theme.
pub fn background(c: Color, cx: &App) -> Hsla {
    let t = cx.theme();
    match c {
        Color::Chart(1) => t.chart_1,
        Color::Chart(2) => t.chart_2,
        Color::Chart(3) => t.chart_3,
        Color::Chart(4) => t.chart_4,
        Color::Chart(_) => t.chart_5,
        Color::Hex(h) => Hsla::from(rgb(h)),
    }
}

/// Black or white, whichever reads on `bg`.
fn ink(bg: Hsla) -> Hsla {
    if bg.l > 0.62 { hsla(0., 0., 0.1, 1.) } else { hsla(0., 0., 1., 1.) }
}

/// One avatar of `size` px, corner `radius` px, from `spec` (and `picture`).
pub fn draw(spec: &Spec, picture: Option<Arc<Image>>, size: f32, radius: f32, cx: &App) -> Div {
    let tile = div().size(px(size)).flex_shrink_0().rounded(px(radius)).overflow_hidden();
    if let Some(p) = picture {
        return tile.child(img(p).size(px(size)).rounded(px(radius)).object_fit(ObjectFit::Cover));
    }
    let bg = background(spec.background(), cx);
    let letters = spec.letters();
    tile.bg(bg)
        .flex()
        .items_center()
        .justify_center()
        .text_color(ink(bg))
        .text_size(px((size * 0.46).round()))
        .font_weight(FontWeight::BOLD)
        .child(if letters.is_empty() { "?".to_string() } else { letters })
}

/// The owner's avatar with a tooltip naming them. Before any profile is
/// known (no data yet), the app's own "F" tile.
pub fn owner(id: &'static str, size: f32, radius: f32, cx: &App) -> Stateful<Div> {
    let a = cx.try_global::<Avatar>();
    let known = a.is_some_and(|a| a.known && (!a.spec.letters().is_empty() || a.picture.is_some()));
    if !known {
        let t = cx.theme();
        return div()
            .id(id)
            .size(px(size))
            .flex_shrink_0()
            .rounded(px(radius))
            .bg(t.primary)
            .flex()
            .items_center()
            .justify_center()
            .text_color(t.primary_foreground)
            .text_size(px((size * 0.54).round()))
            .font_weight(FontWeight::BOLD)
            .child("F");
    }
    let a = a.expect("known");
    let name = if a.spec.name.is_empty() { "You".to_string() } else { a.spec.name.clone() };
    draw(&a.spec, a.picture.clone(), size, radius, cx)
        .id(id)
        .tooltip(move |w, cx| Tooltip::new(name.clone()).build(w, cx))
}
