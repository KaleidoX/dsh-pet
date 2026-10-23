//! Cascading context menu: the tree builder and the panel renderer.
//!
//! A direct port of `src/shared/menu.ts`. Every level is its own panel,
//! positioned next to the item that opened it, flipping to the left above the
//! right edge and to the top above the bottom edge, and clamped into the menu's
//! allowed rectangle. Panels are siblings, never nested, so a child panel is
//! never clipped by its parent's scroll area.
//!
//! dsh-only entries (open site, balance, whisper, chat, display target, reload)
//! are not built: this app has no dsh integration. What remains is the pet's
//! own side — the "回到初始位置" item and the full action tree.

use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, CornerRadius, FontId, Pos2, Rect, Shadow, Stroke, StrokeKind, TextFormat, Vec2};
use pet_config::PetConfig;
use pet_ui_core::OverlayAction;

/// Menu text size (`font-size:13px`).
const FONT_SIZE: f32 = 13.0;
/// `line-height:1.5`.
const LINE_HEIGHT: f32 = FONT_SIZE * 1.5;
const PANEL_MIN_W: f32 = 150.0;
const PANEL_MAX_W: f32 = 240.0;
const PANEL_PAD: f32 = 4.0;
const PANEL_RADIUS: f32 = 8.0;
const BORDER: f32 = 1.0;
const ITEM_PAD_X: f32 = 12.0;
const ITEM_PAD_Y: f32 = 5.0;
const ITEM_GAP: f32 = 14.0;
const ITEM_RADIUS: f32 = 6.0;
const ARROW_SIZE: f32 = 12.0;
const SCROLLBAR_W: f32 = 8.0;
const SCROLLBAR_INSET: f32 = 2.0;
/// Distance between a panel and the item it is attached to.
const PANEL_OFFSET: f32 = 4.0;
/// Margin kept from the edges of the allowed rectangle.
const EDGE: f32 = 4.0;
/// `max-height:min(62vh,460px)` with the desktop override.
const MAX_HEIGHT: f32 = 460.0;
const MIN_MAX_HEIGHT: f32 = 120.0;
/// Pointer leaving the whole tree closes the menu after this long.
const CLOSE_DELAY: Duration = Duration::from_millis(200);

const TEXT: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2b);
const PANEL_BG: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 250);
const PANEL_BORDER: Color32 = Color32::from_black_alpha(31);
const PANEL_SHADOW: Color32 = Color32::from_black_alpha(51);
const HOVER: Color32 = Color32::from_rgba_unmultiplied_const(43, 99, 255, 36);
const ARROW: Color32 = Color32::from_rgb(0x9a, 0xa0, 0xa6);
const THUMB: Color32 = Color32::from_black_alpha(41);
const THUMB_HOVER: Color32 = Color32::from_rgba_unmultiplied_const(43, 99, 255, 102);

/// One clickable menu entry.
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    /// What the pet should do when the entry is picked.
    pub action: OverlayAction,
}

/// A menu node: a leaf or a branch with a submenu.
#[derive(Debug, Clone)]
pub enum MenuNode {
    Item(MenuItem),
    Branch { label: String, children: Vec<MenuNode> },
}

impl MenuNode {
    fn label(&self) -> &str {
        match self {
            Self::Item(item) => &item.label,
            Self::Branch { label, .. } => label,
        }
    }

    fn children(&self) -> Option<&[MenuNode]> {
        match self {
            Self::Item(_) => None,
            Self::Branch { children, .. } => Some(children),
        }
    }
}

/// Builds the menu from the merged animation config, exactly like
/// `buildMenuTree` does for the pet-owned half.
///
/// The root holds "回到初始位置" plus a single "动作" branch; the branch's
/// children are the non-empty pools (idle, turn, drag, clicks, moves), then the
/// non-empty random categories. Event pools are dsh-driven and deliberately not
/// offered, per the project's scope.
pub fn build_menu_tree(config: &PetConfig) -> Vec<MenuNode> {
    let animations = &config.animations;
    let mut groups = Vec::new();

    let pools: [(&str, &[String]); 4] =
        [("待机", &animations.idle), ("转向", &animations.turn), ("拖拽", &animations.drag), ("点击回应", &animations.clicks)];
    for (label, pool) in pools {
        if !pool.is_empty() {
            groups.push(MenuNode::Branch { label: label.to_owned(), children: pool.iter().map(|name| play_once(name)).collect() });
        }
    }

    if !animations.moves.actions.is_empty() {
        groups.push(MenuNode::Branch {
            label: "移动".to_owned(),
            children: animations
                .moves
                .actions
                .iter()
                .map(|action| MenuNode::Item(MenuItem { label: action.name.clone(), action: OverlayAction::MoveTo(action.name.clone()) }))
                .collect(),
        });
    }

    for category in animations.categories.iter().filter(|category| !category.actions.is_empty()) {
        groups.push(MenuNode::Branch { label: category.id.clone(), children: category.actions.iter().map(|name| play_once(name)).collect() });
    }

    let mut root = vec![MenuNode::Item(MenuItem { label: "回到初始位置".to_owned(), action: OverlayAction::Home })];
    if !groups.is_empty() {
        root.push(MenuNode::Branch { label: "动作".to_owned(), children: groups });
    }
    root
}

fn play_once(name: &str) -> MenuNode {
    MenuNode::Item(MenuItem { label: name.to_owned(), action: OverlayAction::PlayOnce(name.to_owned()) })
}

/// State of an open menu.
pub(crate) struct Menu {
    /// Requested top-left corner, in screen pixels (converted per frame).
    pub(crate) anchor: (f32, f32),
    pub(crate) tree: Vec<MenuNode>,
    /// Expanded item index per visible panel; `usize::MAX` means "no branch".
    open: Vec<usize>,
    /// Item under the pointer per panel, for the hover highlight only.
    hover: Vec<Option<usize>>,
    /// Scroll offset per panel, in points.
    scroll: Vec<f32>,
    /// Last pointer position seen inside the overlay window; until one arrives
    /// the leave timer must not run (the pointer may still be over the pet).
    saw_pointer: bool,
    /// When the pointer last left the whole menu.
    left_at: Option<Instant>,
}

/// What the caller should do after a pass.
pub(crate) enum Outcome {
    /// Menu stays open.
    Stay,
    /// The menu was dismissed without a choice.
    Dismiss,
    /// The user picked an entry.
    Action(OverlayAction),
}

impl Menu {
    pub(crate) fn new(anchor: (f32, f32), tree: Vec<MenuNode>) -> Self {
        Self { anchor, tree, open: vec![usize::MAX], hover: Vec::new(), scroll: Vec::new(), saw_pointer: false, left_at: None }
    }

    /// Moves the menu to a new anchor (a second right-click) and collapses it.
    pub(crate) fn reopen(&mut self, anchor: (f32, f32), tree: Vec<MenuNode>) {
        self.anchor = anchor;
        self.tree = tree;
        self.open = vec![usize::MAX];
        self.hover.clear();
        self.scroll.clear();
        self.left_at = None;
    }
}

/// Size of one panel for a node list.
struct Metrics {
    outer: Vec2,
    content_h: f32,
    scrollbar: bool,
}

/// One visible panel: where it is and where each of its items sits.
struct PanelLayout {
    rect: Rect,
    /// Item rectangles in list order, already scrolled and clipped.
    items: Vec<Rect>,
    /// Area items are clipped to (the panel's inner box).
    clip: Rect,
    content_h: f32,
    scrollbar: bool,
}

/// Draws the menu and handles its input for one pass. `anchor` is the root
/// panel corner in overlay points, converted by the caller from the stored
/// screen position; `pointer` is the same for the cursor. Both come from the
/// host rather than the window's own input, for the reason described on
/// [`crate::OverlayInput`].
pub(crate) fn ui(
    ui: &egui::Ui,
    screen: Rect,
    menu: &mut Menu,
    bounds: Option<Rect>,
    now: Instant,
    anchor: (f32, f32),
    pointer: Option<Pos2>,
    pressed: bool,
    escape: bool,
) -> Outcome {
    if menu.tree.is_empty() {
        return Outcome::Dismiss;
    }
    let painter = ui.painter();
    let font = FontId::proportional(FONT_SIZE);
    // The menu is clamped to the window, or to the window ∩ work area when the
    // host can report one (same clamp the desktop client passes upstream).
    let clamp = bounds.map_or(screen, |bounds| screen.intersect(bounds));
    let max_height = (clamp.height() - 16.0).clamp(MIN_MAX_HEIGHT, MAX_HEIGHT);

    let arrow_w = painter.layout_no_wrap("▸".to_owned(), FontId::proportional(ARROW_SIZE), ARROW).size().x;

    // ---- Layout: one panel per open level, positioned as each is built. ----
    let mut panels: Vec<PanelLayout> = Vec::new();
    let mut nodes: &[MenuNode] = &menu.tree;
    let mut depth = 0usize;
    loop {
        let metrics = measure(painter, nodes, &font, arrow_w, max_height);
        let rect = if depth == 0 {
            // Root: drop it at the anchor, pulled inside the clamp rectangle.
            let x = anchor.0.max(clamp.left() + EDGE).min(clamp.right() - metrics.outer.x - EDGE);
            let y = anchor.1.max(clamp.top() + EDGE).min(clamp.bottom() - metrics.outer.y - EDGE);
            Rect::from_min_size(Pos2::new(x.max(clamp.left() + EDGE), y.max(clamp.top() + EDGE)), metrics.outer)
        } else {
            let parent = &panels[depth - 1];
            let Some(item) = menu.open.get(depth - 1).and_then(|index| parent.items.get(*index)).copied() else { break };
            let mut left = item.right() + PANEL_OFFSET;
            if left + metrics.outer.x > clamp.right() - EDGE {
                left = item.left() - metrics.outer.x - PANEL_OFFSET;
            }
            left = left.max(clamp.left() + EDGE);
            let mut top = item.top();
            if top + metrics.outer.y > clamp.bottom() - EDGE {
                top = (clamp.bottom() - EDGE - metrics.outer.y).max(clamp.top() + EDGE);
            }
            Rect::from_min_size(Pos2::new(left, top), metrics.outer)
        };

        // Wheel scrolling only affects this panel's own list.
        let scrollbar = metrics.scrollbar;
        let scroll_max = (metrics.content_h - (metrics.outer.y - 2.0 * BORDER - 2.0 * PANEL_PAD)).max(0.0);
        while menu.scroll.len() <= depth {
            menu.scroll.push(0.0);
        }
        if scroll_max > 0.0 && pointer.is_some_and(|pos| rect.contains(pos)) {
            let delta = ui.input(|input| input.smooth_scroll_delta.y);
            menu.scroll[depth] = (menu.scroll[depth] - delta).clamp(0.0, scroll_max);
        }
        menu.scroll[depth] = menu.scroll[depth].clamp(0.0, scroll_max);

        let content_left = rect.left() + BORDER + PANEL_PAD;
        let content_right = rect.right() - BORDER - PANEL_PAD - if scrollbar { SCROLLBAR_W } else { 0.0 };
        let clip = Rect::from_min_max(rect.min + Vec2::splat(BORDER), rect.max - Vec2::splat(BORDER));
        let item_h = LINE_HEIGHT + 2.0 * ITEM_PAD_Y;
        let mut items = Vec::with_capacity(nodes.len());
        for index in 0..nodes.len() {
            let top = rect.top() + BORDER + PANEL_PAD + index as f32 * item_h - menu.scroll[depth];
            let item = Rect::from_min_max(Pos2::new(content_left, top), Pos2::new(content_right, top + item_h));
            items.push(item.intersect(clip));
        }
        panels.push(PanelLayout { rect, items, clip, content_h: metrics.content_h, scrollbar });

        // Descend only through an expanded branch that still exists. `open` may
        // still be shorter than the panel list while a cascade opens: the
        // hover expansion below resizes it, not the layout loop.
        let next = match menu.open.get(depth).and_then(|index| nodes.get(*index)) {
            Some(MenuNode::Branch { children, .. }) if !children.is_empty() => children.as_slice(),
            _ => break,
        };
        nodes = next;
        depth += 1;
        if depth > 32 {
            break;
        }
    }
    menu.open.resize(panels.len(), usize::MAX);

    // ---- Hover highlight, per visible panel. ----
    menu.hover.clear();
    menu.hover.resize(panels.len(), None);
    for (index, panel) in panels.iter().enumerate() {
        if let Some(pos) = pointer
            && panel.rect.contains(pos)
        {
            menu.hover[index] = panel.items.iter().position(|item| item.contains(pos));
        }
    }

    // ---- Hovering a branch expands its panel; leaves leave the chain alone. ----
    for index in 0..panels.len() {
        let Some(hovered) = menu.hover[index] else { continue };
        let nodes = nodes_at(&menu.tree, &menu.open, index);
        if let Some(MenuNode::Branch { children, .. }) = nodes.get(hovered)
            && !children.is_empty()
            && menu.open[index] != hovered
        {
            menu.open.truncate(index + 1);
            menu.open[index] = hovered;
            menu.scroll.truncate(index + 2);
        }
    }

    // ---- Input. ----
    let mut outcome = Outcome::Stay;
    let inside = pointer.is_some_and(|pos| panels.iter().any(|panel| panel.rect.contains(pos)));
    if pointer.is_some() {
        menu.saw_pointer = true;
    }
    if inside {
        menu.left_at = None;
    } else if menu.saw_pointer {
        // Leaving the whole tree starts the 200 ms close timer; re-entering
        // cancels it, so crossing the gaps between panels is harmless.
        let left_at = *menu.left_at.get_or_insert(now);
        if now.saturating_duration_since(left_at) >= CLOSE_DELAY {
            outcome = Outcome::Dismiss;
        }
    }

    if escape {
        outcome = Outcome::Dismiss;
    }

    // A press is what dismisses (capture-phase mousedown upstream): on a leaf it
    // picks that entry, anywhere outside the panels it just closes the menu.
    let press = pointer.filter(|_| pressed);
    if let Some(pos) = press {
        let mut picked = None;
        for (index, panel) in panels.iter().enumerate() {
            if !panel.rect.contains(pos) {
                continue;
            }
            if let Some(item) = panel.items.iter().position(|item| item.contains(pos))
                && let Some(MenuNode::Item(leaf)) = nodes_at(&menu.tree, &menu.open, index).get(item)
            {
                picked = Some(leaf.action.clone());
            }
        }
        outcome = match picked {
            Some(action) => Outcome::Action(action),
            None if !inside => Outcome::Dismiss,
            None => Outcome::Stay,
        };
    }

    // ---- Paint (later panels on top, as sibling panels that mount later). ----
    for (index, panel) in panels.iter().enumerate() {
        paint_panel(
            ui,
            panel,
            nodes_at(&menu.tree, &menu.open, index),
            &font,
            arrow_w,
            menu.hover[index],
            pointer,
            menu.scroll.get(index).copied().unwrap_or(0.0),
        );
    }

    outcome
}

/// Nodes backing panel `index`: walk the open chain from the root.
fn nodes_at<'a>(tree: &'a [MenuNode], open: &[usize], index: usize) -> &'a [MenuNode] {
    let mut nodes = tree;
    for depth in 0..index {
        let Some(MenuNode::Branch { children, .. }) = open.get(depth).and_then(|index| nodes.get(*index)) else { return &[] };
        nodes = children;
    }
    nodes
}

fn measure(painter: &egui::Painter, nodes: &[MenuNode], font: &FontId, arrow_w: f32, max_height: f32) -> Metrics {
    let mut widest: f32 = 0.0;
    for node in nodes {
        let label_w = painter.layout_no_wrap(node.label().to_owned(), font.clone(), TEXT).size().x;
        let extra = if node.children().is_some() { ITEM_GAP + arrow_w } else { 0.0 };
        widest = widest.max(label_w + extra);
    }
    let item_h = LINE_HEIGHT + 2.0 * ITEM_PAD_Y;
    let content_h = nodes.len() as f32 * item_h;
    let scrollbar = content_h + 2.0 * PANEL_PAD + 2.0 * BORDER > max_height;
    let outer_w =
        (widest + 2.0 * ITEM_PAD_X + 2.0 * PANEL_PAD + 2.0 * BORDER + if scrollbar { SCROLLBAR_W } else { 0.0 }).clamp(PANEL_MIN_W, PANEL_MAX_W);
    let outer_h = (content_h + 2.0 * PANEL_PAD + 2.0 * BORDER).min(max_height);
    Metrics { outer: Vec2::new(outer_w, outer_h), content_h, scrollbar }
}

#[allow(clippy::too_many_arguments)]
fn paint_panel(
    ui: &egui::Ui,
    panel: &PanelLayout,
    nodes: &[MenuNode],
    font: &FontId,
    arrow_w: f32,
    hover: Option<usize>,
    pointer: Option<Pos2>,
    _scroll: f32,
) {
    let painter = ui.painter();
    let radius = CornerRadius::same(PANEL_RADIUS as u8);
    painter.add(Shadow { offset: [0, 8], blur: 28, spread: 0, color: PANEL_SHADOW }.as_shape(panel.rect, radius));
    painter.rect_filled(panel.rect, radius, PANEL_BG);
    painter.rect_stroke(panel.rect, radius, Stroke::new(BORDER, PANEL_BORDER), StrokeKind::Inside);

    let clip = panel.clip;
    let item_painter = painter.with_clip_rect(clip);
    let arrow = painter.layout_no_wrap("▸".to_owned(), FontId::proportional(ARROW_SIZE), ARROW);
    for (index, node) in nodes.iter().enumerate() {
        let Some(rect) = panel.items.get(index).copied() else { break };
        if !clip.intersects(rect) {
            continue;
        }
        if hover == Some(index) {
            item_painter.rect_filled(rect, CornerRadius::same(ITEM_RADIUS as u8), HOVER);
        }
        let reserve = if node.children().is_some() { ITEM_GAP + arrow_w } else { 0.0 };
        let text_width = (rect.width() - 2.0 * ITEM_PAD_X - reserve).max(0.0);
        let mut job = LayoutJob::single_section(
            node.label().to_owned(),
            TextFormat { font_id: font.clone(), line_height: Some(LINE_HEIGHT), color: TEXT, ..Default::default() },
        );
        // `white-space:nowrap` on the item, ellipsis on the label span.
        job.wrap = TextWrapping { max_width: text_width, max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
        let galley = item_painter.layout_job(job);
        let pos = Pos2::new(rect.left() + ITEM_PAD_X, rect.top() + ITEM_PAD_Y);
        item_painter.galley(pos, galley, TEXT);
        if node.children().is_some() {
            let arrow_pos = Pos2::new(rect.right() - ITEM_PAD_X - arrow.size().x, rect.top() + ITEM_PAD_Y + (LINE_HEIGHT - arrow.size().y) / 2.0);
            item_painter.galley(arrow_pos, arrow.clone(), ARROW);
        }
    }

    if panel.scrollbar {
        let track = Rect::from_min_max(
            Pos2::new(panel.rect.right() - BORDER - SCROLLBAR_W, panel.rect.top() + BORDER),
            Pos2::new(panel.rect.right() - BORDER, panel.rect.bottom() - BORDER),
        );
        let viewport = (panel.rect.height() - 2.0 * BORDER - 2.0 * PANEL_PAD).max(1.0);
        let thumb_h = (track.height() * viewport / panel.content_h).max(20.0).min(track.height());
        let max_scroll = (panel.content_h - viewport).max(0.0001);
        let thumb_y = track.top() + (track.height() - thumb_h) * (_scroll / max_scroll).clamp(0.0, 1.0);
        let thumb = Rect::from_min_size(Pos2::new(track.left() + SCROLLBAR_INSET, thumb_y), Vec2::new(SCROLLBAR_W - 2.0 * SCROLLBAR_INSET, thumb_h));
        let color = if pointer.is_some_and(|pos| track.contains(pos)) { THUMB_HOVER } else { THUMB };
        painter.rect_filled(thumb, CornerRadius::same(((SCROLLBAR_W - 2.0 * SCROLLBAR_INSET) / 2.0) as u8), color);
    }
}
