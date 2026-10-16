//! Same-page mixed selection in Edit a PDF. Source references belong to one document
//! generation; a drag previews geometry and commits one engine edit on release.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, Stroke};
use pdfcraft_engine::{Document, Edit, EditableObject, MAX_MOVE_OBJECTS, ObjectKind, ObjectTarget};
use pdfcraft_render::DocInfo;

use crate::canvas::{DocView, PageXform};

const BLUE: Color32 = Color32::from_rgb(0x14, 0x73, 0xE6);

#[derive(Default)]
pub(crate) struct ObjectSelection {
    generation: Option<u64>,
    cache: HashMap<usize, Result<Vec<EditableObject>, String>>,
    page: Option<usize>,
    selected: Vec<EditableObject>,
    gesture: Option<Gesture>,
    /// Expected geometry after our own edit, for remapping indexes that grouping can change.
    pending: Option<Vec<EditableObject>>,
    pub notice: Option<String>,
    pub hold_editor: bool,
    blocked_page: Option<usize>,
    input_blocked: bool,
}

#[derive(Clone, Copy)]
struct Gesture {
    start: Pos2,
    now: Pos2,
    marquee: bool,
    additive: bool,
}

impl ObjectSelection {
    pub fn count(&self) -> usize {
        self.selected.len()
    }

    pub fn selected(&self) -> &[EditableObject] {
        &self.selected
    }

    /// A preview belongs to the active, focused document. Returning from another tab or a
    /// modal must not turn a release that happened elsewhere into a document edit.
    pub fn set_input_blocked(&mut self, blocked: bool) {
        self.input_blocked = blocked;
        if blocked {
            self.gesture = None;
            self.blocked_page = None;
        }
    }

    fn clear(&mut self) {
        self.page = None;
        self.selected.clear();
        self.gesture = None;
        self.pending = None;
        self.blocked_page = None;
    }

    pub fn prepare(&mut self, doc: &Document, enabled: bool, ctx: &egui::Context) {
        self.hold_editor = false;
        if !enabled {
            self.clear();
            return;
        }
        let generation = doc.edit_generation();
        if self.generation != Some(generation) {
            self.cache.clear();
            self.gesture = None;
            let expected = self.pending.take();
            let remapped = self.page.zip(expected).and_then(|(page, expected)| {
                let inventory = doc.editable_objects(page).ok()?;
                let mut out = Vec::new();
                for old in expected {
                    let mut matches = inventory.iter().filter(|o| {
                        o.target.kind == old.target.kind
                            && o.content_type == old.content_type
                            && o.text == old.text
                            && o.rect.iter().zip(old.rect).all(|(a, b)| (a - b).abs() < 0.01)
                    });
                    let object = matches.next()?;
                    if matches.next().is_some() || out.iter().any(|o: &EditableObject| o.target == object.target) {
                        return None;
                    }
                    out.push(object.clone());
                }
                Some(out)
            });
            if let Some(objects) = remapped {
                self.selected = objects;
            } else {
                self.clear();
            }
            self.generation = Some(generation);
        } else {
            // A refused edit keeps the generation and the original selection.
            self.pending = None;
        }
        if self.page.is_some() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.clear();
        }
    }

    /// The move of the whole selection by `offset` (user space). The selection follows it.
    fn move_by(&mut self, page: usize, offset: [f64; 2]) -> Edit {
        let mut expected = self.selected.clone();
        for object in &mut expected {
            object.rect = [object.rect[0] + offset[0], object.rect[1] + offset[1], object.rect[2] + offset[0], object.rect[3] + offset[1]];
        }
        self.pending = Some(expected);
        Edit::MoveObjects { page, objects: self.selected.iter().map(|o| o.target).collect(), offset }
    }

    fn inventory(&mut self, doc: &Document, page: usize) -> Result<Vec<EditableObject>, String> {
        if self.cache.len() >= 8 && !self.cache.contains_key(&page) {
            self.cache.clear();
        }
        self.cache.entry(page).or_insert_with(|| doc.editable_objects(page)).clone()
    }
}

fn screen(xf: &PageXform, info: &DocInfo, page: usize, object: &EditableObject) -> Rect {
    xf.user_rect(info, page, object.rect.map(|v| v as f32))
}

fn modified(ui: &egui::Ui) -> bool {
    ui.input(|i| {
        let modifiers = i
            .events
            .iter()
            .find_map(|e| match e {
                egui::Event::PointerButton { button: egui::PointerButton::Primary, pressed: true, modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(i.modifiers);
        modifiers.shift || modifiers.ctrl || modifiers.command
    })
}

fn single(view: &DocView, page: usize) -> Option<ObjectTarget> {
    if let Some((_, index)) = view.content.selected.filter(|(p, _)| *p == page) {
        Some(ObjectTarget { kind: ObjectKind::Added, index })
    } else if let Some(s) = view.image_selection.as_ref().filter(|s| s.page == page) {
        Some(ObjectTarget { kind: ObjectKind::Image, index: s.index })
    } else {
        view.line_editor.as_ref().filter(|e| e.page == page).map(|e| ObjectTarget { kind: ObjectKind::Text, index: e.block })
    }
}

fn leave_single(view: &mut DocView) {
    view.content.selected = None;
    view.image_selection = None;
    view.line_editor = None;
    view.block_drag = None;
}

/// Returns true while group selection owns the interaction. Ordinary clicks still reach the
/// single-object text/resize tools. A text draft is finished or discarded through its editor.
pub(crate) fn page_input(ui: &egui::Ui, resp: &egui::Response, xf: &PageXform, page: usize, doc: &Document, view: &mut DocView) -> bool {
    if view.objects.input_blocked {
        return true;
    }
    if view.content.draft.is_some() || view.line_editor.as_ref().is_some_and(|e| e.has_changes()) {
        let blocked = view.objects.blocked_page == Some(page);
        let modifier_press = ui.input(|i| i.pointer.primary_pressed())
            && modified(ui)
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| xf.rect.contains(p) && ui.clip_rect().contains(p));
        if blocked || modifier_press {
            view.objects.hold_editor = true;
            view.objects.blocked_page = ui.input(|i| i.pointer.primary_down()).then_some(page);
            if modifier_press {
                view.objects.notice = Some(tl!("Finish or discard the text being edited before selecting several objects.").into());
            }
            return true;
        }
        return false;
    }
    let mut selection = std::mem::take(&mut view.objects);
    let result = input(&mut selection, ui, resp, xf, page, doc, view);
    view.objects = selection;
    result
}

#[allow(clippy::too_many_arguments)]
fn input(s: &mut ObjectSelection, ui: &egui::Ui, resp: &egui::Response, xf: &PageXform, page: usize, doc: &Document, view: &mut DocView) -> bool {
    let (pointer, pressed, down) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down()));
    let info = &doc.info;
    if s.page == Some(page)
        && let Some(mut gesture) = s.gesture
    {
        if let Some(p) = pointer {
            gesture.now = p;
        }
        if !down {
            if !gesture.marquee && gesture.start.distance(gesture.now) <= 3.0 && resp.double_clicked() {
                s.clear();
                return false;
            }
            s.gesture = None;
            if gesture.marquee {
                let area = Rect::from_two_pos(gesture.start, gesture.now).intersect(xf.rect);
                if !gesture.additive {
                    s.selected.clear();
                }
                if area.width() > 3.0
                    && area.height() > 3.0
                    && let Ok(objects) = s.inventory(doc, page)
                {
                    let candidates: Vec<_> = objects.into_iter().filter(|o| area.intersects(screen(xf, info, page, o))).collect();
                    let mut selected = s.selected.clone();
                    for object in candidates {
                        if !selected.iter().any(|o| o.target == object.target) {
                            selected.push(object);
                        }
                    }
                    if selected.len() <= MAX_MOVE_OBJECTS {
                        s.selected = selected;
                    } else {
                        s.notice = Some(tl!("Select at most 1000 objects at a time.").into());
                    }
                }
                if s.selected.is_empty() {
                    s.clear();
                }
            } else if gesture.start.distance(gesture.now) > 3.0 && view.pending_edit.is_none() {
                let (a, b) = (xf.screen_to_view(gesture.start), xf.screen_to_view(gesture.now));
                match doc.object_move_offset(page, [f64::from(b.0 - a.0), f64::from(b.1 - a.1)]) {
                    Ok(offset) => view.pending_edit = Some(s.move_by(page, offset)),
                    Err(_) => s.notice = Some(tl!("These objects could not be moved. Select them again and try a smaller move.").into()),
                }
            }
        } else {
            s.gesture = Some(gesture);
        }
        ui.ctx().set_cursor_icon(if gesture.marquee { egui::CursorIcon::Crosshair } else { egui::CursorIcon::Move });
        return true;
    }
    let Some(p) = pointer.filter(|p| xf.rect.contains(*p) && ui.clip_rect().contains(*p) && resp.rect.contains(*p)) else {
        return s.page == Some(page) && !s.selected.is_empty();
    };
    if !pressed {
        if s.page == Some(page) && !s.selected.is_empty() {
            let union = s.selected.iter().fold(Rect::NOTHING, |r, o| r.union(screen(xf, info, page, o)));
            if union.contains(p) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
            }
            return true;
        }
        return false;
    }
    let objects = match s.inventory(doc, page) {
        Ok(objects) => objects,
        Err(_) => {
            if modified(ui) {
                s.notice = Some(tl!("This page contains objects that cannot be selected together.").into());
                return true;
            }
            return false;
        }
    };
    let additive = modified(ui);
    // A page-sized background image must not hide editable labels or added notes.
    // Within each kind, prefer the last object as the ordinary single-item tools do.
    let hit = [ObjectKind::Added, ObjectKind::Text, ObjectKind::Image]
        .into_iter()
        .find_map(|kind| objects.iter().rev().find(|o| o.target.kind == kind && screen(xf, info, page, o).expand(2.0).contains(p)));
    if additive {
        if s.page != Some(page) {
            s.clear();
            s.page = Some(page);
            if let Some(target) = single(view, page)
                && let Some(object) = objects.iter().find(|o| o.target == target)
            {
                s.selected.push(object.clone());
            }
        }
        if let Some(object) = hit {
            if let Some(index) = s.selected.iter().position(|o| o.target == object.target) {
                s.selected.remove(index);
            } else if s.selected.len() < MAX_MOVE_OBJECTS {
                s.selected.push(object.clone());
            } else {
                s.notice = Some(tl!("Select at most 1000 objects at a time.").into());
            }
        } else {
            s.gesture = Some(Gesture { start: p, now: p, marquee: true, additive: true });
        }
        leave_single(view);
        view.current = page;
        return true;
    }
    if s.page == Some(page) && !s.selected.is_empty() {
        let union = s.selected.iter().fold(Rect::NOTHING, |r, o| r.union(screen(xf, info, page, o)));
        if union.contains(p) && !resp.double_clicked() {
            s.gesture = Some(Gesture { start: p, now: p, marquee: false, additive: false });
            return true;
        }
    }
    // The frame of the paragraph open for typing takes hold of the box, as in Acrobat: a click
    // selects it (for the arrow keys), a drag moves it. Its text keeps the caret.
    if let Some(block) = view.line_editor.as_ref().filter(|e| e.page == page && !e.has_changes()).map(|e| e.block)
        && let Some(object) = objects.iter().find(|o| o.target == ObjectTarget { kind: ObjectKind::Text, index: block })
        && on_frame(screen(xf, info, page, object), p)
    {
        s.clear();
        s.page = Some(page);
        s.selected.push(object.clone());
        s.gesture = Some(Gesture { start: p, now: p, marquee: false, additive: false });
        leave_single(view);
        view.current = page;
        return true;
    }
    s.clear();
    if hit.is_some() {
        return false;
    }
    leave_single(view);
    s.page = Some(page);
    s.gesture = Some(Gesture { start: p, now: p, marquee: true, additive: false });
    view.current = page;
    true
}

/// Whether `p` is on the frame drawn around a paragraph open for typing (`r`, screen): just
/// outside the text, where a press would otherwise close the paragraph.
fn on_frame(r: Rect, p: Pos2) -> bool {
    r.expand(2.0 + FRAME_REACH).contains(p) && !r.expand(2.0).contains(p)
}

/// How far outside a paragraph's frame a press still grabs it (screen pixels).
const FRAME_REACH: f32 = 6.0;

/// The arrow keys move the selected image or objects a point, Shift and an arrow ten points,
/// as in Acrobat. They are taken before the view turns the page with them.
pub(crate) fn nudge_keys(ctx: &egui::Context, view: &mut DocView, doc: &Document) {
    if ctx.egui_wants_keyboard_input() || view.pending_edit.is_some() {
        return;
    }
    let image = view.image_selection.as_ref().filter(|s| !s.dragging()).map(|s| (s.page, s.index));
    let objects = view.objects.page.filter(|_| !view.objects.selected.is_empty() && view.objects.gesture.is_none());
    let Some(page) = objects.or(image.map(|(page, _)| page)) else { return };
    let Some(xf) = view.page_xform(page) else { return };
    use egui::{Key, Modifiers};
    let mut step = egui::Vec2::ZERO;
    ctx.input_mut(|i| {
        for (key, direction) in [
            (Key::ArrowLeft, egui::vec2(-1.0, 0.0)),
            (Key::ArrowRight, egui::vec2(1.0, 0.0)),
            (Key::ArrowUp, egui::vec2(0.0, -1.0)),
            (Key::ArrowDown, egui::vec2(0.0, 1.0)),
        ] {
            // Shift first: a plain key pattern also matches a Shift press.
            if i.consume_key(Modifiers::SHIFT, key) {
                step += direction * 10.0;
            } else if i.consume_key(Modifiers::NONE, key) {
                step += direction;
            }
        }
    });
    if step == egui::Vec2::ZERO {
        return;
    }
    // A point on screen, whatever the zoom and the view's rotation: one screen pixel's step in
    // view space, at unit length.
    let c = xf.rect.center();
    let unit = |d: egui::Vec2| {
        let (a, b) = (xf.screen_to_view(c), xf.screen_to_view(c + d));
        let v = egui::vec2(b.0 - a.0, b.1 - a.1);
        if v.length() > 0.0 { v / v.length() } else { egui::Vec2::ZERO }
    };
    let delta = unit(egui::vec2(1.0, 0.0)) * step.x + unit(egui::vec2(0.0, 1.0)) * step.y;
    let Ok(offset) = doc.object_move_offset(page, [f64::from(delta.x), f64::from(delta.y)]) else { return };
    if objects.is_some() {
        view.pending_edit = Some(view.objects.move_by(page, offset));
    } else if let Some((page, index)) = image
        && let Some(r) = doc.page_images(page).get(index).map(|im| im.rect)
    {
        let rect = [r[0] + offset[0], r[1] + offset[1], r[2] + offset[0], r[3] + offset[1]];
        view.pending_edit = Some(Edit::EditPageImage { page, index, change: pdfcraft_engine::ImageEdit::Move(rect) });
    }
}

pub(crate) fn paint(painter: &egui::Painter, xf: &PageXform, page: usize, info: &DocInfo, selection: &ObjectSelection) {
    if selection.page != Some(page) {
        return;
    }
    let delta = selection.gesture.filter(|g| !g.marquee).map_or(egui::Vec2::ZERO, |g| g.now - g.start);
    let mut union = Rect::NOTHING;
    for object in &selection.selected {
        let rect = screen(xf, info, page, object).translate(delta);
        painter.rect_filled(rect, 0.0, BLUE.gamma_multiply(0.05));
        painter.rect_stroke(rect, 0.0, Stroke::new(1.5, BLUE), egui::StrokeKind::Outside);
        union = union.union(rect);
    }
    if selection.selected.len() > 1 {
        painter.rect_stroke(union.expand(4.0), 0.0, Stroke::new(1.0, BLUE.gamma_multiply(0.7)), egui::StrokeKind::Outside);
    }
    if let Some(g) = selection.gesture.filter(|g| g.marquee) {
        let area = Rect::from_two_pos(g.start, g.now).intersect(xf.rect);
        painter.rect_filled(area, 0.0, BLUE.gamma_multiply(0.08));
        painter.rect_stroke(area, 0.0, Stroke::new(1.0, BLUE), egui::StrokeKind::Outside);
    }
}

/// Keep the UI refusal actionable and translated; headless callers retain the detailed error.
pub(crate) fn move_error(error: &pdfcraft_engine::EditError) -> String {
    let detail = error.to_string();
    match detail.as_str() {
        "vertical text cannot be moved in a group" => tl!("Vertical text cannot be moved with this selection.").into(),
        "text used as a clipping path cannot be moved in a group" => tl!("Text used as a clipping path cannot be moved with this selection.").into(),
        _ => tl!("These objects could not be moved. Select them again and try a smaller move.").into(),
    }
}

/// Formats a list of editable objects into plain text (TSV for multiple columns) and HTML table
/// if the objects form a multi-column or multi-row tabular layout.
pub(crate) fn format_objects_as_table(objects: &[EditableObject]) -> (String, Option<String>) {
    let mut text_objects: Vec<&EditableObject> = objects.iter().filter(|o| o.text.is_some()).collect();
    if text_objects.is_empty() {
        return (String::new(), None);
    }
    if text_objects.len() == 1 {
        let text = text_objects.first().and_then(|o| o.text.as_deref()).unwrap_or("").to_string();
        return (text, None);
    }
    // PDF user space: y increases upward, so top-to-bottom order is descending vertical center.
    text_objects.sort_by(|a, b| {
        let cy_a = (a.rect[1] + a.rect[3]) / 2.0;
        let cy_b = (b.rect[1] + b.rect[3]) / 2.0;
        cy_b.total_cmp(&cy_a)
    });
    let mut rows: Vec<(f64, f64, Vec<&EditableObject>)> = Vec::new();
    for obj in text_objects {
        let cy = (obj.rect[1] + obj.rect[3]) / 2.0;
        let min_y = obj.rect[1].min(obj.rect[3]);
        let max_y = obj.rect[1].max(obj.rect[3]);
        match rows.last_mut() {
            Some((top, bottom, members)) => {
                let overlap = top.min(max_y) - bottom.max(min_y);
                let h_min = (*top - *bottom).min(max_y - min_y);
                if (overlap > 0.0 && overlap > 0.25 * h_min) || (cy >= *bottom && cy <= *top) {
                    members.push(obj);
                    *top = (*top).max(max_y);
                    *bottom = (*bottom).min(min_y);
                } else {
                    rows.push((max_y, min_y, vec![obj]));
                }
            }
            None => rows.push((max_y, min_y, vec![obj])),
        }
    }
    let mut is_table = false;
    let mut out = Vec::with_capacity(rows.len());
    let mut row_cells: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    for (_, _, mut members) in rows {
        if members.len() > 1 {
            is_table = true;
        }
        members.sort_by(|a, b| a.rect[0].total_cmp(&b.rect[0]));
        let cells: Vec<String> = members
            .iter()
            .map(|o| o.text.as_deref().unwrap_or("").to_string())
            .collect();
        out.push(cells.join("\t"));
        row_cells.push(cells);
    }
    let plain = out.join("\n");
    let html = if is_table {
        let mut h = String::from("<table border=\"1\">\n");
        for cells in &row_cells {
            h.push_str("  <tr>\n");
            for cell in cells {
                h.push_str("    <td>");
                for c in cell.chars() {
                    match c {
                        '&' => h.push_str("&amp;"),
                        '<' => h.push_str("&lt;"),
                        '>' => h.push_str("&gt;"),
                        '"' => h.push_str("&quot;"),
                        '\'' => h.push_str("&#39;"),
                        _ => h.push(c),
                    }
                }
                h.push_str("</td>\n");
            }
            h.push_str("  </tr>\n");
        }
        h.push_str("</table>\n");
        Some(h)
    } else {
        None
    };
    (plain, html)
}

#[cfg(test)]
mod tests {
    #[test]
    fn selection_labels_and_refusals_are_translated_in_every_catalog() {
        for language in crate::i18n::LANGUAGES.iter().filter(|l| !l.source.is_empty()) {
            let lang = crate::i18n::Lang::from_code(language.code).unwrap();
            for label in [
                "Drag the selection to move it. Esc clears the selection.",
                "Shift-click to select several objects. Drag empty space to select an area.",
                "Select at most 1000 objects at a time.",
                "Finish or discard the text being edited before selecting several objects.",
                "Move object",
                "Move objects",
                "Vertical text cannot be moved with this selection.",
                "Text used as a clipping path cannot be moved with this selection.",
                "These objects could not be moved. Select them again and try a smaller move.",
                "This page contains objects that cannot be selected together.",
            ] {
                assert_ne!(crate::i18n::tr(lang, label), label, "{}: {label}", language.code);
            }
            assert_ne!(crate::i18n::trn(lang, 2, "{n} object selected", "{n} objects selected"), "2 objects selected", "{}", language.code);
        }
    }

    #[test]
    fn formats_objects_as_table() {
        use pdfcraft_engine::{EditableObject, ObjectKind, ObjectTarget};
        let o1 = EditableObject {
            target: ObjectTarget { kind: ObjectKind::Text, index: 0 },
            rect: [10.0, 100.0, 50.0, 115.0],
            content_type: "text",
            text: Some("Cell A1".into()),
        };
        let o2 = EditableObject {
            target: ObjectTarget { kind: ObjectKind::Text, index: 1 },
            rect: [60.0, 100.0, 100.0, 115.0],
            content_type: "text",
            text: Some("Cell B1".into()),
        };
        let o3 = EditableObject {
            target: ObjectTarget { kind: ObjectKind::Text, index: 2 },
            rect: [10.0, 80.0, 50.0, 95.0],
            content_type: "text",
            text: Some("Cell A2".into()),
        };
        let o4 = EditableObject {
            target: ObjectTarget { kind: ObjectKind::Text, index: 3 },
            rect: [60.0, 80.0, 100.0, 95.0],
            content_type: "text",
            text: Some("Cell B2".into()),
        };
        let (plain, html) = super::format_objects_as_table(&[o4, o2, o1, o3]);
        assert_eq!(plain, "Cell A1\tCell B1\nCell A2\tCell B2");
        assert!(html.is_some());
        let h = html.unwrap();
        assert!(h.contains("<table border=\"1\">"));
        assert!(h.contains("<td>Cell A1</td>"));
        assert!(h.contains("<td>Cell B2</td>"));
    }
}
