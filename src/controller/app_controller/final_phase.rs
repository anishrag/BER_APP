//! Phase-4 ("Final") controller logic: the dropdown form values, per-side
//! sheltering, north orientation, and the completion flag. Thin — sets the
//! house-level fields on `Ber` (Layer 1) and persists; the UI renders the form /
//! sheltered-sides / north-align steps and calls these.
use super::*;
use ber_model::ber::common::ExternalSide;
use ber_model::ber::final_details::{
    FloorKind, HeatingKind, ShelterCategory, ShelteringBySide, WallFinish,
};

/// Which wall-finish field the form is setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallFinishField {
    Internal,
    Separating,
    External,
}

/// The north-align satellite image + its current transform over the fixed plan,
/// for the canvas overlay. `rotation_deg` is the recorded north (orientation).
pub struct NorthImageView {
    pub path: String,
    pub offset: (f32, f32),
    pub zoom: f32,
    pub rotation_deg: f32,
}

impl AppController {
    /// Whether the Final-phase review has been confirmed.
    pub fn details_reviewed(&self) -> bool {
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.details_reviewed)
    }

    /// The Phase-4 form values (defaults when no record).
    pub fn final_details(&self) -> ber_model::ber::final_details::FinalDetails {
        self.current_record
            .as_ref()
            .map(|r| r.data.final_details)
            .unwrap_or_default()
    }

    /// Per-side sheltering (defaults when no record).
    pub fn sheltering(&self) -> ShelteringBySide {
        self.current_record
            .as_ref()
            .map(|r| r.data.sheltering)
            .unwrap_or_default()
    }

    /// The dwelling orientation (degrees from true north) used to align the plan.
    pub fn orientation_deg(&self) -> f32 {
        self.current_record
            .as_ref()
            .map(|r| r.data.orientation_deg)
            .unwrap_or(0.0)
    }

    /// The pre-populated north-up site image (`file_path`), if any.
    pub fn site_satellite_image_path(&self) -> Option<String> {
        self.current_record
            .as_ref()
            .and_then(|r| r.data.site_satellite_image.as_ref())
            .map(|p| p.file_path.clone())
    }

    pub fn set_final_floor(&mut self, floor: FloorKind) {
        if let Some(r) = self.current_record.as_mut() {
            r.data.final_details.floor = floor;
            let _ = self.mark_dirty();
        }
    }

    pub fn set_final_wall_finish(&mut self, field: WallFinishField, finish: WallFinish) {
        if let Some(r) = self.current_record.as_mut() {
            match field {
                WallFinishField::Internal => r.data.final_details.internal_wall = finish,
                WallFinishField::Separating => r.data.final_details.separating_wall = finish,
                WallFinishField::External => r.data.final_details.external_wall = finish,
            }
            let _ = self.mark_dirty();
        }
    }

    pub fn set_final_heating(&mut self, heating: HeatingKind) {
        if let Some(r) = self.current_record.as_mut() {
            r.data.final_details.heating = heating;
            let _ = self.mark_dirty();
        }
    }

    /// Set the orientation (true-north offset, degrees) from the north-align step.
    pub fn set_orientation_deg(&mut self, deg: f32) {
        if let Some(r) = self.current_record.as_mut() {
            r.data.orientation_deg = deg.rem_euclid(360.0);
            let _ = self.mark_dirty();
        }
    }

    /// The north-align image + transform for the canvas overlay, or `None` when
    /// no site image is present on the record.
    pub fn north_image(&self) -> Option<NorthImageView> {
        let path = self.site_satellite_image_path()?;
        Some(NorthImageView {
            path,
            offset: self.north_img_offset,
            zoom: self.north_img_zoom,
            rotation_deg: self.orientation_deg(),
        })
    }

    /// Apply one frame of the north-align gesture: pan (canvas px) + zoom factor
    /// (session-only) + rotation delta (degrees → the persisted orientation).
    pub fn apply_north_gesture(
        &mut self,
        pan_dx: f32,
        pan_dy: f32,
        zoom_factor: f32,
        rotate_deg: f32,
    ) {
        self.north_img_offset.0 += pan_dx;
        self.north_img_offset.1 += pan_dy;
        if zoom_factor.is_finite() && zoom_factor > 0.0 {
            self.north_img_zoom = (self.north_img_zoom * zoom_factor).clamp(0.05, 40.0);
        }
        if rotate_deg != 0.0 {
            if let Some(r) = self.current_record.as_mut() {
                r.data.orientation_deg = (r.data.orientation_deg + rotate_deg).rem_euclid(360.0);
                let _ = self.mark_dirty();
            }
        }
    }

    /// Recenter the north-align image (reset pan + zoom; keeps the rotation).
    pub fn reset_north_image(&mut self) {
        self.north_img_offset = (0.0, 0.0);
        self.north_img_zoom = 1.0;
    }

    /// Advance one side's shelter category on a tap (Unset→Sheltered→Half→
    /// Unsheltered→Sheltered…), keeping the legacy `sheltered_sides` count in sync.
    pub fn cycle_shelter(&mut self, side: ExternalSide) {
        if let Some(r) = self.current_record.as_mut() {
            let next = r.data.sheltering.get(side).next();
            r.data.sheltering.set(side, next);
            r.data.sheltered_sides = r.data.sheltering.sheltered_count();
            let _ = self.mark_dirty();
        }
    }

    /// Directly set one side's shelter category (keeping `sheltered_sides` synced).
    pub fn set_shelter(&mut self, side: ExternalSide, c: ShelterCategory) {
        if let Some(r) = self.current_record.as_mut() {
            r.data.sheltering.set(side, c);
            r.data.sheltered_sides = r.data.sheltering.sheltered_count();
            let _ = self.mark_dirty();
        }
    }

    /// Confirm the Final review — record the dwelling Final-reviewed (BER complete).
    pub fn confirm_final_review(&mut self) {
        if let Some(r) = self.current_record.as_mut() {
            r.data.details_reviewed = true;
            let _ = self.mark_dirty();
        }
    }

    /// The Phase-4 checklist: progress rows for the form, north, and sheltering.
    pub fn final_details_checklist(&self) -> Vec<ChecklistItem> {
        let d = self.final_details();
        let form_done = d.floor != FloorKind::Unset
            && d.internal_wall != WallFinish::Unset
            && d.separating_wall != WallFinish::Unset
            && d.external_wall != WallFinish::Unset
            && d.heating != HeatingKind::Unset;
        let north_done = self.orientation_deg() != 0.0;
        let shelter_done = self.sheltering().all_set();
        let dot = |b: bool| {
            if b {
                ChecklistStatus::AllStoreys
            } else {
                ChecklistStatus::Incomplete
            }
        };
        vec![
            ChecklistItem {
                label: "Details form",
                status: dot(form_done),
            },
            ChecklistItem {
                label: "North aligned",
                status: dot(north_done),
            },
            ChecklistItem {
                label: "Sheltered sides",
                status: dot(shelter_done),
            },
        ]
    }
}
