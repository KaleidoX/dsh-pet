//! AppKit reads use the same top-left origin and window scale as winit.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSScreen};
use pet_config::HIT_BOX;
use pet_config::WorldArea;
use pet_overlay::SceneLayout;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use winit::window::Window;

#[derive(Clone, Copy)]
struct PointerArea {
    height: f64,
    scale: f64,
    origin: (i32, i32),
    layout: SceneLayout,
    menu: bool,
    pressed: bool,
}

impl PointerArea {
    fn contains(self, x: f32, y: f32) -> bool {
        let x = x - self.origin.0 as f32;
        let y = y - self.origin.1 as f32;
        if self.menu {
            x >= 0.0 && y >= 0.0 && x < self.layout.size.0 as f32 && y < self.layout.size.1 as f32
        } else {
            HIT_BOX.contains(x - self.layout.offset.0 as f32, y - self.layout.offset.1 as f32)
        }
    }
}

/// Input polling must not make a stationary 24 fps clip render at 60 fps.
/// AppKit's thread-safe NSEvent reads wake egui only when interaction changes.
pub struct PointerMonitor {
    area: Arc<Mutex<PointerArea>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PointerMonitor {
    pub fn new(ctx: egui::Context, window: &Window, layout: SceneLayout, canvas: (i32, i32)) -> std::io::Result<Self> {
        let area = Arc::new(Mutex::new(PointerArea {
            height: screen_height().unwrap_or(0.0),
            scale: window.scale_factor(),
            origin: layout.window_origin(canvas),
            layout,
            menu: false,
            pressed: false,
        }));
        let stopped = Arc::new(AtomicBool::new(false));
        let watched = area.clone();
        let stop = stopped.clone();
        let thread = thread::Builder::new().name("pet-pointer".to_owned()).spawn(move || {
            let mut previous = None;
            let mut was_inside = false;
            while !stop.load(Ordering::Relaxed) {
                let area = *watched.lock().unwrap_or_else(|error| error.into_inner());
                let current = objc2::rc::autoreleasepool(|_| pointer_at_scale(area.height, area.scale));
                let inside = area.contains(current.0 as f32, current.1 as f32);
                if inside != was_inside || previous != Some(current) && (inside || was_inside || area.pressed) {
                    ctx.request_repaint();
                }
                previous = Some(current);
                was_inside = inside;
                thread::sleep(Duration::from_millis(16));
            }
        })?;
        Ok(Self { area, stopped, thread: Some(thread) })
    }

    pub fn update(&self, window: &Window, layout: SceneLayout, canvas: (i32, i32), menu: bool, pressed: bool) {
        let mut area = self.area.lock().unwrap_or_else(|error| error.into_inner());
        area.scale = window.scale_factor();
        area.origin = layout.window_origin(canvas);
        area.layout = layout;
        area.menu = menu;
        area.pressed = pressed;
    }

    pub fn refresh_screen(&self) {
        if let Some(height) = screen_height() {
            self.area.lock().unwrap_or_else(|error| error.into_inner()).height = height;
        }
    }
}

impl Drop for PointerMonitor {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn screen_height() -> Option<f64> {
    Some(NSScreen::screens(MainThreadMarker::new()?).firstObject()?.frame().size.height)
}

fn pointer_at_scale(height: f64, scale: f64) -> (i32, i32, bool) {
    let point = NSEvent::mouseLocation();
    ((point.x * scale).round() as i32, ((height - point.y) * scale).round() as i32, NSEvent::pressedMouseButtons() & 1 != 0)
}

/// Global pointer state remains available when the window ignores mouse events.
pub fn pointer(window: &Window) -> Option<(i32, i32, bool)> {
    Some(pointer_at_scale(screen_height()?, window.scale_factor()))
}

/// Excludes the menu bar and Dock, in the current window's physical coordinates.
pub fn work_area(window: &Window) -> Option<WorldArea> {
    let screens = NSScreen::screens(MainThreadMarker::new()?);
    let height = screens.firstObject()?.frame().size.height;
    let monitor = window.current_monitor().or_else(|| window.primary_monitor())?;
    let origin = monitor.position().to_logical::<f64>(monitor.scale_factor());
    let screen = screens.iter().find(|screen| {
        let frame = screen.frame();
        (frame.origin.x - origin.x).abs() < 1.0 && (height - frame.origin.y - frame.size.height - origin.y).abs() < 1.0
    })?;
    let frame = screen.visibleFrame();
    let scale = window.scale_factor();
    let area = WorldArea {
        x: (frame.origin.x * scale).round() as i32,
        y: ((height - frame.origin.y - frame.size.height) * scale).round() as i32,
        width: (frame.size.width * scale).round() as u32,
        height: (frame.size.height * scale).round() as u32,
    };
    (area.width > 0 && area.height > 0).then_some(area)
}
