//! Phase-4 ("Final") controller logic: the dropdown form values, per-side
//! sheltering, north orientation, and the completion flag. Thin — sets the
//! house-level fields on `Ber` (Layer 1) and persists; the UI renders the form /
//! sheltered-sides / north-align steps and calls these.
use super::*;
use ber_model::ber::common::{Evidence, ExternalSide};
use ber_model::ber::final_details::{FloorKind, HeatingKind, ShelteringBySide, WallFinish};

/// Which wall-finish field the form is setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallFinishField {
    Internal,
    Separating,
    External,
}

/// The Phase-4 form values gathered for the UI. Post-refactor these no longer
/// live in one model struct: floor + the three wall finishes come from
/// `Ber::thermal_mass_info` (raw capture), and heating from
/// `Ber::heating_system.distribution`. This is a read-only view assembled from
/// both for the panel; the setters write back to the two homes.
#[derive(Debug, Clone, Copy, Default)]
pub struct FinalDetails {
    pub floor: FloorKind,
    pub internal_wall: WallFinish,
    pub separating_wall: WallFinish,
    pub external_wall: WallFinish,
    pub heating: HeatingKind,
}

/// What the Final phase view is doing: showing the form (default), aligning the
/// satellite image (gestures move the image), or marking sheltered sides (taps
/// on the plan's sides).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FinalMode {
    #[default]
    Form,
    Align,
    Sheltered,
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

    /// The Phase-4 form values (defaults when no record) — assembled from the
    /// raw `thermal_mass_info` capture and `heating_system.distribution`.
    pub fn final_details(&self) -> FinalDetails {
        self.current_record
            .as_ref()
            .map(|r| {
                let t = &r.data.thermal_mass_info;
                FinalDetails {
                    floor: t.floor,
                    internal_wall: t.internal_wall,
                    separating_wall: t.separating_wall,
                    external_wall: t.external_wall,
                    heating: r.data.heating_system.distribution,
                }
            })
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
            r.data.thermal_mass_info.floor = floor;
            let _ = self.mark_dirty();
        }
    }

    pub fn set_final_wall_finish(&mut self, field: WallFinishField, finish: WallFinish) {
        if let Some(r) = self.current_record.as_mut() {
            match field {
                WallFinishField::Internal => r.data.thermal_mass_info.internal_wall = finish,
                WallFinishField::Separating => r.data.thermal_mass_info.separating_wall = finish,
                WallFinishField::External => r.data.thermal_mass_info.external_wall = finish,
            }
            let _ = self.mark_dirty();
        }
    }

    pub fn set_final_heating(&mut self, heating: HeatingKind) {
        if let Some(r) = self.current_record.as_mut() {
            r.data.heating_system.distribution = heating;
            let _ = self.mark_dirty();
        }
    }

    /// The current Phase-4 sub-mode (form / align / sheltered).
    pub fn final_mode(&self) -> FinalMode {
        self.final_mode
    }

    /// Switch the Phase-4 sub-mode (e.g. enter align / sheltered, or back to form).
    pub fn set_final_mode(&mut self, mode: FinalMode) {
        self.final_mode = mode;
    }

    /// Finish aligning: record the north as confirmed and return to the form.
    pub fn confirm_align(&mut self) {
        if let Some(r) = self.current_record.as_mut() {
            r.data.north_aligned = true;
            let _ = self.mark_dirty();
        }
        self.final_mode = FinalMode::Form;
    }

    /// Whether the plan's north has been aligned + confirmed.
    pub fn north_aligned(&self) -> bool {
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.north_aligned)
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

    /// DEV/practice helper: generate a fake north-up satellite image (a building
    /// footprint + roads + a blue "north" strip along the top) and attach it as
    /// the site image, so the north-align step can be tried without a real one.
    pub fn load_test_satellite_image(&mut self) {
        let (w, h) = (600u32, 600u32);
        let mut img = image::RgbImage::from_pixel(w, h, image::Rgb([70, 95, 70]));
        let mut fill = |x0: u32, x1: u32, y0: u32, y1: u32, c: [u8; 3]| {
            for y in y0..y1.min(h) {
                for x in x0..x1.min(w) {
                    img.put_pixel(x, y, image::Rgb(c));
                }
            }
        };
        fill(0, w, 270, 330, [120, 120, 120]); // horizontal road
        fill(150, 210, 0, h, [120, 120, 120]); // vertical road
        fill(330, 520, 360, 500, [160, 130, 95]); // building footprint
        fill(0, w, 0, 16, [60, 90, 200]); // north strip (top = north)

        let dir = self.photos_dir.clone();
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join(format!("test_satellite_{}.png", ItemId::default().0));
        if img.save(&path).is_err() {
            return;
        }
        let file_path = std::fs::canonicalize(&path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());
        if let Some(r) = self.current_record.as_mut() {
            r.data.site_satellite_image = Some(Evidence {
                id: ItemId::default(),
                file_path,
                thumbnail_path: None,
                description: Some("Test satellite (practice)".to_string()),
                side: None,
                content_type: None,
            });
            let _ = self.mark_dirty();
        }
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

    /// Confirm the Final review — record the dwelling Final-reviewed (BER
    /// complete) and raise the completion popup.
    ///
    /// DEFERRED-PHOTOS gate: sides deferred on the Mapping panel counted as
    /// set back then precisely because they'd be collected HERE — so if any
    /// deferred side still lacks its photo, this raises the "take them now"
    /// prompt instead of completing (defer = postpone to the end, never
    /// skip). The prompt's capture buttons clear the deferrals; its Continue
    /// re-enters here and then completes.
    pub fn confirm_final_review(&mut self) {
        if !self.deferred_photos_pending().is_empty() {
            self.show_deferred_photos_prompt = true;
            return;
        }
        self.show_deferred_photos_prompt = false;
        if let Some(r) = self.current_record.as_mut() {
            r.data.details_reviewed = true;
            let _ = self.mark_dirty();
        }
        self.show_ber_complete = true;
    }

    /// Whether the "you deferred these photos — take them now" prompt (the
    /// stage between "Mark BER complete" and the completion popup) is showing.
    pub fn show_deferred_photos_prompt(&self) -> bool {
        self.show_deferred_photos_prompt
    }
    /// Dismiss the deferred-photos prompt WITHOUT completing (the "Go back"
    /// choice — the BER stays un-complete until the photos are captured).
    pub fn dismiss_deferred_photos_prompt(&mut self) {
        self.show_deferred_photos_prompt = false;
    }

    /// Whether the "BER complete" popup is showing.
    pub fn show_ber_complete(&self) -> bool {
        self.show_ber_complete
    }
    /// Whether the (dummy) summary screen is showing.
    pub fn show_summary(&self) -> bool {
        self.show_summary
    }
    /// Dismiss the completion popup (the "Go back" choice).
    pub fn dismiss_ber_complete(&mut self) {
        self.show_ber_complete = false;
    }
    /// From the completion popup → open the summary.
    pub fn open_summary(&mut self) {
        self.show_ber_complete = false;
        self.show_summary = true;
    }
    /// Close the summary (back to the phases).
    pub fn close_summary(&mut self) {
        self.show_summary = false;
    }

    /// The form dropdowns still Unset, by field label — the reason core the
    /// "Details form" checklist row shows; [`final_form_done`]
    /// (Self::final_form_done) is its emptiness, so colour and reasons can't
    /// disagree.
    fn final_form_missing(&self) -> Vec<String> {
        let d = self.final_details();
        [
            (d.floor == FloorKind::Unset, "Floor type not set"),
            (
                d.internal_wall == WallFinish::Unset,
                "Internal wall finish not set",
            ),
            (
                d.separating_wall == WallFinish::Unset,
                "Separating wall finish not set",
            ),
            (
                d.external_wall == WallFinish::Unset,
                "External wall finish not set",
            ),
            (d.heating == HeatingKind::Unset, "Heating type not set"),
        ]
        .into_iter()
        .filter_map(|(missing, label)| missing.then(|| label.to_string()))
        .collect()
    }

    /// Whether the form is fully filled (every dropdown set).
    fn final_form_done(&self) -> bool {
        self.final_form_missing().is_empty()
    }

    /// Whether all three Phase-4 parts are done — gates "Mark BER complete".
    pub fn final_complete(&self) -> bool {
        self.final_form_done() && self.north_aligned() && self.sheltering().all_set()
    }

    /// The Phase-4 checklist: progress rows for the form, north, and sheltering,
    /// each carrying the concrete unmet conditions (empty iff green — the
    /// reasons come from the same cores that decide the colour).
    pub fn final_details_checklist(&self) -> Vec<ChecklistItem> {
        let form_missing = self.final_form_missing();
        let north_missing: Vec<String> = if self.north_aligned() {
            Vec::new()
        } else {
            vec!["Satellite image not rotated to north yet".to_string()]
        };
        let shelter_missing: Vec<String> = self
            .sheltering()
            .unset_sides()
            .into_iter()
            .map(|side| format!("{} side's shelter not set", side.label()))
            .collect();
        let row = |label: &'static str, reasons: Vec<String>| {
            let status = if reasons.is_empty() {
                ChecklistStatus::AllStoreys
            } else {
                ChecklistStatus::Incomplete
            };
            ChecklistItem {
                label,
                status,
                reasons,
            }
        };
        vec![
            row("Details form", form_missing),
            row("North aligned", north_missing),
            row("Sheltered sides", shelter_missing),
        ]
    }
}
