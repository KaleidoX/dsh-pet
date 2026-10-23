//! Wayland input → egui events.
//!
//! `PetCore` reads input the same way on every host: from egui's input state.
//! This module turns the protocol's pointer and keyboard callbacks into the
//! `egui::Event` stream.
//!
//! Positions stay in surface-local points, exactly as under eframe; the core
//! multiplies by `ppp` when it needs canvas pixels. Wayland never reports a
//! pointer position outside a surface, but it does keep delivering motion to
//! the grabbed surface while the button is down, which is the only case the
//! drag needs — so no global pointer query is involved.

use egui::{Event, Modifiers, Pos2, Vec2};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym, Modifiers as SeatModifiers};
use smithay_client_toolkit::seat::pointer::{AxisScroll, CursorIcon, PointerEvent, PointerEventKind};
/// `BTN_LEFT`, from `linux/input-event-codes.h`. The protocol only numbers
/// buttons, so the constant has to come from the kernel headers.
pub(crate) const BTN_LEFT: u32 = 0x110;
/// `BTN_RIGHT`; opens the context menu.
pub(crate) const BTN_RIGHT: u32 = 0x111;
/// How many overlay points one discrete wheel step scrolls.
///
/// egui's own `MouseWheelUnit::Line` uses its `line_scroll_speed` (40 points);
/// the Wayland translator emits points directly, so the same figure is applied
/// here to keep the menu scrolling at the rate a winit host sees.
const LINE_HEIGHT_POINTS: f32 = 40.0;

/// Pushes the egui events implied by one pointer frame and tracks the button.
///
/// A frame can mix kinds: a drag that ends outside the surface arrives as
/// `Release` + `Leave`, and a motion plus a press in the same frame is normal
/// for a fast click. They are translated in order, so egui sees the same
/// sequence the compositor sent.
///
/// Scroll (`Axis`) is translated when `scroll` is set for the shared scene.
/// The sign is flipped because Wayland reports how far the
/// content moves, while winit (and therefore eframe, the reference host)
/// reports the inverse and egui follows winit.
pub(crate) fn translate_frame(events: &[PointerEvent], modifiers: Modifiers, down: &mut bool, scroll: bool, out: &mut Vec<Event>) {
    for event in events {
        let pos = Pos2::new(event.position.0 as f32, event.position.1 as f32);
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => out.push(Event::PointerMoved(pos)),
            PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                *down = true;
                out.push(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers });
            }
            PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                *down = false;
                out.push(Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers });
            }
            // egui tracks the secondary button too; the core reads its press
            // edge from the input state, exactly as under eframe.
            PointerEventKind::Press { button, .. } if button == BTN_RIGHT => {
                out.push(Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, modifiers });
            }
            PointerEventKind::Release { button, .. } if button == BTN_RIGHT => {
                out.push(Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: false, modifiers });
            }
            // `Leave` can arrive with the release in the same frame; egui drops
            // its pointer position, and the core falls back to the press start.
            PointerEventKind::Leave { .. } => out.push(Event::PointerGone),
            PointerEventKind::Axis { horizontal, vertical, .. } if scroll => {
                let delta = scroll_delta(horizontal, vertical);
                if delta != Vec2::ZERO {
                    out.push(Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta,
                        phase: if horizontal.stop || vertical.stop { egui::TouchPhase::End } else { egui::TouchPhase::Move },
                        source: egui::MouseWheelSource::Unknown,
                        modifiers,
                    });
                }
            }
            _ => {}
        }
    }
}

/// The scroll of one frame in overlay points.
///
/// Discrete steps are preferred when present (as winit does), because the
/// continuous value is only a hint for smooth devices; otherwise the pixel
/// value, already in surface-local points, is used as-is.
fn scroll_delta(horizontal: AxisScroll, vertical: AxisScroll) -> Vec2 {
    let discrete = (horizontal.discrete, vertical.discrete);
    let value120 = (horizontal.value120, vertical.value120);
    let (x, y) = if discrete != (0, 0) {
        (-discrete.0 as f32, -discrete.1 as f32)
    } else if value120 != (0, 0) {
        (-value120.0 as f32 / 120.0, -value120.1 as f32 / 120.0)
    } else {
        (-horizontal.absolute as f32, -vertical.absolute as f32)
    };
    // Lines are converted the way egui does for `MouseWheelUnit::Line`; the
    // event cannot carry the unit itself without a second variant.
    let line = discrete != (0, 0) || value120 != (0, 0);
    Vec2::new(x, y) * if line { LINE_HEIGHT_POINTS } else { 1.0 }
}

/// egui's modifier state from the seat's. `command` is `ctrl` on Linux, which
/// is what `PetCore` checks for its quit shortcut.
pub(crate) fn egui_modifiers(state: SeatModifiers) -> Modifiers {
    Modifiers { alt: state.alt, ctrl: state.ctrl, shift: state.shift, mac_cmd: false, command: state.ctrl }
}

/// Whether this key event is the pet's quit shortcut.
///
/// sctk already translated the raw key code through the compositor's keymap, so
/// matching the keysym covers the active layout: the key that produces `q` is
/// the key the user sees as `Q`.
pub(crate) fn is_quit(event: &KeyEvent, modifiers: Modifiers) -> bool {
    modifiers.ctrl && event.keysym == Keysym::q
}

/// The Wayland cursor for an egui cursor, if the pet asks for one we map.
///
/// Only the two drag cursors are mapped: `PetCore` never requests anything
/// else, and an unmapped icon must leave the compositor's cursor alone rather
/// than snap to the default arrow.
pub(crate) fn cursor_icon(icon: egui::CursorIcon) -> Option<CursorIcon> {
    match icon {
        egui::CursorIcon::Grab => Some(CursorIcon::Grab),
        egui::CursorIcon::Grabbing => Some(CursorIcon::Grabbing),
        _ => None,
    }
}
