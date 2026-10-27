//! Native macOS window chrome. Everything here is a no-op on other platforms.
//!
//! - [`style_window`]: transparent full-size title bar with the traffic lights inset by an
//!   empty compact unified `NSToolbar`, plus an `NSVisualEffectView` (sidebar material) behind the
//!   app's content view so translucent regions show system vibrancy.
//! - [`chrome_metrics`]: the live height of that title bar region and the width taken by
//!   the traffic lights, so app content never sits under the system's drag region.
//! - [`accent_color`]: the user's system accent colour (`NSColor.controlAccentColor`).
//! - [`updater`]: update checks and installs through Sparkle.

pub mod updater;

use std::ffi::c_void;

/// Height of the unified title bar/toolbar region the app should leave for the system,
/// and the horizontal space taken by the traffic lights, in points.
pub const TITLEBAR_HEIGHT: f32 = 38.0;
pub const TRAFFIC_LIGHTS_WIDTH: f32 = 76.0;

/// Measured title bar geometry, in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Chrome {
    /// Height of the system title bar/toolbar strip over the content (0 in full screen).
    pub titlebar_height: f32,
    /// Distance from the window's left edge to just past the traffic lights.
    pub traffic_lights_width: f32,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::c_void;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSColor, NSColorSpace, NSToolbar, NSView, NSVisualEffectBlendingMode,
        NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowButton, NSWindowOrderingMode,
        NSWindowTitleVisibility, NSWindowToolbarStyle, NSWorkspace,
    };
    use objc2_foundation::NSString;

    /// # Safety
    /// `ns_view` must be a valid `NSView*` attached to a window, and this must run on the
    /// main thread (eframe calls the app's `ui` on the main thread on macOS).
    pub unsafe fn style_window(ns_view: *mut c_void, vibrancy: bool) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        if ns_view.is_null() {
            return false;
        }
        // SAFETY: caller guarantees a live NSView on the main thread.
        let view: &NSView = unsafe { &*(ns_view as *const NSView) };
        let Some(window) = view.window() else { return false };
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setTitlebarAppearsTransparent(true);
        let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &NSString::from_str("terminal-rs.toolbar"));
        window.setToolbar(Some(&toolbar));
        // Compact unified toolbar: one slim row shared with the traffic lights.
        window.setToolbarStyle(NSWindowToolbarStyle::UnifiedCompact);
        if vibrancy {
            let Some(content) = window.contentView() else { return true };
            let frame = content.frame();
            let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
            effect.setMaterial(NSVisualEffectMaterial::Sidebar);
            effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            effect.setState(NSVisualEffectState::FollowsWindowActiveState);
            effect.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            // Behind the content view (which hosts the Metal layer), so translucent
            // pixels the app draws reveal the blurred desktop/wallpaper.
            match unsafe { content.superview() } {
                Some(parent) => {
                    parent.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, Some(&content))
                }
                None => content.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, None),
            }
            window.setOpaque(false);
            window.setBackgroundColor(Some(&NSColor::clearColor()));
        }
        true
    }

    /// # Safety
    /// As for [`style_window`].
    pub unsafe fn chrome_metrics(ns_view: *mut c_void) -> Option<super::Chrome> {
        MainThreadMarker::new()?;
        if ns_view.is_null() {
            return None;
        }
        // SAFETY: caller guarantees a live NSView on the main thread.
        let view: &NSView = unsafe { &*(ns_view as *const NSView) };
        let window = view.window()?;
        // The content layout rect excludes the title bar/toolbar strip that overlaps a
        // full-size content view; the difference is the region the system owns.
        let titlebar = (window.frame().size.height - window.contentLayoutRect().size.height).max(0.0) as f32;
        let lights = window
            .standardWindowButton(NSWindowButton::ZoomButton)
            .map(|b| {
                let f = b.frame();
                (f.origin.x + f.size.width) as f32 + 12.0
            })
            .unwrap_or(0.0);
        Some(super::Chrome { titlebar_height: titlebar, traffic_lights_width: lights })
    }

    // N2: hide/minimize/occlusion must stop external-source schedulers, without a repaint timer.
    pub unsafe fn window_visible(ns_view: *mut c_void) -> Option<bool> {
        MainThreadMarker::new()?;
        if ns_view.is_null() {
            return None;
        }
        // SAFETY: caller guarantees a live main-thread NSView.
        let view: &NSView = unsafe { &*(ns_view as *const NSView) };
        let window = view.window()?;
        Some(
            window.isVisible()
                && !window.isMiniaturized()
                && window.occlusionState().contains(objc2_app_kit::NSWindowOcclusionState::Visible),
        )
    }

    pub fn accent_color() -> Option<[u8; 3]> {
        MainThreadMarker::new()?;
        let c = NSColor::controlAccentColor();
        let srgb = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
        let to = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        Some([to(srgb.redComponent()), to(srgb.greenComponent()), to(srgb.blueComponent())])
    }

    pub fn reduce_motion() -> bool {
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }
}

// N2
/// Whether the owning window is visible, non-minimized and unoccluded. Unsupported elsewhere.
///
/// # Safety
/// On macOS, `ns_view` must be a live `NSView*` on the main thread.
pub unsafe fn window_visible(ns_view: *mut c_void) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        unsafe { imp::window_visible(ns_view) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ns_view;
        None
    }
}

/// Apply native chrome to the window owning `ns_view`. Returns whether it was applied.
///
/// # Safety
/// On macOS, `ns_view` must be a valid `NSView*` and the call must be on the main thread.
pub unsafe fn style_window(ns_view: *mut c_void, vibrancy: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: forwarded caller contract.
        unsafe { imp::style_window(ns_view, vibrancy) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (ns_view, vibrancy);
        false
    }
}

/// Live title bar geometry of the window owning `ns_view` (macOS only).
///
/// # Safety
/// On macOS, `ns_view` must be a valid `NSView*` and the call must be on the main thread.
pub unsafe fn chrome_metrics(ns_view: *mut c_void) -> Option<Chrome> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: forwarded caller contract.
        unsafe { imp::chrome_metrics(ns_view) }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ns_view;
        None
    }
}

/// The system accent colour as sRGB, if the platform has one.
pub fn accent_color() -> Option<[u8; 3]> {
    #[cfg(target_os = "macos")]
    {
        imp::accent_color()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Whether the user turned on Reduce Motion in the system accessibility settings.
pub fn reduce_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        imp::reduce_motion()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Local timezone abbreviation at this instant (including daylight saving time).
/// Chrono's `Local` retains only an offset, so `%Z` cannot supply this name.
pub fn local_timezone_abbreviation(timestamp: i64) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSDate, NSTimeZone};
        let date = NSDate::dateWithTimeIntervalSince1970(timestamp as f64);
        NSTimeZone::localTimeZone().abbreviationForDate(&date).map(|name| name.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = timestamp;
        None
    }
}

pub fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn timezone_abbreviations_follow_the_dates_daylight_saving_rules() {
        use objc2_foundation::{NSDate, NSString, NSTimeZone};
        let zone = NSTimeZone::timeZoneWithName(&NSString::from_str("America/New_York")).unwrap();
        for (timestamp, expected) in [(1_792_713_600, "EDT"), (1_767_225_600, "EST")] {
            let date = NSDate::dateWithTimeIntervalSince1970(timestamp as f64);
            assert_eq!(zone.abbreviationForDate(&date).unwrap().to_string(), expected);
        }
        assert!(super::local_timezone_abbreviation(1_792_713_600).is_some());
    }

    #[test]
    fn stubs_are_safe_off_macos() {
        if !super::is_macos() {
            assert!(super::accent_color().is_none());
            assert!(!unsafe { super::style_window(std::ptr::null_mut(), true) });
            assert!(unsafe { super::chrome_metrics(std::ptr::null_mut()) }.is_none());
        }
    }
}
