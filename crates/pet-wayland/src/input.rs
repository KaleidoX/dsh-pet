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

use egui::{Event, Modifiers, Pos2};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym, Modifiers as SeatModifiers};
use smithay_client_toolkit::seat::pointer::{CursorIcon, PointerEvent, PointerEventKind};

/// `BTN_LEFT`, from `linux/input-event-codes.h`. The protocol only numbers
/// buttons, so the constant has to come from the kernel headers.
pub(crate) const BTN_LEFT: u32 = 0x110;

/// Pushes the egui events implied by one pointer frame and tracks the button.
///
/// A frame can mix kinds: a drag that ends outside the surface arrives as
/// `Release` + `Leave`, and a motion plus a press in the same frame is normal
/// for a fast click. They are translated in order, so egui sees the same
/// sequence the compositor sent.
pub(crate) fn translate_frame(events: &[PointerEvent], modifiers: Modifiers, down: &mut bool, out: &mut Vec<Event>) {
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
            // `Leave` can arrive with the release in the same frame; egui drops
            // its pointer position, and the core falls back to the press start.
            PointerEventKind::Leave { .. } => out.push(Event::PointerGone),
            _ => {}
        }
    }
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
