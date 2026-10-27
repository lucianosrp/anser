//! UI zoom (Ctrl +/-/0, Ctrl+scroll). gpui has no window-wide scale outside
//! tests, and the app sizes things in px, so this module's `px` shadows
//! gpui's in every app file (an explicit import beats the `gpui_kit::*`
//! glob): every px the app asks for is multiplied by the zoom. Text sized in
//! rems (the kit's text_sm & co.) follows through the theme's base font size,
//! which gpui-kit's Root turns into the window's rem every frame.

use std::sync::atomic::{AtomicU32, Ordering};

use gpui_kit::component::Theme;
use gpui_kit::{App, Pixels, Window};

pub const MIN: f32 = 0.6;
pub const MAX: f32 = 2.0;
pub const STEP: f32 = 0.1;
/// gpui's default rem
const REM: f32 = 16.;

/// zoom × 1000 (an atomic f32 stand-in; the UI thread is the only writer)
static ZOOM: AtomicU32 = AtomicU32::new(1000);

pub fn get() -> f32 {
    ZOOM.load(Ordering::Relaxed) as f32 / 1000.
}

/// Clamp, round to whole percents, store; returns the value in effect.
pub fn set(z: f32) -> f32 {
    let z = (z.clamp(MIN, MAX) * 100.).round() / 100.;
    ZOOM.store((z * 1000.).round() as u32, Ordering::Relaxed);
    z
}

/// Make rem-sized text follow the zoom.
pub fn apply(window: &mut Window, cx: &mut App) {
    Theme::global_mut(cx).font_size = gpui_kit::px(REM * get());
    window.refresh();
}

/// `px` at the current zoom.
#[inline]
pub fn px(v: f32) -> Pixels {
    gpui_kit::px(v * get())
}

#[cfg(test)]
mod tests {
    // not super::*: no gpui `test` attribute in scope
    use super::{set, MAX, MIN};

    #[test]
    fn clamps_and_rounds() {
        assert_eq!(set(1.234), 1.23);
        assert_eq!(set(9.), MAX);
        assert_eq!(set(0.1), MIN);
        set(1.);
    }
}
