//! Click-score feedback: the particle burst and the white score card.
//!
//! Both are straight ports of `score-popup.ts`. Particles follow the closed-form
//! trajectory the upstream rAF loop writes into `transform`, so a dropped frame
//! never shifts them; the card pops in over [`SCORE_POPUP_CSS_DURATION`] and
//! disappears after [`SCORE_POPUP_SECS`].

use std::time::Instant;

use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, CornerRadius, FontId, Pos2, Rect, Shadow, Stroke, StrokeKind, TextFormat, Vec2};

/// How long the card stays on screen (`SCORE_POPUP_DURATION_MS`).
pub const SCORE_POPUP_SECS: f32 = 2.2;
/// Length of the pop-in animation (`dshPetScorePop .28s`).
pub const SCORE_POPUP_CSS_DURATION: f32 = 0.28;

/// Particle count per burst.
pub(crate) const BURST_COUNT: usize = 20;
/// Initial speed range, points per second.
pub(crate) const BURST_SPEED_MIN: f32 = 120.0;
pub(crate) const BURST_SPEED_MAX: f32 = 460.0;
/// Downward pull, points per second squared.
const BURST_GRAVITY: f32 = 700.0;
/// Lifetime range, seconds.
pub(crate) const BURST_LIFE_MIN: f32 = 0.5;
pub(crate) const BURST_LIFE_MAX: f32 = 0.9;
/// Radius range, points.
pub(crate) const BURST_RADIUS_MIN: f32 = 3.0;
pub(crate) const BURST_RADIUS_MAX: f32 = 7.0;
/// Upward bias added to the launch speed (`vy: … - 80`).
pub(crate) const BURST_RISE: f32 = 80.0;
/// Warm palette, in upstream order.
pub(crate) const BURST_COLORS: [Color32; 7] = [
    Color32::from_rgb(0xff, 0xb3, 0x00),
    Color32::from_rgb(0xff, 0x8f, 0x00),
    Color32::from_rgb(0xff, 0x70, 0x43),
    Color32::from_rgb(0xf4, 0x51, 0x1e),
    Color32::from_rgb(0xff, 0xc4, 0x00),
    Color32::from_rgb(0xff, 0xd5, 0x4f),
    Color32::from_rgb(0xef, 0x53, 0x50),
];

/// Card metrics straight from `SCORE_POPUP_CSS`.
const CARD_MIN_WIDTH: f32 = 120.0;
const CARD_PAD_X: f32 = 16.0;
const CARD_PAD_TOP: f32 = 8.0;
const CARD_PAD_BOTTOM: f32 = 9.0;
const CARD_RADIUS: f32 = 12.0;
const VAL_SIZE: f32 = 22.0;
const VAL_LINE_HEIGHT: f32 = 1.25;
const SUB_SIZE: f32 = 11.0;
const SUB_LINE_HEIGHT: f32 = 1.4;
const SUB_GAP: f32 = 2.0;
/// The card sits above the click point, kept [`CARD_EDGE`] off the window edge.
const CARD_EDGE: f32 = 4.0;
const CARD_LIFT: f32 = 14.0;
/// Offset of the fake-bold second pass, in points.
const BOLD_OFFSET: f32 = 0.55;

const CARD_BG: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 247);
const CARD_BORDER: Color32 = Color32::from_rgba_unmultiplied_const(255, 179, 0, 89);
const CARD_SHADOW: Color32 = Color32::from_black_alpha(56);
const VAL_COLOR: Color32 = Color32::from_rgb(0xff, 0x8f, 0x00);
const SUB_COLOR: Color32 = Color32::from_rgba_unmultiplied_const(43, 43, 43, 153);

/// One particle of the burst. `born` shares the clock with the card.
///
/// The velocity is a per-particle offset from the anchor, so the whole burst
/// can be moved with the anchor as the overlay window follows the pet.
pub(crate) struct Particle {
    pub(crate) vx: f32,
    pub(crate) vy: f32,
    pub(crate) born: Instant,
    pub(crate) life: f32,
    pub(crate) radius: f32,
    pub(crate) color: Color32,
}

/// The click that spawned the card and its burst. `anchor` is a screen position
/// in physical pixels, converted to overlay points on every draw.
pub(crate) struct ScorePopup {
    pub(crate) anchor: (f32, f32),
    pub(crate) points: u32,
    pub(crate) speed: u32,
    pub(crate) size: u32,
    pub(crate) born: Instant,
    pub(crate) particles: Vec<Particle>,
}

impl ScorePopup {
    /// Spawns the card and [`BURST_COUNT`] particles around `anchor`.
    pub(crate) fn new(anchor: (f32, f32), points: u32, speed: u32, size: u32, now: Instant) -> Self {
        let mut particles = Vec::with_capacity(BURST_COUNT);
        for _ in 0..BURST_COUNT {
            let angle = fastrand::f32() * std::f32::consts::TAU;
            let speed = BURST_SPEED_MIN + fastrand::f32() * (BURST_SPEED_MAX - BURST_SPEED_MIN);
            let life = BURST_LIFE_MIN + fastrand::f32() * (BURST_LIFE_MAX - BURST_LIFE_MIN);
            let radius = BURST_RADIUS_MIN + fastrand::f32() * (BURST_RADIUS_MAX - BURST_RADIUS_MIN);
            let color = BURST_COLORS[fastrand::usize(0..BURST_COLORS.len())];
            particles.push(Particle { vx: angle.cos() * speed, vy: angle.sin() * speed - BURST_RISE, born: now, life, radius, color });
        }
        Self { anchor, points, speed, size, born: now, particles }
    }

    /// Seconds since the click.
    fn age(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.born).as_secs_f32()
    }

    /// Whether the card is still inside its display window.
    pub(crate) fn alive(&self, now: Instant) -> bool {
        self.age(now) < SCORE_POPUP_SECS
    }

    /// Whether any particle is still moving (the burst outlives the card).
    pub(crate) fn animating(&self, now: Instant) -> bool {
        self.particles.iter().any(|particle| now.saturating_duration_since(particle.born).as_secs_f32() < particle.life)
    }
}

/// Paints the burst and the card; returns whether another frame is needed.
///
/// `anchor` is the click position in overlay points (the stored position is a
/// screen coordinate; the caller converts it for this frame).
pub(crate) fn ui(ui: &egui::Ui, screen: Rect, anchor: (f32, f32), score: &ScorePopup, now: Instant) -> bool {
    let painter = ui.painter();

    // Burst first, card on top: the card is the message, the dots the flourish.
    for particle in &score.particles {
        let t = now.saturating_duration_since(particle.born).as_secs_f32();
        let life_ratio = t / particle.life;
        if life_ratio >= 1.0 {
            continue;
        }
        // x = vx·t, y = vy·t + ½g·t² — evaluated, not integrated.
        let x = anchor.0 + particle.vx * t;
        let y = anchor.1 + particle.vy * t + 0.5 * BURST_GRAVITY * t * t;
        painter.circle_filled(Pos2::new(x, y), particle.radius, particle.color.gamma_multiply(1.0 - life_ratio));
    }

    let age = score.age(now);
    if age >= SCORE_POPUP_SECS {
        return score.animating(now);
    }
    // `dshPetScorePop` runs over 0.28 s and then holds scale 1 / opacity 1.
    let progress = (age / SCORE_POPUP_CSS_DURATION).clamp(0.0, 1.0);
    let scale = 0.6 + 0.4 * progress;

    let val = layout(format!("+{}", score.points), VAL_SIZE, VAL_LINE_HEIGHT, VAL_COLOR);
    let sub = layout(format!("速度 {} · 大小 {}", score.speed, score.size), SUB_SIZE, SUB_LINE_HEIGHT, SUB_COLOR);
    let val_galley = painter.layout_job(val);
    let sub_galley = painter.layout_job(sub);

    let content = Vec2::new(val_galley.size().x.max(sub_galley.size().x), val_galley.size().y + SUB_GAP + sub_galley.size().y);
    // `min-width:120px; padding:8px 16px 9px` plus the 1 px border on each side.
    let inner_width = content.x.max(CARD_MIN_WIDTH - 2.0 * CARD_PAD_X);
    let size = Vec2::new(inner_width + 2.0 * CARD_PAD_X + 2.0, content.y + CARD_PAD_TOP + CARD_PAD_BOTTOM + 2.0);

    // `left = clamp(px - w/2, 4, W - w - 4)`, `top = max(4, py - h - 14)`.
    let x = (anchor.0 - size.x / 2.0).clamp(screen.left() + CARD_EDGE, (screen.right() - size.x - CARD_EDGE).max(screen.left() + CARD_EDGE));
    let y = (anchor.1 - size.y - CARD_LIFT).max(screen.top() + CARD_EDGE);
    let rect = Rect::from_min_size(Pos2::new(x, y), size);

    // CSS `transform: scale(.6)` grows around the element's centre.
    let scaled = Rect::from_center_size(rect.center(), size * scale);
    let radius = CornerRadius::same(CARD_RADIUS.round() as u8);

    let mut layer = painter.clone();
    layer.set_opacity(progress);
    layer.add(Shadow { offset: [0, 10], blur: 32, spread: 0, color: CARD_SHADOW }.as_shape(scaled, radius));
    layer.rect_filled(scaled, radius, CARD_BG);
    layer.rect_stroke(scaled, radius, Stroke::new(1.0, CARD_BORDER), StrokeKind::Inside);

    // Text is laid out against the unscaled card and then transformed with it,
    // so the whole card pops as one element (CSS scales padding and text too).
    let val_pos = rect.min + Vec2::new(1.0 + CARD_PAD_X, 1.0 + CARD_PAD_TOP);
    let sub_pos = val_pos + Vec2::new(0.0, val_galley.size().y + SUB_GAP);
    if scale >= 1.0 {
        paint_value(&layer, val_pos, val_galley, VAL_COLOR);
        layer.galley(sub_pos, sub_galley, SUB_COLOR);
    } else {
        // `point' = centre + (point - centre) * scale`, as the CSS transform does.
        let transform = egui::emath::TSTransform::new(rect.center().to_vec2() * (1.0 - scale), scale);
        let mut shapes = egui::Shape::Vec(vec![
            egui::Shape::galley(val_pos, val_galley.clone(), VAL_COLOR),
            egui::Shape::galley(val_pos + Vec2::new(BOLD_OFFSET, 0.0), val_galley, VAL_COLOR),
            egui::Shape::galley(sub_pos, sub_galley, SUB_COLOR),
        ]);
        shapes.transform(transform);
        layer.extend([shapes]);
    }
    true
}

/// Paints the "+N" line. epaint has no synthetic bold, so a second pass half a
/// point to the right stands in for the CSS `font-weight: 700`.
fn paint_value(painter: &egui::Painter, pos: Pos2, galley: std::sync::Arc<egui::Galley>, color: Color32) {
    painter.galley(pos, galley.clone(), color);
    painter.galley(pos + Vec2::new(BOLD_OFFSET, 0.0), galley, color);
}

fn layout(text: String, size: f32, line_height: f32, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::single_section(
        text,
        TextFormat { font_id: FontId::proportional(size), line_height: Some(size * line_height), color, ..Default::default() },
    );
    job.wrap = TextWrapping { max_width: f32::INFINITY, max_rows: 1, break_anywhere: true, overflow_character: None };
    job
}
