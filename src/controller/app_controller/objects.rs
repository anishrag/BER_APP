//! The Phase-3 (Objects) survey workflow: the assessor taps the plan to drop /
//! open a room pin, then adds fixed objects (with a count + photos) to it, then
//! closes it and moves to the next room. Thin shell over the `ber_model::objects`
//! data: objects live on `Storey.object_rooms` (Layer 1), so an add/delete is
//! snapshot → mutate → mark_dirty (the storey snapshot captures `object_rooms`,
//! so undo works; resolve preserves it via the storey clone). No re-resolve is
//! needed — objects don't affect geometry.
use super::*;
use ber_model::ber::common::Photo;
use ber_model::ber::objects::{BuildingObject, ObjectKind, ObjectRoom};
use domain::points::Point2f32;

/// A room pin for the plan overlay: where it is, its name, how many objects it
/// holds, and whether it's the one currently open.
pub struct ObjectPinView {
    pub id: ItemId,
    pub x: f64,
    pub y: f64,
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

    /// Close the open room (the panel's "Done").
    pub fn close_object_room(&mut self) {
        self.active_object_room = None;
    }

    /// Rename the open room.
    pub fn rename_object_room(&mut self, name: String) {
        let Some((idx, room_id)) = self.current_storey_idx.zip(self.active_object_room) else {
            return;
        };
        let snap = self.current_history_snapshot();
        if let Some(room) = self.object_room_mut(idx, room_id) {
            room.name = name;
            let _ = self.mark_dirty();
            self.push_history_snapshot(snap);
        }
    }

    /// Add an object to the open room: store its photos, then push it. `count`
    /// is clamped to ≥1. No-op if no room is open.
    pub fn add_object(
        &mut self,
        kind: ObjectKind,
        count: u32,
        note: Option<String>,
        photos: Vec<std::path::PathBuf>,
        photo_quality: PhotoQuality,
    ) {
        let Some((idx, room_id)) = self.current_storey_idx.zip(self.active_object_room) else {
            return;
        };
        // Store photos up front (needs `&self`) before the mutable borrow.
        let stored: Vec<Photo> = photos
            .iter()
            .filter_map(|p| self.store_photo_file(p, photo_quality))
            .collect();
        let snap = self.current_history_snapshot();
        if let Some(room) = self.object_room_mut(idx, room_id) {
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
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let snap = self.current_history_snapshot();
        if let Some(room) = self.object_room_mut(idx, room_id) {
            let before = room.objects.len();
            room.objects.retain(|o| o.id != object_id);
            if room.objects.len() != before {
                let _ = self.mark_dirty();
                self.push_history_snapshot(snap);
            }
        }
    }

    /// Delete a whole room pin and everything on it (the Delete panel).
    pub fn delete_object_room(&mut self, room_id: ItemId) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let Some(storey) = record.data.storeys.get_mut(idx) else {
            return;
        };
        let before = storey.object_rooms.len();
        storey.object_rooms.retain(|r| r.id != room_id);
        if storey.object_rooms.len() != before {
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
                name: r.name.clone(),
                count: r.total_count(),
                active: self.active_object_room == Some(r.id),
            })
            .collect()
    }

    /// The open room's panel view, or `None` when none is open.
    pub fn active_object_room_view(&self) -> Option<ActiveObjectRoomView> {
        let idx = self.current_storey_idx?;
        let room_id = self.active_object_room?;
        let storey = self.current_record.as_ref()?.data.storeys.get(idx)?;
        let room = storey.object_rooms.iter().find(|r| r.id == room_id)?;
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
        })
    }

    /// Mutable borrow of a room on storey `idx` by id.
    fn object_room_mut(&mut self, idx: usize, room_id: ItemId) -> Option<&mut ObjectRoom> {
        self.current_record
            .as_mut()?
            .data
            .storeys
            .get_mut(idx)?
            .object_rooms
            .iter_mut()
            .find(|r| r.id == room_id)
    }
}
