//! Font injection for the overlay's egui context.
//!
//! The pet ships one font (`fonts/上首软体.ttf`) that upstream uses for bubbles,
//! the tooltip and the menu. Every egui `Context` owns its own font set, so the
//! host injects it into the overlay context once, on the first pass.
//!
//! The font is inserted as the *primary* font of the proportional family:
//! epaint walks the configured fonts first and the system font provider after
//! them, so the bundled font wins for the glyphs it has and CJK text still falls
//! back to a system face for anything it lacks.

/// Registers a bundled font file with `ctx`, preferring it over the defaults.
pub fn install_font(ctx: &egui::Context, data: Vec<u8>) {
    ctx.add_font(egui::FontInsert::new(
        "candy",
        egui::FontData::from_owned(data),
        vec![egui::InsertFontFamily { family: egui::FontFamily::Proportional, priority: egui::FontPriority::Highest }],
    ));
}
