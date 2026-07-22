//! The Phase-3 (Objects) survey workflow: the assessor taps the plan to drop /
//! open a room pin, then adds fixed objects (with a count + photos) to it, then
//! closes it and moves to the next room. Thin shell over the `ber_model::objects`
//! data: objects live on `Storey.object_rooms` (Layer 1), so an add/delete is
//! snapshot → mutate → mark_dirty (the storey snapshot captures `object_rooms`,
//! so undo works; resolve preserves it via the storey clone). No re-resolve is
//! needed — objects don't affect geometry.
use super::*;
use ber_model::ber::common::Evidence;
use ber_model::ber::objects::{BuildingObject, ObjectCategory, ObjectKind, ObjectRoom};
use ber_model::ber::systems::HeatingSystemType;
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
    /// The object's kind — the pin dropdown shows the "separate control for
    /// water & space" checkbox only on `Boiler`/`Programmer` rows.
    pub kind: ObjectKind,
    /// The "separate time/temperature control for water and space" flag
    /// ([`BuildingObject::separate_hw_control`]); meaningful for Boiler /
    /// Programmer, drives `HeatingSystem.separate_hot_water`.
    pub separate_hw_control: bool,
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
    /// Whether this is the house-level ATTIC bucket (no plan pin — the UI
    /// anchors its box to the Attic button instead of a pin).
    pub is_attic: bool,
}

/// Which porch fact a pending review question asks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PorchQuestion {
    /// "Is there a door between the house and the porch?"
    SeparatingDoor,
    /// "Any fixed heating in the porch?" (radiator / UFH / storage heater —
    /// asked because underfloor heating has no pinnable object, so absence
    /// of pins proves nothing).
    FixedHeating,
}

/// One pending porch question in the Objects review.
pub struct PorchPromptView {
    pub storey_idx: usize,
    pub key: ber_model::ber::fenestration::PorchKey,
    /// e.g. "porch 1 (Ground floor)".
    pub label: String,
    pub question: PorchQuestion,
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
    /// Categories the assessor flagged as PRESENT this review ("No — there is
    /// one") that still have zero recorded objects. While non-empty, the
    /// "All reviewed & OK" confirm is blocked — the flagged item must be added
    /// first (see [`AppController::unresolved_present_categories`]).
    pub unresolved_present: Vec<ber_model::ber::objects::ObjectCategory>,
    /// Whether the "Anything in the attic?" question is still pending (attic
    /// bucket empty, not confirmed-empty, not flagged this session). Rendered
    /// after the per-category absence prompts.
    pub attic_prompt: bool,
    /// The assessor flagged the attic as holding something ("Yes — there is")
    /// but hasn't recorded an attic object yet — blocks the confirm alongside
    /// [`unresolved_present`](Self::unresolved_present).
    pub attic_unresolved: bool,
    /// The next pending porch question (DEAP Table 2.2 evidence still
    /// missing), asked after the absence + attic prompts. `None` when every
    /// porch is decided or deferred.
    pub porch_prompt: Option<PorchPromptView>,
    /// Pending porch questions (incl. the current one).
    pub porch_remaining: usize,
    /// Porch questions deferred this review ("Yes — I'll record it") whose
    /// promised door/heater still isn't recorded — blocks the confirm
    /// alongside [`unresolved_present`](Self::unresolved_present).
    pub porch_unresolved: Vec<String>,
    /// Item 6 gate: no primary heat source is placed (no boiler AND no panel
    /// heater). Blocks the confirm — the review shows "add the main heat
    /// source" until one exists.
    pub heating_source_missing: bool,
    /// Item 4: the "is the house heated by panel heaters?" question is pending
    /// (a panel heater but no boiler, unanswered this review). Blocks the
    /// confirm until answered.
    pub panel_heater_prompt: bool,
    /// Item 5: the distinct primary heat-source candidates to pick between when
    /// ≥2 are present and none has been chosen this review. Empty when there's
    /// nothing to ask; while non-empty it blocks the confirm.
    pub primary_pick_options: Vec<HeatingSystemType>,
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
        // A canvas tap while a room box is already OPEN just DISMISSES it — a
        // tap outside the box is most likely a mis-tap, so don't spawn a stray
        // pin there (nor jump to another). Close it (pruning an untouched pin);
        // the NEXT tap opens or creates.
        if self.active_object_room.is_some() {
            self.close_object_room();
            return;
        }
        // Open the nearest existing pin within tolerance. ROOF pins are hidden
        // from the 2D plan (they belong to the roof, not a storey — see
        // `object_rooms_view`), so an invisible pin must not swallow the tap;
        // reopen those from a 3D view instead.
        if let Some(record) = self.current_record.as_ref() {
            if let Some(storey) = record.data.storeys.get(idx) {
                let near = storey
                    .object_rooms
                    .iter()
                    .filter(|r| !r.is_roof_mounted(storey.ceiling_height_m))
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
    /// else drop a new room ON the surface — carrying its height, for a
    /// sloped surface (roof plane / sloped ceiling) its pitch for the dialog,
    /// and `on_roof` when the pick landed on a dwelling ROOF plane (roof pins
    /// are hidden from the per-storey views — solar panels aren't storey
    /// objects).
    #[allow(clippy::too_many_arguments)]
    pub fn objects_pointer_down_3d(
        &mut self,
        storey_idx: usize,
        wx: f64,
        wy: f64,
        z_m: f64,
        pitch_deg: Option<f32>,
        on_roof: bool,
        tol: f64,
    ) {
        // A tap while a room box is OPEN dismisses it (see the 2D handler) —
        // don't drop a stray pin on a mis-tap; the next tap opens or creates.
        if self.active_object_room.is_some() {
            self.close_object_room();
            return;
        }
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
        let room = ObjectRoom::new_on_surface(
            Point2f32::new(wx as f32, wy as f32),
            z_m,
            pitch_deg,
            on_roof,
        );
        let id = room.id;
        storey.object_rooms.push(room);
        self.active_object_room = Some(id);
        let _ = self.mark_dirty();
        self.push_history_snapshot(snap);
    }

    /// Close the open room (the panel's "Done" — or a tap that dismisses it).
    /// A pin that never received an object is an accidental / dismissed tap, so
    /// it's dropped rather than left on the plan as an empty "0" hotspot.
    pub fn close_object_room(&mut self) {
        if let Some(id) = self.active_object_room.take() {
            self.prune_object_room_if_empty(id);
        }
    }

    /// Remove object room `room_id` iff it holds no objects. Searches every
    /// storey (a 3D-placed pin can live off the current storey) and the attic
    /// bucket (an attic dismissed empty disappears — the review then still
    /// asks "anything in the attic?"). Persists the removal; deliberately NOT
    /// snapshotted — placing then dismissing a bare pin isn't a meaningful
    /// edit to undo.
    fn prune_object_room_if_empty(&mut self, room_id: ItemId) {
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let mut removed = false;
        for storey in &mut record.data.storeys {
            let before = storey.object_rooms.len();
            storey
                .object_rooms
                .retain(|r| r.id != room_id || !r.objects.is_empty());
            removed |= storey.object_rooms.len() != before;
        }
        if record
            .data
            .attic_room
            .as_ref()
            .is_some_and(|r| r.id == room_id && r.objects.is_empty())
        {
            record.data.attic_room = None;
            removed = true;
        }
        if removed {
            let _ = self.mark_dirty();
        }
    }

    /// Open the house-level ATTIC bucket (the Objects-stage "Attic" button),
    /// creating it on first use. The attic isn't a storey: its objects live on
    /// `Ber::attic_room` and it has no plan pin. The usual entry panel then
    /// adds objects to it like any room.
    pub fn open_attic_room(&mut self) {
        let snap = self.current_history_snapshot();
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let created = record.data.attic_room.is_none();
        let room = record
            .data
            .attic_room
            .get_or_insert_with(ObjectRoom::new_attic);
        self.active_object_room = Some(room.id);
        if created {
            let _ = self.mark_dirty();
            self.push_history_snapshot(snap);
        }
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
        let stored: Vec<Evidence> = photos
            .iter()
            .filter_map(|(p, quality)| self.ingest_photo(p, *quality))
            .collect();
        let snap = self.current_history_snapshot();
        if let Some(room) = self.object_room_mut(room_id) {
            // Whether this is a HeatingZones record — captured before `kind` is
            // moved so the zone-count prompt can be raised after the push.
            let is_zones = matches!(kind, ObjectKind::HeatingZones);
            let mut obj = BuildingObject::new(kind, count);
            obj.note = note.filter(|s| !s.trim().is_empty());
            obj.photos = stored;
            let new_id = obj.id;
            room.objects.push(obj);
            self.resync_heating_from_objects();
            self.resync_ventilation_from_objects();
            // A recorded HeatingZones object raises the "how many zones?"
            // numeric prompt (item 3) — its answer lands in `controls.zones`.
            if is_zones {
                self.zone_prompt = Some(new_id);
            }
            let _ = self.mark_dirty();
            self.push_history_snapshot(snap);
        }
    }

    /// Reflect the placed boiler/heat-pump objects into the BER's
    /// `heating_system` (see [`Ber::sync_heating_system_from_objects`]). Called
    /// after any object add/remove so the field tracks the survey.
    fn resync_heating_from_objects(&mut self) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.sync_heating_system_from_objects();
        }
    }

    /// Reflect placed centralised ventilation units into the BER's
    /// `ventilation_system` (see [`Ber::sync_ventilation_system_from_objects`]).
    /// Fully deterministic re-derivation — called after any object add/remove.
    fn resync_ventilation_from_objects(&mut self) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.sync_ventilation_system_from_objects();
        }
    }

    /// Delete one object from a room (the Delete panel).
    pub fn delete_object(&mut self, room_id: ItemId, object_id: ItemId) {
        let snap = self.current_history_snapshot();
        if let Some(room) = self.object_room_mut(room_id) {
            let before = room.objects.len();
            room.objects.retain(|o| o.id != object_id);
            if room.objects.len() != before {
                self.resync_heating_from_objects();
                self.resync_ventilation_from_objects();
                self.reset_zones_if_no_heating_zones();
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
        if record
            .data
            .attic_room
            .as_ref()
            .is_some_and(|r| r.id == room_id)
        {
            record.data.attic_room = None;
            removed = true;
        }
        if removed {
            if self.active_object_room == Some(room_id) {
                self.active_object_room = None;
            }
            self.resync_heating_from_objects();
            self.resync_ventilation_from_objects();
            self.reset_zones_if_no_heating_zones();
            let _ = self.mark_dirty();
            self.push_history_snapshot(snap);
        }
    }

    /// Room pins on the CURRENT storey, for the plan overlay and the
    /// single-storey 3D hotspots. ROOF pins are excluded — a solar panel on
    /// the roof isn't a storey object; it shows in the full-house 3D view
    /// (and its world point) instead, where the roof itself is drawn.
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
            .filter(|r| !r.is_roof_mounted(storey.ceiling_height_m))
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
    /// other than the current one — plus the attic bucket.
    pub fn active_object_room_view(&self) -> Option<ActiveObjectRoomView> {
        let room_id = self.active_object_room?;
        let data = &self.current_record.as_ref()?.data;
        let room = data.object_rooms_iter().find(|r| r.id == room_id)?;
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
                    kind: o.kind.clone(),
                    separate_hw_control: o.separate_hw_control,
                })
                .collect(),
            categories_present: room.categories_present(),
            surface_pitch_deg: room.surface_pitch_deg,
            is_attic: data.attic_room.as_ref().is_some_and(|a| a.id == room.id),
        })
    }

    /// The distinct object KINDS already recorded house-wide — the objects
    /// panel combines this with each kind's [`ObjectKind::photo_policy`] to
    /// decide whether the next instance needs a photo (drives the auto-camera
    /// and the OK gate). A first-instance kind whose kind is in here is
    /// satisfied; an always-required kind isn't, regardless.
    pub fn recorded_object_kinds(&self) -> Vec<ObjectKind> {
        let mut kinds: Vec<ObjectKind> = Vec::new();
        if let Some(record) = self.current_record.as_ref() {
            for room in record.data.object_rooms_iter() {
                for o in &room.objects {
                    if !kinds.contains(&o.kind) {
                        kinds.push(o.kind.clone());
                    }
                }
            }
        }
        kinds
    }

    /// Whether a photo is mandatory for the NEXT instance of `kind` (its
    /// [`ObjectKind::photo_policy`] combined with whether one is already
    /// recorded house-wide). The objects panel uses this to auto-open the
    /// camera and gate OK.
    pub fn object_photo_required(&self, kind: &ObjectKind) -> bool {
        kind.photo_required(self.recorded_object_kinds().contains(kind))
    }

    /// Mutable borrow of a room by id, searching every storey (a pin placed
    /// from the full-house 3D view can live off the current storey) plus the
    /// attic bucket.
    fn object_room_mut(&mut self, room_id: ItemId) -> Option<&mut ObjectRoom> {
        self.current_record
            .as_mut()?
            .data
            .object_rooms_iter_mut()
            .find(|r| r.id == room_id)
    }

    // ── Review ────────────────────────────────────────────────────────────────

    /// Whether any object room exists house-wide — a storey pin or the attic
    /// bucket (gates the review entry).
    pub fn has_object_rooms(&self) -> bool {
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.object_rooms_iter().next().is_some())
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
            self.object_review_attic_skipped = false;
            self.porch_review_skipped.clear();
            // Re-offer the heating stage questions (items 4/5) each review.
            self.panel_heater_asked = false;
            self.heating_primary_asked = false;
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
    /// Until an object of the category IS added, the flag blocks the review's
    /// confirm (see [`unresolved_present_categories`](Self::unresolved_present_categories)).
    pub fn skip_object_absent(&mut self, cat: ber_model::ber::objects::ObjectCategory) {
        self.object_review_skipped.insert(cat);
    }

    /// Confirm the attic holds NOTHING ("Anything in the attic?" → No, empty).
    /// Recorded durably, like a category's `confirm_object_absent`.
    pub fn confirm_attic_absent(&mut self) {
        if let Some(record) = self.current_record.as_mut() {
            if !record.data.attic_no_items_confirmed {
                record.data.attic_no_items_confirmed = true;
                let _ = self.mark_dirty();
            }
        }
    }

    /// "Anything in the attic?" → Yes, there is — I'll add it. Session-only,
    /// like [`skip_object_absent`](Self::skip_object_absent); blocks the
    /// review's confirm until an attic object is actually recorded (see
    /// [`attic_unresolved`](Self::attic_unresolved)).
    pub fn skip_attic_absent(&mut self) {
        self.object_review_attic_skipped = true;
    }

    /// Whether the assessor asserted the attic holds something ("Anything in
    /// the attic?" → Yes) but hasn't recorded an attic object yet — the attic
    /// counterpart of [`unresolved_present_categories`](Self::unresolved_present_categories);
    /// blocks the review's confirm while true.
    pub fn attic_unresolved(&self) -> bool {
        self.object_review_attic_skipped
            && self
                .current_record
                .as_ref()
                .is_none_or(|r| attic_is_empty(&r.data))
    }

    // ── Porch checks (DEAP Table 2.2 evidence) ──────────────────────────

    /// Every PENDING porch question: a fully-protruding porch (the only kind
    /// Table 2.2 can exclude — within-line / corner porches are always
    /// included, so nothing needs asking) whose separating-door or
    /// fixed-heating fact has neither positive evidence nor a
    /// confirmed-absent answer, minus the ones deferred this review.
    pub fn pending_porch_questions(&self) -> Vec<PorchPromptView> {
        use ber_model::ber::structural::porch_facts;
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (idx, s) in record.data.storeys.iter().enumerate() {
            for (i, porch) in s.porches.regions.iter().enumerate() {
                let facts = porch_facts(&record.data, idx, porch);
                if !facts.fully_protruding {
                    continue;
                }
                let label = if s.porches.regions.len() == 1 {
                    format!("the porch ({})", storey_display_name(s, idx))
                } else {
                    format!("porch {} ({})", i + 1, storey_display_name(s, idx))
                };
                let mut push = |question: PorchQuestion| {
                    if !self.porch_review_skipped.contains(&(idx, facts.key, question)) {
                        out.push(PorchPromptView {
                            storey_idx: idx,
                            key: facts.key,
                            label: label.clone(),
                            question,
                        });
                    }
                };
                if facts.separating_door.is_none() {
                    push(PorchQuestion::SeparatingDoor);
                }
                if facts.heated.is_none() {
                    push(PorchQuestion::FixedHeating);
                }
            }
        }
        out
    }

    /// Confirm a porch fact ABSENT ("No — open access" / "No — unheated"):
    /// record the durable [`PorchCheck`] and re-run the exposure derivation so
    /// a now-decided porch stamps its geometry immediately.
    ///
    /// [`PorchCheck`]: ber_model::ber::structural::PorchCheck
    pub fn confirm_porch_absent(
        &mut self,
        storey_idx: usize,
        key: ber_model::ber::fenestration::PorchKey,
        question: PorchQuestion,
    ) {
        let mut changed = false;
        if let Some(record) = self.current_record.as_mut() {
            if let Some(storey) = record.data.storeys.get_mut(storey_idx) {
                let check = match storey.porch_checks.iter_mut().find(|c| c.key == key) {
                    Some(c) => c,
                    None => {
                        storey
                            .porch_checks
                            .push(ber_model::ber::structural::PorchCheck {
                                key,
                                ..Default::default()
                            });
                        storey.porch_checks.last_mut().expect("just pushed")
                    }
                };
                let flag = match question {
                    PorchQuestion::SeparatingDoor => &mut check.door_confirmed_absent,
                    PorchQuestion::FixedHeating => &mut check.heating_confirmed_absent,
                };
                if !*flag {
                    *flag = true;
                    changed = true;
                }
            }
        }
        if changed {
            // A decided porch re-stamps its exposures (Irrelevant /
            // SemiExposed) in the same derive tail the storey pipeline uses.
            self.derive_horizontal_exposure_all();
            let _ = self.mark_dirty();
        }
    }

    /// Defer a porch question this review ("Yes — I'll record it"): the
    /// promised door/heater must actually be recorded before the review can
    /// confirm (see [`porch_unresolved_labels`](Self::porch_unresolved_labels)).
    /// Session-only, like [`skip_object_absent`](Self::skip_object_absent).
    pub fn skip_porch_check(
        &mut self,
        storey_idx: usize,
        key: ber_model::ber::fenestration::PorchKey,
        question: PorchQuestion,
    ) {
        self.porch_review_skipped.insert((storey_idx, key, question));
    }

    /// Deferred porch questions whose promised evidence STILL isn't recorded
    /// — "porch door (Ground floor)" etc. Blocks the review confirm.
    pub fn porch_unresolved_labels(&self) -> Vec<String> {
        use ber_model::ber::structural::porch_facts;
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (idx, key, question) in &self.porch_review_skipped {
            let Some(s) = record.data.storeys.get(*idx) else {
                continue;
            };
            let still_pending = s.porches.regions.iter().any(|p| {
                let facts = porch_facts(&record.data, *idx, p);
                facts.key == *key
                    && match question {
                        PorchQuestion::SeparatingDoor => facts.separating_door.is_none(),
                        PorchQuestion::FixedHeating => facts.heated.is_none(),
                    }
            });
            if still_pending {
                out.push(match question {
                    PorchQuestion::SeparatingDoor => {
                        format!("porch door ({})", storey_display_name(s, *idx))
                    }
                    PorchQuestion::FixedHeating => {
                        format!("porch heater ({})", storey_display_name(s, *idx))
                    }
                });
            }
        }
        out
    }

    /// Categories the assessor asserted are PRESENT during this review ("No —
    /// there is one") that still have zero recorded objects house-wide. The
    /// assertion is a promise to add the item: while any remain, the review
    /// cannot be confirmed (and the Final phase stays gated).
    pub fn unresolved_present_categories(&self) -> Vec<ObjectCategory> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let present = recorded_categories(&record.data);
        ObjectCategory::ALL
            .into_iter()
            .filter(|c| self.object_review_skipped.contains(c) && !present.contains(c))
            .collect()
    }

    /// Whether the Objects review has been confirmed ("all reviewed & OK").
    pub fn objects_reviewed(&self) -> bool {
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.objects_reviewed)
    }

    /// Confirm the Objects review (counts + absences all checked): record it on
    /// the dwelling and close the review. Refused while a category the assessor
    /// flagged as present ("No — there is one") still has no recorded object,
    /// or the attic was flagged as holding something still unrecorded — the
    /// promised item must be added first (the UI disables the button too;
    /// this is the authoritative guard).
    pub fn confirm_objects_review(&mut self) {
        if !self.unresolved_present_categories().is_empty()
            || self.attic_unresolved()
            || !self.heating_stage_ready()
            || !self.pending_porch_questions().is_empty()
            || !self.porch_unresolved_labels().is_empty()
        {
            return;
        }
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
        let present = recorded_categories(data);
        let remaining: Vec<ObjectCategory> = ObjectCategory::ALL
            .into_iter()
            .filter(|c| {
                *c != ObjectCategory::Other
                    && !present.contains(c)
                    && !data.objects_no_items_confirmed.contains(c)
                    && !self.object_review_skipped.contains(c)
            })
            .collect();
        // The attic question ("anything in the attic?") follows the same
        // pattern, pending while the attic bucket is empty and neither
        // confirmed-empty nor flagged this session.
        let attic_prompt = attic_is_empty(data)
            && !data.attic_no_items_confirmed
            && !self.object_review_attic_skipped;
        let porch_pending = self.pending_porch_questions();
        Some(ObjectReviewView {
            totals,
            selected,
            hotspots,
            absent_prompt: remaining.first().copied(),
            absent_remaining: remaining.len(),
            unresolved_present: self.unresolved_present_categories(),
            attic_prompt,
            attic_unresolved: self.attic_unresolved(),
            porch_remaining: porch_pending.len(),
            porch_prompt: porch_pending.into_iter().next(),
            porch_unresolved: self.porch_unresolved_labels(),
            heating_source_missing: self.heating_source_missing(),
            panel_heater_prompt: self.panel_heater_prompt_pending(),
            primary_pick_options: self.primary_pick_options(),
        })
    }

    // ── Heating-system wiring (items 2–6) ───────────────────────────────────

    /// Set the "separate control for water & space" flag
    /// ([`BuildingObject::separate_hw_control`]) on one object — a Boiler or
    /// Programmer — and re-derive `heating_system` (item 2). The model owns the
    /// derivation ([`Ber::sync_heating_system_from_objects`]); the controller
    /// just flips the flag and sequences.
    pub fn set_object_separate_hw_control(&mut self, room_id: ItemId, object_id: ItemId, on: bool) {
        let snap = self.current_history_snapshot();
        let mut changed = false;
        if let Some(room) = self.object_room_mut(room_id) {
            if let Some(obj) = room.objects.iter_mut().find(|o| o.id == object_id) {
                if obj.separate_hw_control != on {
                    obj.separate_hw_control = on;
                    changed = true;
                }
            }
        }
        if changed {
            self.resync_heating_from_objects();
            let _ = self.mark_dirty();
            self.push_history_snapshot(snap);
        }
    }

    /// Whether the "how many heating zones?" numeric prompt is open — raised
    /// when a `HeatingZones` object is recorded (item 3). The number box reads
    /// this.
    pub fn heating_zones_prompt_open(&self) -> bool {
        self.zone_prompt.is_some()
    }

    /// Record the zone count from the prompt into `heating_system.controls.zones`
    /// (the sync deliberately doesn't own this field) and close the prompt.
    /// Clamped to ≥1 — the prompt only opens once a zones object exists.
    pub fn confirm_heating_zones(&mut self, count: u32) {
        self.zone_prompt = None;
        if let Some(record) = self.current_record.as_mut() {
            record.data.heating_system.controls.zones = count.max(1);
            let _ = self.mark_dirty();
        }
    }

    /// Dismiss the heating-zones prompt without recording a count (leaves
    /// `controls.zones` unchanged).
    pub fn cancel_heating_zones(&mut self) {
        self.zone_prompt = None;
    }

    /// After a heating-object removal, zero `controls.zones` once the LAST
    /// `HeatingZones` object is gone — the sync doesn't own this field, so the
    /// controller clears it here (item 3). No-op while any zones object remains.
    fn reset_zones_if_no_heating_zones(&mut self) {
        if !self.has_object_kind(|k| matches!(k, ObjectKind::HeatingZones)) {
            if let Some(record) = self.current_record.as_mut() {
                record.data.heating_system.controls.zones = 0;
            }
            self.zone_prompt = None;
        }
    }

    /// Whether any recorded object (house-wide, attic included) matches `pred`.
    fn has_object_kind(&self, pred: impl Fn(&ObjectKind) -> bool) -> bool {
        self.current_record.as_ref().is_some_and(|r| {
            r.data
                .object_rooms_iter()
                .flat_map(|room| room.objects.iter())
                .any(|o| pred(&o.kind))
        })
    }

    /// Item 6 gate: no primary heat source placed (no boiler AND no panel
    /// heater). A chimney is secondary-only, so a chimney-only house still hits
    /// this. `false` when no record is loaded (nothing to gate).
    /// No PRIMARY heating is resolved (`system_type == Invalid`) and no heating
    /// question is currently pending — the state where the review must block and
    /// the "add the main heat source" message shows. Covers both "nothing
    /// placed" and "a panel heater was declined as the main heating" (item 4
    /// "No" → the panel is secondary, so there's still no primary).
    pub fn heating_source_missing(&self) -> bool {
        let Some(record) = self.current_record.as_ref() else {
            return false;
        };
        record.data.heating_system.system_type
            == ber_model::ber::systems::HeatingSystemType::Invalid
            && !self.panel_heater_prompt_pending()
            && self.primary_pick_options().is_empty()
    }

    /// Item 4: the "is the house heated by panel heaters?" question is pending —
    /// a panel heater is placed with no boiler, unanswered this review. A panel
    /// is not a primary heat source until this is answered "yes"; "no" leaves it
    /// as a secondary source (and the heat-source gate keeps blocking).
    pub fn panel_heater_prompt_pending(&self) -> bool {
        !self.panel_heater_asked
            && self.has_object_kind(|k| matches!(k, ObjectKind::PanelHeater))
            && !self.has_object_kind(|k| matches!(k, ObjectKind::Boiler(_)))
    }

    /// Answer the panel-heater question (item 4). "Yes" makes the electric panel
    /// the primary (`ElectricHeater`, which the sync then preserves). "No" means
    /// the panel isn't the main heating: drop any provisional electric primary
    /// so the sync files the panel under `secondary_heating` and the heat-source
    /// gate blocks until a real primary is recorded. Either way the question is
    /// done for this review.
    pub fn answer_panel_heater_primary(&mut self, yes: bool) {
        self.panel_heater_asked = true;
        if let Some(record) = self.current_record.as_mut() {
            if yes {
                record.data.heating_system.system_type = HeatingSystemType::ElectricHeater;
            } else if record.data.heating_system.system_type == HeatingSystemType::ElectricHeater {
                record.data.heating_system.system_type = HeatingSystemType::Invalid;
            }
            record.data.sync_heating_system_from_objects();
            let _ = self.mark_dirty();
        }
    }

    /// Item 5: the distinct primary heat-source candidates to choose between —
    /// non-empty only when ≥2 are present and the assessor hasn't picked one
    /// this review. Empty otherwise (no prompt).
    pub fn primary_pick_options(&self) -> Vec<HeatingSystemType> {
        if self.heating_primary_asked {
            return Vec::new();
        }
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        // Only genuine PRIMARY candidates (boilers, plus an affirmed panel) —
        // an unaffirmed panel is secondary, never offered as a primary choice.
        let candidates = record.data.primary_heating_candidates();
        if candidates.len() >= 2 {
            candidates
        } else {
            Vec::new()
        }
    }

    /// Choose the primary heat source (item 5): set `system_type` to `ty` (the
    /// sync preserves it and files the rest under `secondary_heating`) and mark
    /// the pick done for this review.
    pub fn pick_primary_heat_source(&mut self, ty: HeatingSystemType) {
        self.heating_primary_asked = true;
        if let Some(record) = self.current_record.as_mut() {
            record.data.heating_system.system_type = ty;
            record.data.sync_heating_system_from_objects();
            let _ = self.mark_dirty();
        }
    }

    /// Whether the heating stage is resolved enough to finish the Objects review:
    /// a primary source exists (item 6) and the panel-heater / primary-pick
    /// questions (items 4/5) are answered.
    pub fn heating_stage_ready(&self) -> bool {
        !self.heating_source_missing()
            && !self.panel_heater_prompt_pending()
            && self.primary_pick_options().is_empty()
    }
}

/// Whether the attic bucket holds no objects (no bucket, or an empty one).
/// A storey's display name for porch-question labels — its recorded name,
/// falling back to "storey N".
fn storey_display_name(s: &Storey, idx: usize) -> String {
    if s.name.trim().is_empty() {
        format!("storey {}", idx + 1)
    } else {
        s.name.clone()
    }
}

fn attic_is_empty(data: &Ber) -> bool {
    data.attic_room
        .as_ref()
        .is_none_or(|r| r.objects.is_empty())
}

/// The categories with at least one recorded object, house-wide (attic
/// included).
fn recorded_categories(data: &Ber) -> std::collections::HashSet<ObjectCategory> {
    data.object_rooms_iter()
        .flat_map(|r| r.objects.iter())
        .map(|o| o.kind.category())
        .collect()
}

/// House-wide object totals as (label, count), in first-seen order (attic
/// included).
fn object_label_totals(data: &Ber) -> Vec<(String, u32)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for room in data.object_rooms_iter() {
        for o in &room.objects {
            let label = o.kind.label();
            if !counts.contains_key(&label) {
                order.push(label.clone());
            }
            *counts.entry(label).or_default() += o.count;
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
