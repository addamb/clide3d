//! Tablet pen pressure on macOS (opt-in, docs/SCULPT_PLAN.md Q4): winit
//! and egui pass no pen pressure on, so an NSEvent local monitor watches
//! the window's tablet events and keeps the latest pressure, which the
//! brush tools read for each sample (`brush::Pen`). The monitor passes
//! every event on unchanged. It is installed only on the main thread
//! (never in a headless editor) and only when the setting is on; other
//! platforms have none (their pens arrive as touches with a force).

/// The latest pen pressure, when a tablet event came in just now and the
/// monitor is on.
pub fn pressure() -> Option<f32> {
    imp::pressure()
}

/// Turns the monitor on or off (Err when it cannot run here).
pub fn set_enabled(on: bool) -> Result<(), String> {
    imp::set_enabled(on)
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
    use std::sync::OnceLock;
    use std::time::Instant;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventSubtype, NSEventType};

    /// How long a tablet event's pressure stays the pen's.
    const FRESH_MS: u64 = 150;

    static ON: AtomicBool = AtomicBool::new(false);
    static PRESSURE: AtomicU32 = AtomicU32::new(0);
    /// When it came (ms since `clock`; 0: never).
    static STAMP: AtomicU64 = AtomicU64::new(0);

    fn clock() -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
    }

    thread_local! {
        /// The installed monitor (main thread only).
        static MONITOR: RefCell<Option<Retained<AnyObject>>> = const { RefCell::new(None) };
    }

    pub fn pressure() -> Option<f32> {
        if !ON.load(Ordering::Relaxed) {
            return None;
        }
        let at = STAMP.load(Ordering::Relaxed);
        (at != 0 && clock().saturating_sub(at) <= FRESH_MS).then(|| f32::from_bits(PRESSURE.load(Ordering::Relaxed)))
    }

    pub fn set_enabled(on: bool) -> Result<(), String> {
        let Some(_main) = MainThreadMarker::new() else {
            return Err("the tablet monitor runs on the window's thread".into());
        };
        ON.store(on, Ordering::Relaxed);
        MONITOR.with(|m| {
            let mut m = m.borrow_mut();
            match (on, m.is_some()) {
                (true, false) => {
                    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
                        // SAFETY: AppKit hands the monitor a live event for
                        // the duration of the call.
                        let e = unsafe { event.as_ref() };
                        let kind = e.r#type();
                        let tablet = kind == NSEventType::TabletPoint || (matches!(kind, NSEventType::LeftMouseDown | NSEventType::LeftMouseDragged) && e.subtype() == NSEventSubtype::TabletPoint);
                        if tablet {
                            PRESSURE.store(e.pressure().clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
                            STAMP.store(clock(), Ordering::Relaxed);
                        }
                        // Passed on unchanged.
                        event.as_ptr()
                    });
                    let mask = NSEventMask::TabletPoint | NSEventMask::LeftMouseDown | NSEventMask::LeftMouseDragged;
                    // SAFETY: the handler returns the event it was given.
                    *m = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) };
                    if m.is_none() {
                        ON.store(false, Ordering::Relaxed);
                        return Err("AppKit refused the tablet monitor".to_string());
                    }
                }
                (false, true) => {
                    if let Some(monitor) = m.take() {
                        // SAFETY: the object `addLocalMonitor...` returned.
                        unsafe { NSEvent::removeMonitor(&monitor) };
                    }
                    STAMP.store(0, Ordering::Relaxed);
                }
                _ => {}
            }
            Ok(())
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn pressure() -> Option<f32> {
        None
    }

    pub fn set_enabled(on: bool) -> Result<(), String> {
        if on { Err("the tablet monitor is for macOS; pens elsewhere press through touch force".into()) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    /// Compiled on every platform; off the main thread (tests) it never
    /// installs, and with it off there is no pressure.
    #[test]
    fn the_monitor_is_off_until_turned_on_on_the_main_thread() {
        assert_eq!(super::pressure(), None);
        assert!(super::set_enabled(true).is_err());
        assert_eq!(super::pressure(), None);
    }
}
