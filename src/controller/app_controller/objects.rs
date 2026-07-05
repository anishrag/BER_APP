//! The Phase-3 (Objects) survey workflow: the assessor taps the plan to drop /
//! open a room pin, then adds fixed objects (with a count + photos) to it, then
//! closes it and moves to the next room. Thin shell over the `ber_model::objects`
//! data: objects live on `Storey.object_rooms` (Layer 1), so an add/delete is
//! snapshot → mutate → mark_dirty (the storey snapshot captures `object_rooms`,
//! so undo works; resolve preserves it via the storey clone). No re-resolve is
//! needed — objects don't affect geometry.
use super::*;
use ber_model::ber::common::Photo;
use ber_model::ber::objects::{BuildingObject, ObjectCategory, ObjectKind, ObjectRoom};
use domain::points::Point2f32;

/// A room pin for the plan overlay: where it is, its name, how many objects it
/// holds, and whether it's the one currently open.
pub struct ObjectPinView {
    pub id: ItemId,
    pub x: f64,
    pub y: f64,
    /// Height above the pin's storey floor datum (m) for pins placed on a 3D
    /// surface; `None` for plan pins (drawn at the legacy 1 m in 3D).
    pub z_m: Option<f64>,
    pub name: String,
    pub count: u32,
    pub active: bool,
}

/// One object row in the open room's panel.
pub struct ObjectRowView {
    pub id: ItemId,
    pub label: String,
    pub count: u32,
    pub photo_count: usize,
}

/// The open room's details for the entry panel: its name, the objects recorded
/// so far, and which categories are already present (to highlight them).
pub struct ActiveObjectRoomView {
    pub id: ItemId,
    pub name: String,
    pub objects: Vec<ObjectRowView>,
    pub categories_present: Vec<ber_model::ber::objects::ObjectCategory>,
    /// The pitch (degrees from horizontal) of the surface the pin was placed
    /// on in 3D, when that surface is sloped (a roof plane — flat roof = 0° —
    /// or a sloped ceiling). Shown read-only in the add-object dialog so the
    /// assessor sees the roof angle while recording e.g. a solar panel.
    pub surface_pitch_deg: Option<f32>,
}

/// One row in the Objects review: an object kind's house-wide total.
pub struct ObjectTotalView {
    pub label: String,
    pub count: u32,
    pub selected: bool,
}

/// The Objects review takeover: the flat house-wide totals, the selected row,
/// and the 3D world points where the selected kind is located (the hotspots).
pub struct ObjectReviewView {
    pub totals: Vec<ObjectTotalView>,
    pub selected: usize,
    pub hotspots: Vec<(f64, f64, f64)>,
    /// The next empty category awaiting an absence confirmation ("No X?"), or
    /// `None` when every empty category has been confirmed or skipped.
    pub absent_prompt: Option<ber_model::ber::objects::ObjectCategory>,
    /// How many empty categories still await confirmation (incl. the current one).
    pub absent_remaining: usize,
}

impl AppController {
    /// The currently open room pin, if any.
    pub fn active_object_room(&self) -> Option<ItemId> {
        self.active_object_room
    }

    /// Tap the plan at world `(wx, wy)`: open the nearest existing pin within
    /// `tol`, else drop a new room there and open it. Only meaningful in the
    /// Objects phase (the caller gates on it).
    pub fn objects_pointer_down(&mut self, wx: f64, wy: f64, tol: f64) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        self.objects_pointer_down_on(idx, wx, wy, tol);
    }

    /// As [`objects_pointer_down`](Self::objects_pointer_down), but on an EXPLICIT
    /// storey — the all-storeys 2D tab resolves the target storey from the cell
    /// the tap landed in, so there's no single "current" storey to key off.
    pub fn objects_pointer_down_on(&mut self, idx: usize, wx: f64, wy: f64, tol: f64) {
        // Open the nearest existing pin within tolerance.
        if let Some(record) = self.current_record.as_ref() {
            if let Some(storey) = record.data.storeys.get(idx) {
                let near = storey
                    .object_rooms
                    .iter()
                    .map(|r| {
                        let dx = r.plan_position.x as f64 - wx;
                        let dy = r.plan_position.y as f64 - wy;
                        (r.id, (dx * dx + dy * dy).sqrt())
                    })
                    .filter(|(_, d)| *d <= tol)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((id, _)) = near {
                    self.active_object_room = Some(id);
                    return;
                }
            }
        }
        // Otherwise drop a new room pin and open it.
        let snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let Some(storey) = record.data.storeys.get_mut(idx) else {
            return;
        };
        let room = ObjectRoom::new(Point2f32::new(wx as f32, wy as f32));
        let id = room.id;
        storey.object_rooms.push(room);
        self.active_object_room = Some(id);
        let _ = self.mark_dirty();
        self.push_history_snapshot(snap);
    }

    /// Tap a SURFACE in the 3D view (the render layer's pick already resolved
    /// the screen point to `storey_idx` + the storey-local point): open the
    /// nearest existing pin on that storey within `tol` (metres, 3D distance),
    /// else drop a new room ON the surface — carrying its height and, for a
    /// sloped surface (roof plane / sloped ceiling), its pitch for the dialog.
    pub fn objects_pointer_down_3d(
        &mut self,
        storey_idx: usize,
        wx: f64,
        wy: f64,
        z_m: f64,
        pitch_deg: Option<f32>,
        tol: f64,
    ) {
        // Open the nearest existing pin on that storey within tolerance.
        if let Some(record) = self.current_record.as_ref() {
            if let Some(storey) = record.data.storeys.get(storey_idx) {
                let near = storey
                    .object_rooms
                    .iter()
                    .map(|r| {
                        let dx = r.plan_position.x as f64 - wx;
                        let dy = r.plan_position.y as f64 - wy;
                        // Plan pins sit at the legacy 1 m render height.
                        let dz = r.z_m.unwrap_or(1.0) - z_m;
                        (r.id, (dx * dx + dy * dy + dz * dz).sqrt())
                    })
                    .filter(|(_, d)| *d <= tol)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((id, _)) = near {
                    self.active_object_room = Some(id);
                    return;
                }
            }
        }
        // Otherwise drop a new room pin on the surface and open it.
        let snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let Some(storey) = record.data.storeys.get_mut(storey_idx) else {
            return;
        };
        let room = ObjectRoom::new_on_surface(Point2f32::new(wx as f32, wy as f32), z_m, pitch_deg);
        let id = room.id;
        storey.object_rooms.push(room);
        self.active_object_room = Some(id);
        let _ = self.mark_dirty();
        self.push_history_snapshot(snap);
    }

    /// Close the open room (the panel's "Done").
    pub fn close_object_room(&mut self) {
        self.active_object_room = None;
    }

    /// Rename the open room.
    pub fn rename_object_room(&mut self, name: String) {
        let Some(room_id) = self.active_object_room else {
            return;
        };
        let snap = self.current_history_snapshot();
        if let Some(room) = self.object_room_mut(room_id) {
            room.name = name;
            let _ = self.mark_dirty();
            self.push_history_snapshot(snap);
        }
    }

    /// Add an object to the open room: store its photos (each at the capture
    /// tier its button asked for — dual hi/lo buttons), then push it. `count`
    /// is clamped to ≥1. No-op if no room is open.
    pub fn add_object(
        &mut self,
        kind: ObjectKind,
        count: u32,
        note: Option<String>,
        photos: Vec<(std::path::PathBuf, PhotoQuality)>,
    ) {
        let Some(room_id) = self.active_object_room else {
            return;
        };
        // Store photos up front (needs `&self`) before the mutable borrow.
        let stored: Vec<Photo> = photos
            .iter()
            .filter_map(|(p, quality)| self.ingest_photo(p, *quality))
            .collect();
        let snap = self.current_history_snapshot();
        if let Some(room) = self.object_room_mut(room_id) {
            let mut obj = BuildingObject::new(kind, count);
            obj.note = note.filter(|s| !s.trim().is_empty());
            obj.photos = stored;
            room.objects.push(obj);
            let _ = self.mark_dirty();
            self.push_history_snapshot(snap);
        }
    }

    /// Delete one object from a room (the Delete panel).
    pub fn delete_object(&mut self, room_id: ItemId, object_id: ItemId) {
        let snap = self.current_history_snapshot();
        if let Some(room) = self.object_room_mut(room_id) {
            let before = room.objects.len();
            room.objects.retain(|o| o.id != object_id);
            if room.objects.len() != before {
                let _ = self.mark_dirty();
                self.push_history_snapshot(snap);
            }
        }
    }

    /// Delete a whole room pin and everything on it (the Delete panel).
    /// Searches every storey — a pin placed from the full-house 3D view can
    /// live on a storey other than the current one.
    pub fn delete_object_room(&mut self, room_id: ItemId) {
        let snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let mut removed = false;
        for storey in &mut record.data.storeys {
            let before = storey.object_rooms.len();
            storey.object_rooms.retain(|r| r.id != room_id);
            removed |= storey.object_rooms.len() != before;
        }
        if removed {
            if self.active_object_room == Some(room_id) {
                self.active_object_room = None;
            }
            let _ = self.mark_dirty();
            self.push_history_snapshot(snap);
        }
    }

    /// Room pins on the CURRENT storey, for the plan overlay.
    pub fn object_rooms_view(&self) -> Vec<ObjectPinView> {
        let Some(idx) = self.current_storey_idx else {
            return Vec::new();
        };
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return Vec::new();
        };
        storey
            .object_rooms
            .iter()
            .map(|r| ObjectPinView {
                id: r.id,
                x: r.plan_position.x as f64,
                y: r.plan_position.y as f64,
                z_m: r.z_m,
                name: r.name.clone(),
                count: r.total_count(),
                active: self.active_object_room == Some(r.id),
            })
            .collect()
    }

    /// EVERY storey's room pins as world-3D points (each at its storey's floor
    /// datum + its surface height, legacy plan pins at +1 m) — the hotspot
    /// markers for the full-house 3D view, where the current-storey pin list
    /// doesn't apply.
    pub fn object_pin_world_points(&self) -> Vec<(f64, f64, f64)> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let datums = record.data.storey_floor_datums();
        let mut pts = Vec::new();
        for (i, s) in record.data.storeys.iter().enumerate() {
            let datum = datums.get(i).copied().unwrap_or(0.0);
            for r in &s.object_rooms {
                pts.push((
                    r.plan_position.x as f64,
                    r.plan_position.y as f64,
                    datum + r.z_m.unwrap_or(1.0),
                ));
            }
        }
        pts
    }

    /// The open room's panel view, or `None` when none is open. Searches every
    /// storey — a pin placed from the full-house 3D view can live on a storey
    /// other than the current one.
    pub fn active_object_room_view(&self) -> Option<ActiveObjectRoomView> {
        let room_id = self.active_object_room?;
        let room = self
            .current_record
            .as_ref()?
            .data
            .storeys
            .iter()
            .flat_map(|s| s.object_rooms.iter())
            .find(|r| r.id == room_id)?;
        Some(ActiveObjectRoomView {
            id: room.id,
            name: room.name.clone(),
            objects: room
                .objects
                .iter()
                .map(|o| ObjectRowView {
                    id: o.id,
                    label: o.kind.label(),
                    count: o.count,
                    photo_count: o.photos.len(),
                })
                .collect(),
            categories_present: room.categories_present(),
            surface_pitch_deg: room.surface_pitch_deg,
        })
    }

    /// Mutable borrow of a room by id, searching every storey (a pin placed
    /// from the full-house 3D view can live off the current storey).
    fn object_room_mut(&mut self, room_id: ItemId) -> Option<&mut ObjectRoom> {
        self.current_record
            .as_mut()?
            .data
            .storeys
            .iter_mut()
            .flat_map(|s| s.object_rooms.iter_mut())
            .find(|r| r.id == room_id)
    }

    // ── Review ────────────────────────────────────────────────────────────────

    /// Whether any object pin exists house-wide (gates the review entry).
    pub fn has_object_rooms(&self) -> bool {
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.storeys.iter().any(|s| !s.object_rooms.is_empty()))
    }

    /// Whether the Objects review takeover is open.
    pub fn object_review_mode(&self) -> bool {
        self.object_review_mode
    }

    /// Open / close the Objects review. Entering resets the selection + camera
    /// and clears the session "skipped" absences (so every empty category is
    /// re-offered for confirmation).
    pub fn set_object_review_mode(&mut self, on: bool) {
        self.object_review_mode = on;
        if on {
            self.object_review_selected = 0;
            self.object_review_skipped.clear();
            self.reset_view();
        }
    }

    /// Select an object-kind row in the review (drives the 3D hotspots).
    pub fn select_object_review(&mut self, idx: usize) {
        self.object_review_selected = idx;
    }

    /// Confirm an empty category has NO items in the dwelling ("No cylinder?" →
    /// Yes). Recorded durably on the record; the next empty category is offered.
    pub fn confirm_object_absent(&mut self, cat: ber_model::ber::objects::ObjectCategory) {
        if let Some(record) = self.current_record.as_mut() {
            if !record.data.objects_no_items_confirmed.contains(&cat) {
                record.data.objects_no_items_confirmed.push(cat);
                let _ = self.mark_dirty();
            }
        }
    }

    /// Skip an empty category's absence prompt for THIS review ("No X?" → No,
    /// there is one — I'll add it). Session-only; re-offered on the next review.
    pub fn skip_object_absent(&mut self, cat: ber_model::ber::objects::ObjectCategory) {
        self.object_review_skipped.insert(cat);
    }

    /// Whether the Objects review has been confirmed ("all reviewed & OK").
    pub fn objects_reviewed(&self) -> bool {
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.objects_reviewed)
    }

    /// Confirm the Objects review (counts + absences all checked): record it on
    /// the dwelling and close the review.
    pub fn confirm_objects_review(&mut self) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.objects_reviewed = true;
        }
        let _ = self.mark_dirty();
        self.set_object_review_mode(false);
    }

    /// The Objects review view: the flat house-wide totals + the selected kind's
    /// hotspots. `None` when no record / no objects recorded.
    pub fn object_review_view(&self) -> Option<ObjectReviewView> {
        let data = &self.current_record.as_ref()?.data;
        let order = object_label_totals(data);
        // Open even with no objects yet (a pin was dropped) so the absence
        // confirmations can still be worked through.
        let selected = self
            .object_review_selected
            .min(order.len().saturating_sub(1));
        let selected_label = order.get(selected).map(|(l, _)| l.clone());
        let hotspots = selected_label
            .as_deref()
            .map(|l| object_hotspots(data, l))
            .unwrap_or_default();
        let totals = order
            .into_iter()
            .enumerate()
            .map(|(i, (label, count))| ObjectTotalView {
                label,
                count,
                selected: i == selected,
            })
            .collect();
        // Absence prompts: standard categories (not "Other") with NO items, that
        // haven't been confirmed-absent on the record nor skipped this session.
        let present: std::collections::HashSet<ObjectCategory> = data
            .storeys
            .iter()
            .flat_map(|s| s.object_rooms.iter())
            .flat_map(|r| r.objects.iter())
            .map(|o| o.kind.category())
            .collect();
        let remaining: Vec<ObjectCategory> = ObjectCategory::ALL
            .into_iter()
            .filter(|c| {
                *c != ObjectCategory::Other
                    && !present.contains(c)
                    && !data.objects_no_items_confirmed.contains(c)
                    && !self.object_review_skipped.contains(c)
            })
            .collect();
        Some(ObjectReviewView {
            totals,
            selected,
            hotspots,
            absent_prompt: remaining.first().copied(),
            absent_remaining: remaining.len(),
        })
    }
}

/// House-wide object totals as (label, count), in first-seen order.
fn object_label_totals(data: &Ber) -> Vec<(String, u32)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for s in &data.storeys {
        for room in &s.object_rooms {
            for o in &room.objects {
                let label = o.kind.label();
                if !counts.contains_key(&label) {
                    order.push(label.clone());
                }
                *counts.entry(label).or_default() += o.count;
            }
        }
    }
    order.into_iter().map(|l| (l.clone(), counts[&l])).collect()
}

/// World-3D points (per occurrence) where an object with `label` was recorded —
/// each room holding it, at its storey's floor datum + its surface height (a
/// 3D-placed pin's `z_m`; legacy plan pins at the historical +1 m).
fn object_hotspots(data: &Ber, label: &str) -> Vec<(f64, f64, f64)> {
    let datums = data.storey_floor_datums();
    let mut pts = Vec::new();
    for (i, s) in data.storeys.iter().enumerate() {
        let datum = datums.get(i).copied().unwrap_or(0.0);
        for room in &s.object_rooms {
            if room.objects.iter().any(|o| o.kind.label() == label) {
                pts.push((
                    room.plan_position.x as f64,
                    room.plan_position.y as f64,
                    datum + room.z_m.unwrap_or(1.0),
                ));
            }
        }
    }
    pts
}
