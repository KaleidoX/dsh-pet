//! Hover tooltip: upstream `tooltipPlacement` plus the `TOOLTIP_CSS` look.
//!
//! Coordinates are logical points, which is also what the upstream CSS numbers
//! are in: the pet is always laid out at the 462-point reference width, so every
//! `* size` expression in the stylesheet is a constant here.

use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, FontId, Pos2, Rect, Shadow, Stroke, StrokeKind, TextFormat, Vec2};

/// Gap between the cursor and the box (`TOOLTIP_GAP_RATIO` * size).
pub const GAP: f32 = 0.022 * pet_config::PET_REF_WIDTH;

/// Box metrics at the reference size, straight from `TOOLTIP_CSS`.
const MAX_WIDTH: f32 = 0.8 * pet_config::PET_REF_WIDTH;
const PAD_X: f32 = 0.021 * pet_config::PET_REF_WIDTH;
const PAD_Y: f32 = 0.010 * pet_config::PET_REF_WIDTH;
const RADIUS: f32 = 0.016 * pet_config::PET_REF_WIDTH;
const SHADOW_DY: f32 = 0.004 * pet_config::PET_REF_WIDTH;
const SHADOW_BLUR: f32 = 0.013 * pet_config::PET_REF_WIDTH;
/// `line-height: 1.4`.
const LINE_HEIGHT: f32 = 1.4;

const BG: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 245);
const TEXT: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x1f);
const BORDER: Color32 = Color32::from_black_alpha(31);
const SHADOW: Color32 = Color32::from_black_alpha(46);

/// Where to put the box so it hugs `cursor` while staying inside `bounds`.
///
/// The cursor's bottom-right is preferred; a box that would leave `bounds` on
/// the right flips to the left of the cursor, and one that would leave it at
/// the bottom flips above. A box wider than `bounds` is pinned to its left edge,
/// which is what upstream's `hi < lo` clamp does.
pub fn tooltip_placement(cursor: (f32, f32), tip: (f32, f32), bounds: Rect, gap: f32) -> (f32, f32) {
    let clamp = |v: f32, lo: f32, hi: f32| if hi < lo { lo } else { v.clamp(lo, hi) };
    let right_max = bounds.right();
    let bottom_max = bounds.bottom();

    let mut x = cursor.0 + gap;
    if x + tip.0 > right_max {
        x = cursor.0 - gap - tip.0;
    }
    x = clamp(x, bounds.left(), right_max - tip.0);

    let mut y = cursor.1 + gap;
    if y + tip.1 > bottom_max {
        y = cursor.1 - gap - tip.1;
    }
    y = clamp(y, bounds.top(), bottom_max - tip.1);
    (x, y)
}

/// Draws the tooltip with its top-left corner placed by [`tooltip_placement`].
///
/// `font_px` is the configured tooltip font size (`Font::tooltip_size`), which
/// upstream injects as `--dsh-pet-font-tip`; the box grows around it.
pub fn ui(ui: &egui::Ui, screen: Rect, anchor: (f32, f32), text: &str, font_px: f32) {
    let font = FontId::proportional(font_px);
    let line_height = font_px * LINE_HEIGHT;
    let mut job =
        LayoutJob::single_section(text.to_owned(), TextFormat { font_id: font, line_height: Some(line_height), color: TEXT, ..Default::default() });
    // `white-space:nowrap; overflow:hidden; text-overflow:ellipsis`.
    job.wrap = TextWrapping { max_width: MAX_WIDTH, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };

    let painter = ui.painter();
    let galley = painter.layout_job(job);
    // The 1 px CSS border on each side is part of the box.
    let size = Vec2::new(galley.size().x + 2.0 * PAD_X + 2.0, line_height + 2.0 * PAD_Y + 2.0);
    let (x, y) = tooltip_placement(anchor, (size.x, size.y), screen, GAP);
    let rect = Rect::from_min_size(Pos2::new(x, y), size);
    let radius = egui::CornerRadius::same(RADIUS.round() as u8);

    painter.add(Shadow { offset: [0, SHADOW_DY.round() as i8], blur: SHADOW_BLUR.round() as u8, spread: 0, color: SHADOW }.as_shape(rect, radius));
    painter.rect_filled(rect, radius, BG);
    painter.rect_stroke(rect, radius, Stroke::new(1.0, BORDER), StrokeKind::Inside);
    painter.galley(Pos2::new(rect.left() + 1.0 + PAD_X, rect.top() + 1.0 + PAD_Y), galley, TEXT);
}
