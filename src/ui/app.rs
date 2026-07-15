use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

use std::sync::mpsc::{channel, Receiver};

use crate::controller::app_controller::{AppController, AssessmentPhase};
use crate::ui::adapters::sketch_egui::pos2_to_point2f;
use crate::ui::pages::events::{Page, Section, UiEvent};
use crate::ui::pages::home::{HomeEvent, HomePage, HomeState};
use crate::ui::pages::key_details::{KeyDetailsEvent, KeyDetailsPage, KeyDetailsState};
use crate::ui::pages::sketch::{
    categories, touch, EguiStroke, SketchEvent, SketchPage, SketchView,
};
use crate::ui::pages::splash::SplashPage;
use crate::ui::pages::workspace::workspace_chrome;

/// Surface a controller call's error (if any) to stderr with a label. Used by
/// the event-dispatch arms whose only failure handling is to log.
fn log_err(label: &str, result: Result<(), String>) {
    if let Err(e) = result {
        log::error!("{label}: {e}");
    }
}

pub struct BerApp {
    pub current_page: Page,
    pub app_controller: Rc<RefCell<AppController>>,

    // Page-specific state
    pub splash_state: SplashState,
    pub home_state: HomeState,
    pub key_details_state: KeyDetailsState,
    pub sketch_state: SketchState,

    // Global bug-report overlay (available on every page but Splash).
    pub bug_report: BugReportState,

    // Idle-autosave bookkeeping: the controller's edit `revision` last seen, and
    // the app-clock time it last changed. When the revision goes quiet for
    // `IDLE_SAVE_SECS`, we flush un-persisted edits to disk.
    last_seen_revision: u64,
    last_change_time: f64,

    // Keeps the screen on while the app is used; releases after 10 min idle.
    keep_awake: crate::keep_awake::KeepAwake,
}

/// Idle gap (seconds of no edits) before an autosave flush fires.
const IDLE_SAVE_SECS: f64 = 2.5;

/// State for the global bug-report overlay: the modal's open/recording flags,
/// the note being typed, when recording began (app-clock seconds, for the live
/// timer), and the last confirmation/error to show.
#[derive(Default)]
pub struct BugReportState {
    pub open: bool,
    pub recording: bool,
    pub started_at: f64,
    /// Duration of the finalized recording (set on Stop), shown as a ✓ marker.
    pub recorded_secs: Option<u64>,
    pub note: String,
    pub status_msg: Option<String>,
}

#[derive(Default)]
pub struct SketchState {
    /// Two-finger canvas-navigation state (pinch zoom / pan / stroke
    /// retraction) — cross-frame because a gesture spans frames.
    pub touch_nav: touch::TouchNav,
    /// Backing string for the dimension prompt's text input. Lives across
    /// frames so the user's in-progress typing isn't lost on re-render.
    pub dimension_input: String,
    /// Last error from `apply_dimension`. Shown next to the textbox so the
    /// user can correct without losing what they typed.
    pub dimension_error: Option<String>,
    /// Backing string for the position prompt's text input. PREFILLED with
    /// the prompt's current (drawn) distance whenever the prompt identity
    /// changes (see `position_prompt_key`); the user overtypes or accepts.
    pub position_input: String,
    /// Identity `(axis, kind, option_idx)` of the position prompt the input
    /// was last prefilled for. When the visible prompt's identity differs
    /// (next axis, next Wall param, cycled reference), the input is refilled
    /// with its drawn distance — `kind` is part of the identity because two
    /// consecutive Wall prompts can share the same axis and option index.
    /// `None` when no position prompt is showing.
    pub position_prompt_key: Option<(
        manip_plan::ParameterAxis,
        crate::controller::app_controller::PositionPromptKind,
        usize,
    )>,
    /// Last error from `apply_position_dimension`. Shown next to the textbox
    /// so the user can correct without losing what they typed.
    pub position_error: Option<String>,
    /// Backing string for the Edit-drag measurement prompt's input.
    pub edit_drag_input: String,
    /// Last error from `apply_edit_drag` (or the cancel/cycle paths).
    pub edit_drag_error: Option<String>,
    /// Backing strings for the vertex-drop prompt's up-to-4 text boxes.
    /// Keyed by measurement index (matches `VertexMeasurementUiData.idx`).
    pub vertex_drop_inputs: [String; 4],
    /// Last error from `apply_vertex_measurement`.
    pub vertex_drop_error: Option<String>,
    /// Backing strings for the exposure section's typed start/end inputs.
    pub exposure_start_input: String,
    pub exposure_end_input: String,
    /// Backing string for the wall-partition dimension prompt's metre input.
    pub partition_dim_input: String,
    /// Backing string for the floor-step amount input (metres).
    pub step_amount_input: String,
    // Error surfacing follows the established per-flow pattern (one
    // `Option<String>` per prompt panel — `dimension_error`, `position_error`,
    // etc.) rather than a single shared field: each error renders inside the
    // panel that owns the rejected input, and the step / ceiling-step / roof
    // toolbars can be on screen alongside other prompts. The roof family
    // (apex height, ridge length, ridge height) shares ONE field because it
    // shares one toolbar with mutually exclusive stages.
    /// Last error from `assign_step_level_to_pending`. Shown next to the
    /// amount field so the user can correct without losing what they typed;
    /// cleared on success and on cancel.
    pub step_error: Option<String>,
    /// Backing string for the ceiling-step amount input (metres).
    pub ceiling_amount_input: String,
    /// Last error from `assign_ceiling_step_delta`. Same lifecycle as
    /// `step_error` (also cleared by un-snap — the stage it described is gone).
    pub ceiling_step_error: Option<String>,
    /// Last error from the roof-point/ridge assigns (`assign_roof_apex`,
    /// `assign_roof_ridge_length`, `assign_roof_ridge_height`) — one field for
    /// the shared roof toolbar; cleared on success, skip, cancel, and un-snap.
    pub roof_error: Option<String>,
    /// Backing string for the roof-point apex-height input (metres). PREFILLED
    /// with the storey's default ceiling height when the Height stage opens
    /// (via `ceiling_apex_prefilled`); the user overtypes or accepts.
    pub ceiling_apex_input: String,
    /// Backing string for the Phase-1 roof step's pitch input (degrees).
    pub roof_pitch_input: String,
    /// One-shot guard for the prefill above: set when the Height stage's
    /// prefill has been written, cleared when the stage closes, so the user's
    /// in-progress typing isn't clobbered every frame.
    pub ceiling_apex_prefilled: bool,
    /// Monotonic counter bumped each time a roof-HEIGHT stage freshly opens
    /// (alongside the prefill above). Folded into the number box's suggestion
    /// token so re-opening the SAME stage (e.g. a ridge height for a second
    /// area) re-arms the "first key replaces the prefill" guard — the token
    /// otherwise persists in egui memory across the gap between the two boxes.
    pub ceiling_apex_open_seq: u64,
    /// Backing string for the roof-ridge LENGTH input (metres). PREFILLED
    /// with the pending ridge's drawn length when the Length stage opens
    /// (via `ceiling_ridge_length_prefilled`); the user overtypes or skips.
    pub ceiling_ridge_length_input: String,
    /// One-shot guard for the length prefill, mirroring `ceiling_apex_prefilled`.
    pub ceiling_ridge_length_prefilled: bool,
    /// Identity (the open stage's axis) of the roof-RECT side-length input's
    /// last prefill — keyed on the AXIS so the LenX → LenY transition refills
    /// (the rect shares `ceiling_ridge_length_input`; the two flows are never
    /// pending at once). `None` when no rect length stage is up.
    pub ceiling_rect_len_key: Option<manip_plan::ParameterAxis>,
    /// Backing string for the new-storey flow's thickness input. PREFILLED
    /// "0.25" when the Thickness stage opens (via the one-shot guard below);
    /// the user overtypes, or "Use 0.25" submits the default directly.
    pub new_storey_thickness_input: String,
    /// One-shot guard for the thickness prefill, mirroring
    /// `ceiling_apex_prefilled`: set when the stage's prefill has been written,
    /// cleared when the stage closes.
    pub new_storey_thickness_prefilled: bool,
    /// Last error from the new-storey toolbar's actions
    /// (`assign_floor_thickness`, `choose_suggest_outlines`,
    /// `accept_suggestion`) — one field for the one toolbar, whose stages
    /// are mutually exclusive (the roof-family pattern). Shown inline in it;
    /// cleared on each success.
    pub new_storey_error: Option<String>,
    /// Armed delete-storey confirm in the storey tab row: `Some(idx)` while
    /// the inline "Delete Storey N?" confirm is showing for storey `idx`.
    /// Disarmed by the tab row itself whenever the deletable (top) storey
    /// changes, and on dispatching the delete.
    pub delete_storey_armed: Option<usize>,
    /// Last error from the Heights thickness field's `set_floor_thickness`.
    /// Its own field (not `new_storey_error`): the Heights field and the
    /// flow toolbar can be on screen at once, each error renders in the
    /// panel that owns the rejected input.
    pub floor_thickness_error: Option<String>,
    /// One-shot clear-on-success for the Heights thickness field's input
    /// buffer, mirroring `ceiling_height_input_clear` (the event closure
    /// can't clear `categories.floor_thickness_input` directly).
    pub floor_thickness_input_clear: bool,
    /// Last error from `set_default_ceiling_height`. Shown next to the
    /// Heights field so the user can correct without losing what they typed;
    /// cleared on the next successful Set.
    pub ceiling_height_error: Option<String>,
    /// One-shot clear-on-success for the Heights field's input buffer: the
    /// event closure can't clear `categories.ceiling_height_input` directly
    /// (the render borrows `categories` mutably), so a successful Set raises
    /// this and the next frame's pre-render housekeeping does the clear.
    pub ceiling_height_input_clear: bool,
    /// Category-layout UI state: the ribbon's active tab, the wall-length
    /// label visibility toggle (defaults ON), and the Heights field's
    /// ceiling-height input buffer.
    pub categories: categories::CategoryUiState,
    // ── Windows/doors ──────────────────────────────────────────────────────
    /// Per-field measurement buffers for the staged placement: one is bound to
    /// the floating number box at a time (width → height → sill, or the
    /// door+sidelights fields). The box clears the active one on Apply.
    pub opening_width_input: String,
    pub opening_height_input: String,
    /// The placement/edit box's "Full width" / "Full height" checkboxes: the
    /// opening spans the whole wall length / face height, the matching field
    /// disabled showing the resolved span. Reset when a new placement starts.
    pub opening_full_width: bool,
    pub opening_full_height: bool,
    /// Door+sidelights composer inputs (sidelight widths + per-side sills +
    /// transom height + the semicircular-transom toggle). A sidelight's sill
    /// field is only active when its `*_has_sill` box is ticked. (Detailed mode.)
    pub sidelight_left_input: String,
    pub sidelight_left_sill_input: String,
    pub sidelight_left_has_sill: bool,
    pub sidelight_right_input: String,
    pub sidelight_right_sill_input: String,
    pub sidelight_right_has_sill: bool,
    pub transom_input: String,
    pub transom_semicircular: bool,
    /// Door+sidelights OVERALL-mode inputs: the door-leaf W/H (typed under the
    /// whole-doorset W/H, which reuse `opening_width_input`/`opening_height_input`)
    /// and which side(s) carry a sidelight (both default on → even split).
    pub door_width_input: String,
    pub door_height_input: String,
    pub sidelight_left_present: bool,
    pub sidelight_right_present: bool,
    /// Complex-window 3rd/4th dimension buffers (fields 0/1 reuse the width/height
    /// inputs); the pinned shape kind lives on `categories`.
    pub window_shape_c_input: String,
    pub window_shape_d_input: String,
    /// Window-type picker (the placement box): which shared type a placed window
    /// joins. `wt_existing_id = Some` reuses an existing type by id; `None`
    /// defines a new one from glazing/frame/year/cert (find-or-created on Place,
    /// so a "new" type that matches an existing one joins it — "only add if
    /// different"). Persists across placements so repeated windows of one type
    /// are a single pick.
    pub wt_existing_id: Option<ber_model::ber::common::ItemId>,
    pub wt_glazing: ber_model::ber::fenestration::Glazing,
    pub wt_frame: ber_model::ber::fenestration::Frame,
    pub wt_year_input: String,
    pub wt_certified: bool,
    /// Pane gap(s) in mm for a new window type (comma/space separated for the two
    /// gaps of triple glazing); and an optional model name distinguishing two
    /// otherwise-identical types.
    pub wt_gap_input: String,
    pub wt_name_input: String,
    /// Per-window overshading selected in the placement box (defaults to Average).
    /// Persists across placements; applied to each window as it's placed.
    pub overshading: ber_model::ber::fenestration::Overshading,
    /// Source paths of photos picked in the placement box for the window being
    /// placed (a window photo is per-instance), each with the capture tier its
    /// button asked for (TEMPORARY dual hi/lo buttons — `photo_button_pair`).
    /// Cleared at the start of each new placement; consumed (attached) when the
    /// window is placed.
    pub pending_window_photos: Vec<(std::path::PathBuf, crate::ui::widgets::PhotoQuality)>,
    /// Phase-3 (Objects) entry-panel state: the chosen category (to show its
    /// subtypes) and the count / note / "Other"-name buffers. Cleared after
    /// each Add and reset on a category switch.
    pub object_selected_category: Option<ber_model::ber::objects::ObjectCategory>,
    /// The concrete kind chosen (a subtype picked, or a singleton category).
    /// `None` while a category is selected but its subtype hasn't been picked yet.
    pub object_selected_kind: Option<ber_model::ber::objects::ObjectKind>,
    pub object_count_input: String,
    pub object_note_input: String,
    pub object_other_name: String,
    /// Photos captured for the object being added, buffered PER KIND: OK for a
    /// kind commits (and clears) only that kind's buffer; switching kind parks
    /// the previous kind's photos — never dropped, never leaked into another
    /// kind's commit. Parked photos survive room close / phase exit here until
    /// their kind is OK'd.
    pub object_pending_photos: crate::ui::pages::sketch::PendingObjectPhotos,
    /// The object kind we've already AUTO-opened the camera for this selection
    /// (a mandatory-photo kind opens the camera once). Latches the auto-open to
    /// one shot per selected kind so a cancel doesn't re-launch it; reset when
    /// the selection changes or the object is added. See `objects_panel`.
    pub object_auto_photo_for: Option<ber_model::ber::objects::ObjectKind>,
    /// The open external-photo ANNOTATION session (paintbrush over a stored
    /// photo), or `None`. Opened by `BeginAnnotateExternalPhoto`; drawn as a
    /// takeover window over the Sketch canvas; closes itself on Save/Cancel.
    pub photo_annotator: Option<crate::ui::widgets::PhotoAnnotator>,
    /// Whether the floating default-ceiling-height entry box is open. Lives here
    /// (not on `categories`, which the view borrows) so the event handler can
    /// toggle it; the view reads it by value. Opened by the Heights tool button,
    /// closed on a successful set or its Close button.
    pub ceiling_entry_open: bool,
    /// Background connection to a Bluetooth laser meter (Leica DISTO), feeding
    /// the number-entry box when it's in Laser mode. Lives across frames so the
    /// link stays up; inert unless the `bluetooth` feature is built.
    pub laser: crate::number_entry::bluetooth::LaserSource,
}

pub struct SplashState {
    pub progress_text: String,
    pub progress_value: f32,
    pub receiver: Option<Receiver<(String, f32)>>,
    pub loading_started: bool,
}

impl Default for SplashState {
    fn default() -> Self {
        Self {
            progress_text: String::new(),
            progress_value: 0.0,
            receiver: None,
            loading_started: false,
        }
    }
}

impl BerApp {
    pub fn new(app_controller: Rc<RefCell<AppController>>) -> Self {
        // DEV: `BER_CHECKPOINT=1 cargo run` loads the saved checkpoint plan and
        // jumps straight to the sketch section, so a test case needn't be redrawn.
        let current_page = if std::env::var("BER_CHECKPOINT").is_ok()
            && app_controller.borrow_mut().load_checkpoint()
        {
            Page::Workspace(Section::Sketch)
        } else {
            Page::Splash
        };
        let mut app = Self {
            current_page,
            app_controller,
            splash_state: SplashState::default(),
            home_state: HomeState::default(),
            key_details_state: KeyDetailsState::default(),
            sketch_state: SketchState::default(),
            bug_report: BugReportState::default(),
            last_seen_revision: 0,
            last_change_time: 0.0,
            keep_awake: crate::keep_awake::KeepAwake::default(),
        };
        // Checkpoint path enters the workspace directly — prefill Key Details
        // the same way New/Load do.
        if matches!(app.current_page, Page::Workspace(_)) {
            app.populate_key_details();
        }
        app
    }

    pub fn navigate_to(&mut self, page: Page) {
        self.current_page = page;
    }

    /// Dismiss the OPEN object room (a tap outside its box, or its Done):
    /// commit like OK first, then close. The field rule — an EMPTY in-flight
    /// entry dismisses silently, but anything already entered (count, note,
    /// photo, an "Other" name) must not be lost — plus a flush of any photo
    /// buffers still parked for this room (each becomes its own object), so a
    /// captured photo can neither strand nor leak into another room. The
    /// decision logic is `sketch::take_dismiss_commits` (unit-tested there);
    /// runs post-render because the render view borrows the entry buffers.
    fn flush_and_close_object_room(&mut self) {
        let Some(room_id) = self.app_controller.borrow().active_object_room() else {
            return;
        };
        let st = &mut self.sketch_state;
        let commits = {
            let ctrl = self.app_controller.borrow();
            crate::ui::pages::sketch::take_dismiss_commits(
                room_id,
                st.object_selected_kind.as_ref(),
                &st.object_count_input,
                &st.object_note_input,
                |k| ctrl.object_photo_required(k),
                &mut st.object_pending_photos,
            )
        };
        // Reset the entry UI — the room is closing, so the selection doesn't
        // carry over to the next room (mirrors the OK button's reset).
        st.object_selected_category = None;
        st.object_selected_kind = None;
        st.object_auto_photo_for = None;
        st.object_count_input.clear();
        st.object_note_input.clear();
        st.object_other_name.clear();
        let mut ctrl = self.app_controller.borrow_mut();
        for c in commits {
            ctrl.add_object(c.kind, c.count, c.note, c.photos);
        }
        ctrl.close_object_room();
    }

    /// Draw the always-available bug-report overlay: a floating 🐞 button and,
    /// when opened, a modal to record a spoken description + save a full state
    /// dump. Handled entirely here (direct controller calls) — the page render
    /// borrows are already released by the time this runs.
    fn bug_overlay(&mut self, ctx: &egui::Context) {
        use crate::ui::widgets::hint::HintExt;

        // A WiFi-up signal (from the Android connectivity callback) means retry
        // any queued uploads now, rather than waiting for the periodic tick.
        if crate::platform::net::take_wifi_up() {
            self.app_controller.borrow().bug_upload_now();
        }

        // The button is out of place on the transient splash/loading screen.
        if self.current_page == Page::Splash {
            return;
        }

        // Floating button, bottom-right, above everything.
        let mut open_clicked = false;
        egui::Area::new(egui::Id::new("bug_report_button"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let btn = egui::Button::new(egui::RichText::new("🐞").size(22.0))
                    .fill(egui::Color32::from_rgb(176, 42, 42))
                    .min_size(egui::vec2(46.0, 46.0));
                if ui.add(btn).hint("Report a bug").clicked() {
                    open_clicked = true;
                }
            });
        if open_clicked {
            self.bug_report.open = true;
            self.bug_report.status_msg = None;
        }
        if !self.bug_report.open {
            return;
        }

        let audio_supported = self.app_controller.borrow().bug_audio_supported();
        let recording = self.bug_report.recording;
        // Keep the live timer ticking while recording.
        if recording {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        let elapsed = if recording {
            (ctx.input(|i| i.time) - self.bug_report.started_at).max(0.0)
        } else {
            0.0
        };

        // Collect intents inside the closure; act after it releases its borrows.
        let mut do_record = false;
        let mut do_stop = false;
        let mut do_save = false;
        let mut do_cancel = false;

        // While the native keyboard is up (the note field below raises it),
        // shrink the constrain rect by the keyboard inset so the centred
        // window gets pushed above it — and settles back when it hides. Same
        // pattern as the sketch number box; never below 40% of the screen so
        // an oversized keyboard can't squash the constraint into nothing.
        // On desktop the inset is 0, so this is the full screen as before.
        let constrain = {
            let full = ctx.content_rect();
            let inset = crate::ui::widgets::ime_inset_bottom_points(ctx);
            let bottom = (full.bottom() - inset).max(full.top() + full.height() * 0.4);
            egui::Rect::from_min_max(full.min, egui::pos2(full.max.x, bottom))
        };
        egui::Window::new("Report a bug")
            .id(egui::Id::new("bug_report_modal"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .constrain_to(constrain)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                ui.label(
                    egui::RichText::new("Describe what went wrong")
                        .strong()
                        .size(17.0),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Records a spoken note and saves the current BER's state so it can \
                         be debugged later. Uploads automatically once online.",
                    )
                    .color(egui::Color32::from_gray(150)),
                );
                ui.add_space(10.0);

                // Audio controls.
                if audio_supported {
                    ui.horizontal(|ui| {
                        if recording {
                            let secs = elapsed as u64;
                            ui.label(
                                egui::RichText::new(format!(
                                    "● Recording  {:02}:{:02}",
                                    secs / 60,
                                    secs % 60
                                ))
                                .color(egui::Color32::from_rgb(200, 60, 60))
                                .strong(),
                            );
                            if ui.button("■ Stop").clicked() {
                                do_stop = true;
                            }
                        } else if let Some(secs) = self.bug_report.recorded_secs {
                            ui.label(
                                egui::RichText::new(format!(
                                    "✔ Recorded  {:02}:{:02}",
                                    secs / 60,
                                    secs % 60
                                ))
                                .color(egui::Color32::from_rgb(60, 140, 60)),
                            );
                            if ui.button("Re-record").clicked() {
                                do_record = true;
                            }
                        } else if ui.button("● Record spoken description").clicked() {
                            do_record = true;
                        }
                    });
                } else {
                    ui.label(
                        egui::RichText::new("Audio recording is only available on the tablet.")
                            .italics()
                            .color(egui::Color32::from_gray(140)),
                    );
                }

                ui.add_space(8.0);
                ui.label("Note (optional):");
                crate::ui::widgets::text_input(
                    ui,
                    &mut self.bug_report.note,
                    f32::INFINITY,
                    "Anything to add in text",
                );

                if let Some(msg) = &self.bug_report.status_msg {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(msg).color(egui::Color32::from_rgb(60, 140, 60)));
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new("Save & send").strong(),
                        ))
                        .clicked()
                    {
                        do_save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                });
            });

        // ---- Act on the collected intents (borrows now released) ----
        if do_record {
            // Re-record discards any previously staged take first.
            self.app_controller.borrow_mut().cancel_bug_recording();
            self.bug_report.recorded_secs = None;
            match self.app_controller.borrow_mut().start_bug_recording() {
                Ok(()) => {
                    self.bug_report.recording = true;
                    self.bug_report.started_at = ctx.input(|i| i.time);
                    self.bug_report.status_msg = None;
                }
                Err(e) => self.bug_report.status_msg = Some(e),
            }
        }
        if do_stop {
            // Finalize but KEEP the take so Save can attach it.
            self.app_controller.borrow_mut().stop_bug_recording();
            self.bug_report.recording = false;
            self.bug_report.recorded_secs = Some(elapsed as u64);
        }
        if do_cancel {
            self.app_controller.borrow_mut().cancel_bug_recording();
            self.bug_report = BugReportState::default();
        }
        if do_save {
            let mut context = std::collections::BTreeMap::new();
            context.insert("page".to_string(), format!("{:?}", self.current_page));
            let note = std::mem::take(&mut self.bug_report.note);
            let result = self
                .app_controller
                .borrow_mut()
                .finish_bug_report(note, context);
            match result {
                Ok(msg) => {
                    // Close the modal on success.
                    self.bug_report = BugReportState::default();
                    self.bug_report.status_msg = Some(msg);
                    self.bug_report.open = false;
                }
                Err(e) => self.bug_report.status_msg = Some(format!("Could not save: {e}")),
            }
        }
    }

    /// Rebuild the Key Details form's backing strings from the loaded record
    /// (name, date, construction year + extensions reconstructed from its
    /// `ConstructionPhases`). Called on every workspace entry (New / Load /
    /// checkpoint) so the form always mirrors the record.
    fn populate_key_details(&mut self) {
        self.key_details_state = match self.app_controller.borrow().current_key_details() {
            Some((name, address, eircode, date, phases, intake)) => {
                KeyDetailsState::from_record(name, address, eircode, date, &phases, &intake)
            }
            None => KeyDetailsState::default(),
        };
    }

    /// Act on a Home-page intent (controller call + navigation + state
    /// population). Runs after the render closure has released its borrows.
    fn handle_home_event(&mut self, event: HomeEvent) {
        match event {
            HomeEvent::NewBer => {
                // The record is created IMMEDIATELY — Key Details edits it live.
                let today = chrono::Local::now().naive_local().date();
                self.app_controller
                    .borrow_mut()
                    .create_ber("Untitled BER".to_string(), today);
                self.populate_key_details();
                self.home_state = HomeState::default();
                self.navigate_to(Page::Workspace(Section::KeyDetails));
            }
            HomeEvent::ToggleLoadPanel => {
                self.home_state.show_load_panel = !self.home_state.show_load_panel;
                self.home_state.pending_delete = None;
                if self.home_state.show_load_panel {
                    self.home_state.rows = self.app_controller.borrow().list_bers();
                }
            }
            HomeEvent::OpenBer(id) => {
                let result = self.app_controller.borrow_mut().open_ber(&id);
                match result {
                    Ok(()) => {
                        self.populate_key_details();
                        self.home_state = HomeState::default();
                        self.navigate_to(Page::Workspace(Section::KeyDetails));
                    }
                    Err(e) => log::error!("open_ber: {e}"),
                }
            }
            HomeEvent::DeleteBer(id) => {
                log_err(
                    "delete_ber",
                    self.app_controller.borrow_mut().delete_ber(&id),
                );
                self.home_state.pending_delete = None;
                self.home_state.rows = self.app_controller.borrow().list_bers();
            }
            HomeEvent::DuplicateBer(id) => {
                match self.app_controller.borrow().duplicate_ber(&id) {
                    Ok(_) => {}
                    Err(e) => log::error!("duplicate_ber: {e}"),
                }
                // Refresh so the new copy appears in the Load list; the assessor
                // stays on Home (the copy is not opened).
                self.home_state.rows = self.app_controller.borrow().list_bers();
            }
        }
    }

    pub fn start_loading(&mut self) {
        if self.splash_state.loading_started {
            return;
        }

        let (tx, rx) = channel::<(String, f32)>();
        self.splash_state.receiver = Some(rx);
        self.splash_state.loading_started = true;

        // Start async loading
        self.app_controller.borrow().load_data_async(tx);
    }

    pub fn check_progress(&mut self) {
        let mut should_navigate = false; // Local variable, not borrowing self

        if let Some(receiver) = &self.splash_state.receiver {
            // Immutable borrow starts
            while let Ok((text, progress)) = receiver.try_recv() {
                self.splash_state.progress_text = text; // ✅ OK: different field
                self.splash_state.progress_value = progress; // ✅ OK: different field

                if progress >= 1.0 {
                    should_navigate = true; // ✅ OK: just setting a bool
                                            // NOT calling navigate_to() here!
                }
            }
        } // Immutable borrow ends here

        if should_navigate {
            self.navigate_to(Page::Home); // ✅ OK: borrow is gone now
        }
    }
}

impl eframe::App for BerApp {
    /// Backstop for the idle autosave: eframe calls `save` periodically and on
    /// shutdown, so edits made within the idle window before a clean exit still
    /// reach disk. No-op when nothing is pending.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        let _ = self.app_controller.borrow_mut().flush_if_dirty();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.splash_state.loading_started {
            self.start_loading();
            self.splash_state.loading_started = true;
        }

        // Native soft keyboard: hide it when no field has focus any more, and
        // keep frames coming while it's up (its text arrives by polling, not
        // as events). Top of the frame — focus state persists in egui memory,
        // so last frame's focus is already visible here.
        #[cfg(target_os = "android")]
        crate::ui::widgets::native_ime::end_frame(ctx);

        // Keep the screen on while the app is in use (releases after 10 min idle).
        self.keep_awake.tick(ctx);

        // Idle autosave: edits refresh the in-memory BER (cheap) but no longer
        // write to disk; we flush to SQLite once editing goes quiet. On a change
        // we reset the timer and schedule a wake-up at the idle deadline (egui is
        // reactive — without `request_repaint_after` it would never tick to save).
        {
            let now = ctx.input(|i| i.time);
            let rev = self.app_controller.borrow().revision();
            if rev != self.last_seen_revision {
                self.last_seen_revision = rev;
                self.last_change_time = now;
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(IDLE_SAVE_SECS));
            } else if now - self.last_change_time >= IDLE_SAVE_SECS {
                // No-op if nothing is pending; otherwise hand the write to the
                // background thread (falls back to a sync write if none attached).
                let _ = self.app_controller.borrow_mut().autosave();
            }
        }
        // Background photo jobs: fold in any that finished (swap the recompressed
        // file + thumbnail into the photo by id), and keep repainting while any
        // are still processing so the upgrade appears without needing an event.
        {
            let mut c = self.app_controller.borrow_mut();
            c.apply_finished_photos();
            if c.photos_processing() {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }

        // Check for progress updates if on splash screen
        if self.current_page == Page::Splash {
            self.check_progress();
            ctx.request_repaint(); // Keep updating while loading
        }

        // Android's back button arrives as an ordinary egui key (winit maps
        // KEYCODE_BACK → BrowserBack). Back from the workspace exits the BER
        // (same flush-and-return-Home path as the chrome's Exit); back from
        // Home/Splash is deliberately ignored so the activity's default
        // minimise happens instead of killing the app. Unconditional — it's a
        // reasonable desktop shortcut too (browser-back keyboards).
        let back_pressed =
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::BrowserBack));

        let page = self.current_page;
        // Render current page
        match page {
            Page::Splash => {
                egui::CentralPanel::default().show(ctx, |ui| {
                    SplashPage::render(
                        ui,
                        self.splash_state.progress_text.clone(),
                        self.splash_state.progress_value,
                        |_| {},
                    )
                });
            }
            Page::Home => {
                // Collect intents from the render closure first, then act after
                // it releases its borrow of self.home_state.
                let mut home_events: Vec<HomeEvent> = Vec::new();
                egui::CentralPanel::default().show(ctx, |ui| {
                    HomePage::render(ui, &mut self.home_state, |event| {
                        if let UiEvent::Home(e) = event {
                            home_events.push(e);
                        }
                    });
                });
                for e in home_events {
                    self.handle_home_event(e);
                }
            }
            Page::Workspace(section) => {
                let ber_name = self
                    .app_controller
                    .borrow()
                    .current_record_name()
                    .unwrap_or_default();
                // Chrome intents land in locals (the body closure borrows self).
                // `exit_clicked` is a Cell because BOTH the chrome closure and
                // the body closure (via the sketch page's "Save & exit BER" in
                // the summary modal) raise it — two `&mut` captures of the same
                // local across one `workspace_chrome` call would not borrow-check,
                // so they share it immutably and it's consumed after the call.
                let mut next_page: Option<Page> = None;
                let exit_clicked = std::cell::Cell::new(back_pressed);
                workspace_chrome(
                    ctx,
                    section,
                    &ber_name,
                    |event| match event {
                        UiEvent::NavigateTo(p) => next_page = Some(p),
                        UiEvent::ExitBer => exit_clicked.set(true),
                        _ => {}
                    },
                    |ui| match section {
                        Section::KeyDetails => {
                            // LIVE editor: every field change writes through to
                            // the loaded record (and persists) immediately.
                            let controller = self.app_controller.clone();
                            KeyDetailsPage::render(ui, &mut self.key_details_state, |event| {
                                if let UiEvent::KeyDetails(e) = event {
                                    let mut c = controller.borrow_mut();
                                    match e {
                                        KeyDetailsEvent::Rename(name) => c.rename_ber(name),
                                        KeyDetailsEvent::SetAddress(a) => c.set_address(a),
                                        KeyDetailsEvent::SetEircode(e) => c.set_eircode(e),
                                        KeyDetailsEvent::SetDateAssessed(d) => {
                                            c.set_date_assessed(d)
                                        }
                                        KeyDetailsEvent::SetConstructionPhases(p) => {
                                            c.set_construction_phases(p)
                                        }
                                        KeyDetailsEvent::SetIntake(i) => c.set_intake(i),
                                    }
                                }
                            });
                        }
                        Section::MoreDetails => {
                            ui.heading("More Details");
                            ui.label("More details — coming soon.");
                        }
                        Section::Sketch => {
                            // Bring the resolved geometry current before the immutable borrow
                            // reads it for rendering — edits only `mark_dirty`, so the derive
                            // is deferred to here (once per frame, not per edit).
                            self.app_controller.borrow_mut().materialize();
                            let (
                                stroke,
                                mode,
                                edit_mode,
                                display_mode,
                                ber,
                                review,
                                review_remaining,
                                has_floor_plan,
                                pending_redraw,
                                dim_prompt,
                                dim_scale_set,
                                dim_error,
                                position_prompt,
                                dimensioned_walls,
                                copy_wall_labels,
                                arc_preview,
                                curve_bulge,
                                living_area_mode,
                                living_area_polygons,
                                living_area_wall_labels,
                                active_living_area,
                                pending_cut_regions,
                                exposure_mode,
                                exposure_walls,
                                exposure_selection,
                                riser_views,
                                partition_mode,
                                edit_selected_wall,
                                edit_selected_vertex,
                                edit_drag_prompt,
                                edit_drag_error,
                                vertex_drop_prompt,
                                vertex_drop_error,
                                can_undo,
                                can_redo,
                                phase_mode,
                                phase_region_polygons,
                                phase_delete_labels,
                                pending_phase_polygon,
                                pending_phase_cut_regions,
                                phase_choices,
                                phase_pick_pending,
                                step_mode,
                                step_region_polygons,
                                pending_step_polygon,
                                pending_step_cut_regions,
                                step_pick_pending,
                                ceiling_step_mode,
                                ceiling_step_available,
                                ceiling_step_region_polygons,
                                ceiling_divider_lines,
                                ceiling_step_editing,
                                ceiling_step_current_height,
                                pending_ceiling_step_line,
                                pending_ceiling_step_side_rects,
                                ceiling_step_snap_tag,
                                ceiling_step_orientation_pending,
                                ceiling_step_side_pending,
                                ceiling_step_amount_pending,
                                ceiling_step_pending,
                                ceiling_point_mode,
                                ceiling_point_available,
                                ceiling_apex_views,
                                ceiling_fan_marker_views,
                                pending_roof_point,
                                ceiling_point_snap_tag,
                                ceiling_point_position_pending,
                                ceiling_point_height_pending,
                                ceiling_point_pending,
                                ceiling_ridge_views,
                                pending_roof_ridge_line,
                                ceiling_ridge_snap_tag,
                                ceiling_ridge_position_pending,
                                ceiling_ridge_length_pending,
                                ceiling_ridge_length_anchored,
                                pending_roof_ridge_length,
                                ceiling_ridge_height_pending,
                                ceiling_ridge_meet_edge_height,
                                ceiling_ridge_pending,
                                reset_ceiling_pending,
                                neighbour_continuation_mode,
                                storey_continuation_mode,
                                ceiling_continuation_picking_target,
                                ceiling_continuation_picking_source,
                                ceiling_rect_views,
                                pending_roof_rect_outline,
                                ceiling_rect_position_pending,
                                ceiling_rect_length_axis,
                                pending_roof_rect_length,
                                ceiling_rect_height_pending,
                                ceiling_rect_pending,
                                ceiling_rect_is_dormer,
                                dormer_mode,
                                dormer_available,
                                dormer_views,
                                default_ceiling_height,
                                ceiling_heights_set,
                                new_storey_prompt,
                                floor_thickness,
                                view_params,
                            ) = {
                                let controller_ref = self.app_controller.borrow();
                                let v = controller_ref.view();
                                (
                                    controller_ref.sketch_stroke(),
                                    controller_ref.sketch_mode(),
                                    controller_ref.edit_mode(),
                                    controller_ref.display_mode(),
                                    controller_ref.current_ber_clone(),
                                    controller_ref.current_review_suggestion_clone(),
                                    controller_ref.review_remaining(),
                                    controller_ref.has_floor_plan(),
                                    controller_ref.has_pending_redraw(),
                                    controller_ref.dimension_prompt_for_ui(),
                                    controller_ref.dimensioning_scale_set(),
                                    self.sketch_state.dimension_error.clone(),
                                    controller_ref.position_prompt_for_ui(),
                                    controller_ref.walls_with_known_length(),
                                    controller_ref.copyable_wall_labels(),
                                    controller_ref.pending_arc_preview(),
                                    controller_ref.pending_arc_bulge(),
                                    controller_ref.living_area_mode(),
                                    controller_ref.living_area_polygons(),
                                    controller_ref.living_area_wall_labels(),
                                    controller_ref.active_living_area_index(),
                                    controller_ref.pending_cut_regions(),
                                    controller_ref.exposure_mode(),
                                    controller_ref.wall_exposure_views(),
                                    controller_ref.exposure_selection(),
                                    controller_ref.riser_views(),
                                    controller_ref.partition_mode(),
                                    controller_ref.edit_selected_wall_positions(),
                                    controller_ref.edit_selected_vertex_position(),
                                    controller_ref.edit_drag_prompt_for_ui(),
                                    self.sketch_state.edit_drag_error.clone(),
                                    controller_ref.vertex_drop_prompt_for_ui(),
                                    self.sketch_state.vertex_drop_error.clone(),
                                    controller_ref.can_undo(),
                                    controller_ref.can_redo(),
                                    controller_ref.phase_mode(),
                                    controller_ref.phase_region_polygons(),
                                    controller_ref.phase_region_delete_labels(),
                                    controller_ref.pending_phase_polygon(),
                                    controller_ref.pending_phase_cut_regions(),
                                    controller_ref.construction_phase_choices(),
                                    controller_ref.phase_pick_pending(),
                                    controller_ref.step_mode(),
                                    controller_ref.step_region_polygons(),
                                    controller_ref.pending_step_polygon(),
                                    controller_ref.pending_step_cut_regions(),
                                    controller_ref.step_pick_pending(),
                                    controller_ref.ceiling_step_mode(),
                                    controller_ref.ceiling_step_available(),
                                    controller_ref.ceiling_step_region_polygons(),
                                    controller_ref.ceiling_divider_lines(),
                                    controller_ref.ceiling_step_editing(),
                                    controller_ref.ceiling_step_current_height(),
                                    controller_ref.pending_ceiling_step_line(),
                                    controller_ref.pending_ceiling_step_side_rects(),
                                    controller_ref.ceiling_step_snap_tag(),
                                    controller_ref.ceiling_step_orientation_pending(),
                                    controller_ref.ceiling_step_side_pending(),
                                    controller_ref.ceiling_step_amount_pending(),
                                    controller_ref.ceiling_step_pending(),
                                    controller_ref.ceiling_point_mode(),
                                    controller_ref.ceiling_point_available(),
                                    controller_ref.ceiling_apex_views(),
                                    controller_ref.ceiling_fan_marker_views(),
                                    controller_ref.pending_roof_point_position(),
                                    controller_ref.ceiling_point_snap_tag(),
                                    controller_ref.ceiling_point_position_pending(),
                                    controller_ref.ceiling_point_height_pending(),
                                    controller_ref.ceiling_point_pending(),
                                    controller_ref.ceiling_ridge_views(),
                                    controller_ref.pending_roof_ridge_line(),
                                    controller_ref.ceiling_ridge_snap_tag(),
                                    controller_ref.ceiling_ridge_position_pending(),
                                    controller_ref.ceiling_ridge_length_pending(),
                                    controller_ref.ceiling_ridge_length_anchored(),
                                    controller_ref.pending_roof_ridge_length(),
                                    controller_ref.ceiling_ridge_height_pending(),
                                    controller_ref.ceiling_ridge_meet_edge_height(),
                                    controller_ref.ceiling_ridge_pending(),
                                    controller_ref.reset_ceiling_shape_pending(),
                                    controller_ref.neighbour_continuation_mode(),
                                    controller_ref.storey_continuation_mode(),
                                    controller_ref.ceiling_continuation_picking_target(),
                                    controller_ref.ceiling_continuation_picking_source(),
                                    controller_ref.ceiling_rect_views(),
                                    controller_ref.pending_roof_rect_outline(),
                                    controller_ref.ceiling_rect_position_pending(),
                                    controller_ref.ceiling_rect_length_pending(),
                                    controller_ref.pending_roof_rect_length(),
                                    controller_ref.ceiling_rect_height_pending(),
                                    controller_ref.ceiling_rect_pending(),
                                    controller_ref.pending_rect_is_dormer(),
                                    controller_ref.dormer_mode(),
                                    controller_ref.dormer_available(),
                                    controller_ref.dormer_views(),
                                    controller_ref.default_ceiling_height(),
                                    controller_ref.ceiling_heights_set(),
                                    controller_ref.new_storey_prompt(),
                                    controller_ref.floor_thickness(),
                                    crate::ui::render::ViewParams {
                                        zoom: v.zoom,
                                        offset_x: v.offset_x,
                                        offset_y: v.offset_y,
                                        yaw_deg: v.yaw_deg,
                                        pitch_deg: v.pitch_deg,
                                    },
                                )
                            };

                            // Prefill the position-prompt textbox with the prompt's drawn
                            // distance whenever the prompt IDENTITY (axis + selected
                            // reference option) changes — a fresh prompt or a cycled
                            // reference starts from its measured-as-drawn value; the
                            // user's in-progress typing on the SAME prompt is preserved.
                            if let Some(pp) = &position_prompt {
                                let key = (pp.axis, pp.kind, pp.option_idx);
                                if self.sketch_state.position_prompt_key != Some(key) {
                                    self.sketch_state.position_prompt_key = Some(key);
                                    self.sketch_state.position_input =
                                        format!("{:.2}", pp.current_distance);
                                }
                            } else {
                                self.sketch_state.position_prompt_key = None;
                            }

                            // Prefill the roof-point/ridge height box with the
                            // storey's default ceiling height when a Height stage
                            // OPENS (a sensible starting point for "how high is the
                            // ridge?"); the one-shot guard preserves the user's
                            // typing on the same stage across frames. The two flows
                            // share the buffer — they're never pending at once.
                            if ceiling_point_height_pending
                                || ceiling_ridge_height_pending
                                || ceiling_rect_height_pending
                            {
                                if !self.sketch_state.ceiling_apex_prefilled {
                                    self.sketch_state.ceiling_apex_prefilled = true;
                                    // A fresh open: bump the seq so the token changes
                                    // even when the same stage re-opens for a new area.
                                    self.sketch_state.ceiling_apex_open_seq =
                                        self.sketch_state.ceiling_apex_open_seq.wrapping_add(1);
                                    self.sketch_state.ceiling_apex_input = default_ceiling_height
                                        .map(|h| format!("{h:.2}"))
                                        .unwrap_or_default();
                                }
                            } else {
                                self.sketch_state.ceiling_apex_prefilled = false;
                            }

                            // Prefill the ridge LENGTH box with the pending
                            // ridge's drawn length when its stage OPENS — the
                            // stroke is approximate, so the typed value is
                            // the real dimension and the prefill is just the
                            // starting point. Same one-shot guard pattern.
                            if ceiling_ridge_length_pending {
                                if !self.sketch_state.ceiling_ridge_length_prefilled {
                                    self.sketch_state.ceiling_ridge_length_prefilled = true;
                                    self.sketch_state.ceiling_ridge_length_input =
                                        pending_roof_ridge_length
                                            .map(|l| format!("{l:.2}"))
                                            .unwrap_or_default();
                                }
                            } else {
                                self.sketch_state.ceiling_ridge_length_prefilled = false;
                            }

                            // Prefill the roof-RECT side-length box (shared
                            // buffer with the ridge length — never both
                            // pending) whenever the open stage's AXIS
                            // changes, so LenX → LenY refills with the drawn
                            // depth while preserving in-progress typing on
                            // the same stage.
                            if let Some(axis) = ceiling_rect_length_axis {
                                if self.sketch_state.ceiling_rect_len_key != Some(axis) {
                                    self.sketch_state.ceiling_rect_len_key = Some(axis);
                                    self.sketch_state.ceiling_ridge_length_input =
                                        pending_roof_rect_length
                                            .map(|l| format!("{l:.2}"))
                                            .unwrap_or_default();
                                }
                            } else {
                                self.sketch_state.ceiling_rect_len_key = None;
                            }

                            // Prefill the new-storey thickness box with the typical
                            // 0.25 m when the Thickness stage OPENS — "Use 0.25" /
                            // Enter then accepts it as-is. Same one-shot guard
                            // pattern as the roof prefills, so the user's typing
                            // on the open stage isn't clobbered every frame.
                            let thickness_pending = new_storey_prompt
                                .as_ref()
                                .is_some_and(|p| p.thickness_pending);
                            if thickness_pending {
                                if !self.sketch_state.new_storey_thickness_prefilled {
                                    self.sketch_state.new_storey_thickness_prefilled = true;
                                    self.sketch_state.new_storey_thickness_input =
                                        "0.25".to_string();
                                }
                            } else {
                                self.sketch_state.new_storey_thickness_prefilled = false;
                            }

                            // Deferred clear-on-success for the Heights field's input
                            // (raised by the SetDefaultCeilingHeight Ok path last frame).
                            if self.sketch_state.ceiling_height_input_clear {
                                self.sketch_state.categories.ceiling_height_input.clear();
                                self.sketch_state.ceiling_height_input_clear = false;
                            }
                            // Same deferred clear for the thickness field's input
                            // (raised by the SetFloorThickness Ok path last frame).
                            if self.sketch_state.floor_thickness_input_clear {
                                self.sketch_state.categories.floor_thickness_input.clear();
                                self.sketch_state.floor_thickness_input_clear = false;
                            }

                            let current_storey_idx =
                                self.app_controller.borrow().current_storey_idx();
                            let position_error = self.sketch_state.position_error.clone();
                            let ceiling_height_error =
                                self.sketch_state.ceiling_height_error.clone();
                            let step_error = self.sketch_state.step_error.clone();
                            let ceiling_step_error = self.sketch_state.ceiling_step_error.clone();
                            let roof_error = self.sketch_state.roof_error.clone();
                            let copy_height_pick_pending =
                                self.app_controller.borrow().copy_height_pick_pending();
                            let copy_height_error = self
                                .app_controller
                                .borrow()
                                .copy_height_error()
                                .map(str::to_owned);
                            let roof_ridge_notice = self
                                .app_controller
                                .borrow()
                                .roof_ridge_notice()
                                .map(str::to_owned);
                            let ceiling_continuation_notice = self
                                .app_controller
                                .borrow()
                                .ceiling_continuation_notice()
                                .map(str::to_owned);
                            let ceiling_continuation_confirm = self
                                .app_controller
                                .borrow()
                                .ceiling_continuation_confirm()
                                .map(str::to_owned);
                            let ceiling_continuation_source_options = self
                                .app_controller
                                .borrow()
                                .ceiling_continuation_source_options();
                            let ceiling_continued_region_rings = self
                                .app_controller
                                .borrow()
                                .ceiling_continued_region_rings();
                            let new_storey_error = self.sketch_state.new_storey_error.clone();
                            let floor_thickness_error =
                                self.sketch_state.floor_thickness_error.clone();
                            // Windows/doors view data (its own borrow — kept out
                            // of the big tuple above).
                            let (
                                openings_mode,
                                openings_tool,
                                opening_fields_prompt,
                                opening_dim_error,
                                existing_window_types,
                                elevation_view,
                                roof_window_selected,
                                delete_confirm_pending,
                            ) = {
                                let c = self.app_controller.borrow();
                                let prompt = c.opening_fields_prompt(
                                    self.sketch_state.categories.door_sidelights_mode,
                                    self.sketch_state.categories.complex_window_kind,
                                );
                                // The window-type picker list is only needed while a
                                // placement that offers it is in flight.
                                let existing_types = match &prompt {
                                    Some(p) if p.offer_window_type => c.existing_window_types(),
                                    _ => Vec::new(),
                                };
                                (
                                    c.openings_mode(),
                                    c.openings_tool(),
                                    prompt,
                                    c.opening_dim_error().map(str::to_owned),
                                    existing_types,
                                    c.elevation_view(),
                                    c.roof_window_selection(),
                                    c.delete_confirm_pending(),
                                )
                            };
                            // Owned partition view (the overlay takes a `&`); `None`
                            // when not partitioning.
                            let partition_view = self.app_controller.borrow().partition_view();

                            let controller = self.app_controller.clone();
                            let splice_debug = self.app_controller.borrow().splice_debug_overlay();
                            let snap_tags = self.app_controller.borrow().snap_overlay();

                            // Autosave status for the State category, derived
                            // from the idle-autosave bookkeeping above: when the
                            // controller is clean, everything up to the last
                            // edit is persisted, so "saved Ns ago" reports the
                            // time since that edit (the actual write happened
                            // slightly later — close enough for a status line).
                            let autosave_status = {
                                let now = ui.input(|i| i.time);
                                if self.app_controller.borrow().is_dirty() {
                                    Some("autosave pending…".to_string())
                                } else if self.last_change_time > 0.0 {
                                    Some(format!(
                                        "saved {:.0}s ago",
                                        (now - self.last_change_time).max(0.0)
                                    ))
                                } else {
                                    // No edits this session — nothing to report.
                                    None
                                }
                            };

                            // Per-phase checklist + phase state — recomputed
                            // from the (already materialized) editing model for
                            // the floating checklist window + phase signpost.
                            let (
                                current_phase,
                                mapping_complete,
                                surface_counts,
                                structure_progress,
                                structure_kinds,
                                structure_select,
                                checklist,
                            ) = {
                                let c = self.app_controller.borrow();
                                let phase = c.current_phase();
                                let checklist = match phase {
                                    AssessmentPhase::Mapping => c.stage_a_checklist(),
                                    AssessmentPhase::Structure => c.structure_checklist(),
                                    AssessmentPhase::Objects => c.objects_checklist(),
                                    AssessmentPhase::Final => c.final_details_checklist(),
                                };
                                (
                                    phase,
                                    c.mapping_complete(),
                                    c.current_surface_counts(),
                                    c.structure_progress(),
                                    c.structure_kinds().clone(),
                                    c.structure_select(),
                                    checklist,
                                )
                            };
                            // External elevation photos (Front/Right/Left/Back)
                            // for the floating "Add external pictures" panel,
                            // plus which sides are DEFERRED to the end (the
                            // panel's Defer toggles + the Final-phase prompt).
                            let (external_photos, deferred_photo_sides) = {
                                let c = self.app_controller.borrow();
                                (c.external_photos(), c.deferred_photo_sides())
                            };

                            // Stage-1 review: whether it's confirmed (the Structure
                            // gate) and, when the review is open, the current side's
                            // elevation + photo for the takeover overlay.
                            let (mapping_reviewed, mapping_review_view) = {
                                let c = self.app_controller.borrow();
                                let view = if c.mapping_review_mode() {
                                    c.mapping_review_view()
                                } else {
                                    None
                                };
                                (c.mapping_reviewed(), view)
                            };
                            let (structure_reviewed, structure_complete, structure_review_view) = {
                                let c = self.app_controller.borrow();
                                let view = if c.structure_review_mode() {
                                    c.structure_review_view()
                                } else {
                                    None
                                };
                                (c.structure_reviewed(), c.structure_complete(), view)
                            };
                            let (objects_reviewed, details_reviewed) = {
                                let c = self.app_controller.borrow();
                                (c.objects_reviewed(), c.details_reviewed())
                            };
                            let (show_ber_complete, show_deferred_photos_prompt, show_summary) = {
                                let c = self.app_controller.borrow();
                                (
                                    c.show_ber_complete(),
                                    c.show_deferred_photos_prompt(),
                                    c.show_summary(),
                                )
                            };
                            // Phase-4 (Final) view-data (only while in that phase).
                            let (
                                final_details,
                                sheltering,
                                orientation_deg,
                                site_satellite_image,
                                north_image,
                                final_mode,
                                final_complete,
                            ) = {
                                let c = self.app_controller.borrow();
                                if c.current_phase() == AssessmentPhase::Final {
                                    // The image overlay shows only while aligning.
                                    let north = (c.final_mode()
                                        == crate::controller::app_controller::FinalMode::Align)
                                        .then(|| c.north_image())
                                        .flatten();
                                    (
                                        c.final_details(),
                                        c.sheltering(),
                                        c.orientation_deg(),
                                        c.site_satellite_image_path(),
                                        north,
                                        c.final_mode(),
                                        c.final_complete(),
                                    )
                                } else {
                                    (
                                        Default::default(),
                                        Default::default(),
                                        0.0,
                                        None,
                                        None,
                                        crate::controller::app_controller::FinalMode::Form,
                                        false,
                                    )
                                }
                            };
                            // Phase-3 (Objects) view-data: room pins + the open
                            // room's panel (only while in the Objects phase).
                            let (
                                object_pins,
                                object_pins_world,
                                active_object_room,
                                object_review_view,
                                objects_present,
                            ) = {
                                let c = self.app_controller.borrow();
                                if c.current_phase() == AssessmentPhase::Objects {
                                    let rv = if c.object_review_mode() {
                                        c.object_review_view()
                                    } else {
                                        None
                                    };
                                    (
                                        c.object_rooms_view(),
                                        c.object_pin_world_points(),
                                        c.active_object_room_view(),
                                        rv,
                                        c.has_object_rooms(),
                                    )
                                } else {
                                    (Vec::new(), Vec::new(), None, None, false)
                                }
                            };
                            // Object kinds already recorded house-wide — lets the
                            // objects panel decide each kind's photo requirement
                            // (auto-camera + OK gate).
                            let object_recorded_kinds =
                                self.app_controller.borrow().recorded_object_kinds();

                            // Owned copy of the selected region ids so the view
                            // can borrow it without holding the controller.
                            let structure_selected = self
                                .app_controller
                                .borrow()
                                .structure_selected_ids()
                                .clone();
                            // Owned copy of the in-flight "Edit attributes" session.
                            let structure_edit =
                                self.app_controller.borrow().structure_edit().cloned();
                            // The existing constructions a selection can join (only
                            // needed while an edit session is open).
                            let structure_existing_groups = if structure_edit.is_some() {
                                self.app_controller.borrow().structure_existing_groups()
                            } else {
                                Vec::new()
                            };
                            // Owned copy of the reusable construction-layer library
                            // (the build-up editor's pick list).
                            let layer_library = self
                                .app_controller
                                .borrow()
                                .construction_layer_library()
                                .to_vec();
                            // Candidate regions of a pending Structure carve.
                            let structure_cut_regions =
                                self.app_controller.borrow().structure_cut_regions();
                            // The in-flight Structure split ring (highlighted
                            // while it's being dimensioned).
                            let pending_structure_split_polygon = self
                                .app_controller
                                .borrow()
                                .pending_structure_split_polygon();
                            // Staged (not-yet-split) carve rings — the pending
                            // selection shading until "Edit attributes" commits.
                            let pending_structure_carves = self
                                .app_controller
                                .borrow()
                                .pending_structure_carve_polygons();
                            // The same rings tagged by storey, for the
                            // all-storeys canvas cells.
                            let pending_structure_carves_by_storey = self
                                .app_controller
                                .borrow()
                                .pending_structure_carve_polygons_by_storey();
                            // The wall-inclusion ask for a just-dimensioned
                            // carve (highlighted edge + Include/Exclude).
                            let wall_inclusion_prompt =
                                self.app_controller.borrow().wall_inclusion_prompt();

                            // Floor-exposure tool: mode flags + the overlaid
                            // all-storeys floor view data.
                            let (
                                floor_exposure_mode,
                                floor_exposure_gate_pending,
                                floor_exposure_select,
                                floor_exposure_storeys,
                                floor_exposure_carve_polygon,
                                floor_exposure_cut_regions,
                            ) = {
                                let c = self.app_controller.borrow();
                                (
                                    c.floor_exposure_mode(),
                                    c.floor_exposure_gate_pending(),
                                    c.floor_exposure_select(),
                                    c.floor_exposure_view(),
                                    c.pending_floor_exposure_carve_polygon(),
                                    c.floor_exposure_cut_regions(),
                                )
                            };

                            // Ceiling-exposure tool: mode flags + the overlaid
                            // all-storeys ceiling view data (mirror of the floor
                            // tool above).
                            let (
                                ceiling_exposure_mode,
                                ceiling_exposure_gate_pending,
                                ceiling_exposure_select,
                                ceiling_exposure_storeys,
                                ceiling_exposure_carve_polygon,
                                ceiling_exposure_cut_regions,
                            ) = {
                                let c = self.app_controller.borrow();
                                (
                                    c.ceiling_exposure_mode(),
                                    c.ceiling_exposure_gate_pending(),
                                    c.ceiling_exposure_select(),
                                    c.ceiling_exposure_view(),
                                    c.pending_ceiling_exposure_carve_polygon(),
                                    c.ceiling_exposure_cut_regions(),
                                )
                            };

                            // Phase-1 roof step: mode flags + the whole-house
                            // top-storey-shaded context.
                            let (
                                roof_mode,
                                roof_gate_pending,
                                roof_context,
                                roof_regions,
                                roof_area_can_pick_shape,
                                roof_area_ceiling_sloped,
                                roof_area_ridge_pending,
                                roof_area_hip_ridge_count,
                                roof_area_pitch_pending,
                                roof_area_can_centre_ridge,
                                roof_area_ridge_lines,
                            ) = {
                                let c = self.app_controller.borrow();
                                (
                                    c.roof_mode(),
                                    c.roof_gate_pending(),
                                    c.roof_context(),
                                    c.roof_regions_view(),
                                    c.roof_area_can_pick_shape(),
                                    c.roof_area_ceiling_is_sloped(),
                                    c.roof_area_ridge_pending(),
                                    c.roof_area_hip_ridge_count(),
                                    c.roof_area_pitch_pending(),
                                    c.roof_area_can_centre_ridge(),
                                    c.pending_roof_area_ridge_lines(),
                                )
                            };

                            let navigation = self.app_controller.borrow().navigation_mode();
                            let view = SketchView {
                                current: stroke.as_ref().map(EguiStroke::from),
                                mode,
                                display: display_mode,
                                edit_tool: edit_mode,
                                navigation,
                                ber: ber.as_ref(),
                                current_storey_idx,
                                review: review.as_ref(),
                                review_remaining,
                                has_floor_plan,
                                pending_redraw,
                                dim_prompt,
                                dim_input: &mut self.sketch_state.dimension_input,
                                dim_scale_set,
                                dim_error: dim_error.as_deref(),
                                dimensioned_walls: &dimensioned_walls,
                                copy_wall_labels: &copy_wall_labels,
                                position_prompt,
                                position_input: &mut self.sketch_state.position_input,
                                position_error: position_error.as_deref(),
                                arc_preview: arc_preview.as_deref(),
                                curve_bulge,
                                living_area_mode,
                                living_area_polygons: &living_area_polygons,
                                living_area_wall_labels: &living_area_wall_labels,
                                active_living_area,
                                pending_cut_regions: &pending_cut_regions,
                                exposure_mode,
                                exposure_walls: &exposure_walls,
                                exposure_selection,
                                riser_views: &riser_views,
                                exposure_start_input: &mut self.sketch_state.exposure_start_input,
                                exposure_end_input: &mut self.sketch_state.exposure_end_input,
                                partition_mode,
                                partition_view: partition_view.as_ref(),
                                partition_dim_input: &mut self.sketch_state.partition_dim_input,
                                phase_mode,
                                phase_region_polygons: &phase_region_polygons,
                                phase_delete_labels: &phase_delete_labels,
                                pending_phase_polygon: &pending_phase_polygon,
                                pending_phase_cut_regions: &pending_phase_cut_regions,
                                phase_choices: &phase_choices,
                                phase_pick_pending,
                                step_mode,
                                step_region_polygons: &step_region_polygons,
                                pending_step_polygon: &pending_step_polygon,
                                pending_step_cut_regions: &pending_step_cut_regions,
                                step_pick_pending,
                                step_amount_input: &mut self.sketch_state.step_amount_input,
                                step_error: step_error.as_deref(),
                                ceiling_step_mode,
                                ceiling_step_available,
                                ceiling_step_region_polygons: &ceiling_step_region_polygons,
                                ceiling_divider_lines: &ceiling_divider_lines,
                                ceiling_step_editing: ceiling_step_editing.clone(),
                                ceiling_step_current_height,
                                pending_ceiling_step_line,
                                pending_ceiling_step_side_rects: &pending_ceiling_step_side_rects,
                                ceiling_step_snap_tag: ceiling_step_snap_tag.as_ref(),
                                ceiling_step_orientation_pending,
                                ceiling_step_side_pending,
                                ceiling_step_amount_pending,
                                ceiling_step_pending,
                                ceiling_amount_input: &mut self.sketch_state.ceiling_amount_input,
                                ceiling_step_error: ceiling_step_error.as_deref(),
                                copy_height_pick_pending,
                                copy_height_error: copy_height_error.as_deref(),
                                ceiling_point_mode,
                                ceiling_point_available,
                                ceiling_apex_views: &ceiling_apex_views,
                                ceiling_fan_markers: &ceiling_fan_marker_views,
                                pending_roof_point,
                                ceiling_point_snap_tag: ceiling_point_snap_tag.as_ref(),
                                ceiling_point_position_pending,
                                ceiling_point_height_pending,
                                ceiling_point_pending,
                                ceiling_apex_input: &mut self.sketch_state.ceiling_apex_input,
                                ceiling_apex_open_seq: self.sketch_state.ceiling_apex_open_seq,
                                ceiling_ridge_views: &ceiling_ridge_views,
                                pending_roof_ridge_line,
                                ceiling_ridge_snap_tag: ceiling_ridge_snap_tag.as_ref(),
                                ceiling_ridge_position_pending,
                                ceiling_ridge_length_pending,
                                ceiling_ridge_length_anchored,
                                ceiling_ridge_height_pending,
                                ceiling_ridge_meet_edge_height,
                                ceiling_ridge_pending,
                                ceiling_ridge_area_options: self
                                    .app_controller
                                    .borrow()
                                    .ceiling_ridge_area_options(),
                                reset_ceiling_pending,
                                neighbour_continuation_mode,
                                storey_continuation_mode,
                                ceiling_continuation_picking_target,
                                ceiling_continuation_picking_source,
                                ceiling_continuation_source_options,
                                ceiling_continuation_confirm: ceiling_continuation_confirm
                                    .as_deref(),
                                ceiling_continued_region_rings: &ceiling_continued_region_rings,
                                ceiling_rect_views: &ceiling_rect_views,
                                pending_roof_rect_outline,
                                ceiling_rect_position_pending,
                                ceiling_rect_length_axis,
                                ceiling_rect_height_pending,
                                ceiling_rect_pending,
                                dormer_mode,
                                dormer_available,
                                dormer_views: &dormer_views,
                                ceiling_rect_is_dormer,
                                ceiling_ridge_length_input: &mut self
                                    .sketch_state
                                    .ceiling_ridge_length_input,
                                roof_error: roof_error.as_deref(),
                                roof_ridge_notice: roof_ridge_notice.as_deref(),
                                ceiling_continuation_notice: ceiling_continuation_notice.as_deref(),
                                ceiling_height_error: ceiling_height_error.as_deref(),
                                default_ceiling_height,
                                ceiling_heights_set,
                                new_storey_prompt,
                                new_storey_thickness_input: &mut self
                                    .sketch_state
                                    .new_storey_thickness_input,
                                new_storey_error: new_storey_error.as_deref(),
                                floor_thickness,
                                floor_thickness_error: floor_thickness_error.as_deref(),
                                edit_selected_wall,
                                edit_selected_vertex,
                                edit_drag_prompt,
                                edit_drag_input: &mut self.sketch_state.edit_drag_input,
                                edit_drag_error: edit_drag_error.as_deref(),
                                vertex_drop_prompt,
                                vertex_drop_inputs: &mut self.sketch_state.vertex_drop_inputs,
                                vertex_drop_error: vertex_drop_error.as_deref(),
                                can_undo,
                                can_redo,
                                view_params,
                                splice_debug: &splice_debug,
                                snap_tags: &snap_tags,
                                cat_state: &mut self.sketch_state.categories,
                                delete_storey_armed: &mut self.sketch_state.delete_storey_armed,
                                laser: &mut self.sketch_state.laser,
                                ceiling_entry_open: self.sketch_state.ceiling_entry_open,
                                autosave_status,
                                openings_mode,
                                openings_tool,
                                opening_fields_prompt,
                                opening_dim_error: opening_dim_error.as_deref(),
                                opening_width_input: &mut self.sketch_state.opening_width_input,
                                opening_height_input: &mut self.sketch_state.opening_height_input,
                                opening_full_width: &mut self.sketch_state.opening_full_width,
                                opening_full_height: &mut self.sketch_state.opening_full_height,
                                sidelight_left_input: &mut self.sketch_state.sidelight_left_input,
                                sidelight_left_sill_input: &mut self
                                    .sketch_state
                                    .sidelight_left_sill_input,
                                sidelight_left_has_sill: &mut self
                                    .sketch_state
                                    .sidelight_left_has_sill,
                                sidelight_right_input: &mut self.sketch_state.sidelight_right_input,
                                sidelight_right_sill_input: &mut self
                                    .sketch_state
                                    .sidelight_right_sill_input,
                                sidelight_right_has_sill: &mut self
                                    .sketch_state
                                    .sidelight_right_has_sill,
                                transom_input: &mut self.sketch_state.transom_input,
                                transom_semicircular: &mut self.sketch_state.transom_semicircular,
                                door_width_input: &mut self.sketch_state.door_width_input,
                                door_height_input: &mut self.sketch_state.door_height_input,
                                sidelight_left_present: &mut self
                                    .sketch_state
                                    .sidelight_left_present,
                                sidelight_right_present: &mut self
                                    .sketch_state
                                    .sidelight_right_present,
                                window_shape_c_input: &mut self.sketch_state.window_shape_c_input,
                                window_shape_d_input: &mut self.sketch_state.window_shape_d_input,
                                wt_existing_id: &mut self.sketch_state.wt_existing_id,
                                wt_glazing: &mut self.sketch_state.wt_glazing,
                                wt_frame: &mut self.sketch_state.wt_frame,
                                wt_year_input: &mut self.sketch_state.wt_year_input,
                                wt_certified: &mut self.sketch_state.wt_certified,
                                wt_gap_input: &mut self.sketch_state.wt_gap_input,
                                wt_name_input: &mut self.sketch_state.wt_name_input,
                                overshading: &mut self.sketch_state.overshading,
                                pending_window_photos: &mut self.sketch_state.pending_window_photos,
                                existing_window_types,
                                elevation_view,
                                roof_window_selected,
                                delete_confirm_pending,
                                checklist,
                                external_photos,
                                deferred_photo_sides,
                                current_phase,
                                mapping_complete,
                                mapping_reviewed,
                                mapping_review_view,
                                structure_complete,
                                structure_reviewed,
                                objects_reviewed,
                                details_reviewed,
                                final_details,
                                sheltering,
                                orientation_deg,
                                site_satellite_image,
                                north_image,
                                final_mode,
                                final_complete,
                                show_ber_complete,
                                show_deferred_photos_prompt,
                                show_summary,
                                structure_review_view,
                                object_pins,
                                object_pins_world,
                                active_object_room,
                                object_selected_category: &mut self
                                    .sketch_state
                                    .object_selected_category,
                                object_selected_kind: &mut self.sketch_state.object_selected_kind,
                                object_count_input: &mut self.sketch_state.object_count_input,
                                object_note_input: &mut self.sketch_state.object_note_input,
                                object_other_name: &mut self.sketch_state.object_other_name,
                                object_pending_photos: &mut self.sketch_state.object_pending_photos,
                                object_recorded_kinds: &object_recorded_kinds,
                                object_auto_photo_for: &mut self.sketch_state.object_auto_photo_for,
                                photo_annotator: &mut self.sketch_state.photo_annotator,
                                objects_present,
                                object_review_view,
                                surface_counts,
                                structure_progress,
                                structure_kinds,
                                structure_select,
                                structure_selected: &structure_selected,
                                structure_edit: structure_edit.as_ref(),
                                structure_existing_groups,
                                structure_cut_regions: &structure_cut_regions,
                                pending_structure_split_polygon: &pending_structure_split_polygon,
                                pending_structure_carves: &pending_structure_carves,
                                pending_structure_carves_by_storey:
                                    &pending_structure_carves_by_storey,
                                wall_inclusion_prompt,
                                layer_library: &layer_library,
                                floor_exposure_mode,
                                floor_exposure_gate_pending,
                                floor_exposure_select,
                                floor_exposure_storeys: &floor_exposure_storeys,
                                floor_exposure_carve_polygon: &floor_exposure_carve_polygon,
                                floor_exposure_cut_regions: &floor_exposure_cut_regions,
                                roof_mode,
                                roof_gate_pending,
                                roof_context: &roof_context,
                                roof_regions: &roof_regions,
                                roof_area_can_pick_shape,
                                roof_area_ceiling_sloped,
                                roof_area_ridge_pending,
                                roof_area_hip_ridge_count,
                                roof_area_pitch_pending,
                                roof_area_can_centre_ridge,
                                roof_area_ridge_lines,
                                roof_pitch_input: &mut self.sketch_state.roof_pitch_input,
                                ceiling_exposure_mode,
                                ceiling_exposure_gate_pending,
                                ceiling_exposure_select,
                                ceiling_exposure_storeys: &ceiling_exposure_storeys,
                                ceiling_exposure_carve_polygon: &ceiling_exposure_carve_polygon,
                                ceiling_exposure_cut_regions: &ceiling_exposure_cut_regions,
                            };
                            let touch_nav = &mut self.sketch_state.touch_nav;
                            // A `BeginAnnotateExternalPhoto` can't open the
                            // annotator inside the event closure (the in-render
                            // view still borrows the page-state slot), so the
                            // request is parked here and honoured right after
                            // render.
                            let annotate_request: std::cell::Cell<
                                Option<ber_model::ber::common::ExternalSide>,
                            > = std::cell::Cell::new(None);
                            // A dimension-prompt wall-length copy tap can't
                            // edit the expression buffer inside the event
                            // closure (the in-render view mutably borrows
                            // `sketch_state.dimension_input`), so the tapped
                            // length is parked here and inserted right after
                            // render — same pattern as `annotate_request`.
                            let dim_copy_request: std::cell::Cell<Option<f32>> =
                                std::cell::Cell::new(None);
                            // Dismissing the OPEN object room (a tap outside
                            // its box / its Done) must first COMMIT a
                            // non-empty in-flight entry like OK would — but
                            // the entry buffers are mutably borrowed by the
                            // render view, so the dismissal is parked here and
                            // honoured right after render (same pattern as
                            // `annotate_request`).
                            let object_dismiss_request: std::cell::Cell<bool> =
                                std::cell::Cell::new(false);
                            // Opening the ATTIC bucket goes through the same
                            // deferred flush (a previous room's in-flight
                            // entry must commit before the attic opens).
                            let attic_open_request: std::cell::Cell<bool> =
                                std::cell::Cell::new(false);
                            SketchPage::render(ui, view, touch_nav, |event| match event {
                                UiEvent::Sketch(sketch_event) => match sketch_event {
                                    // Logged, not expected: after a two-finger
                                    // CancelStroke, a trailing AddPoint/EndStroke
                                    // can legitimately find no active stroke.
                                    SketchEvent::AddPoint(p) => log_err(
                                        "sketch_add_point",
                                        controller
                                            .borrow_mut()
                                            .sketch_add_point(pos2_to_point2f(p)),
                                    ),
                                    SketchEvent::EndStroke(p) => log_err(
                                        "sketch_end_stroke",
                                        controller
                                            .borrow_mut()
                                            .sketch_end_stroke(pos2_to_point2f(p)),
                                    ),
                                    SketchEvent::CancelStroke => {
                                        controller.borrow_mut().sketch_cancel_stroke();
                                    }
                                    SketchEvent::StartStroke(p) => log_err(
                                        "sketch_start_stroke",
                                        controller
                                            .borrow_mut()
                                            .sketch_start_stroke(pos2_to_point2f(p)),
                                    ),
                                    SketchEvent::ChangeMode(m) => {
                                        // Switching 2D↔3D drops the active tool —
                                        // CAD-style: the canvas returns to plain
                                        // navigation so a stray tap does nothing.
                                        let mut c = controller.borrow_mut();
                                        c.set_sketch_mode(m);
                                        c.enter_navigation_mode();
                                    }
                                    SketchEvent::ChangeEditTool(m) => {
                                        controller.borrow_mut().set_edit_mode(m)
                                    }
                                    SketchEvent::AcceptCollinearity => {
                                        log_err(
                                            "accept_current_review",
                                            controller.borrow_mut().accept_current_review(),
                                        );
                                    }
                                    SketchEvent::RejectCollinearity => {
                                        controller.borrow_mut().reject_current_review();
                                    }
                                    SketchEvent::SkipAllCollinearity => {
                                        controller.borrow_mut().skip_all_review();
                                    }
                                    SketchEvent::ZoomView {
                                        factor,
                                        anchor_x,
                                        anchor_y,
                                        canvas_center_x,
                                        canvas_center_y,
                                    } => {
                                        controller.borrow_mut().zoom_view(
                                            factor,
                                            (anchor_x, anchor_y),
                                            (canvas_center_x, canvas_center_y),
                                        );
                                    }
                                    SketchEvent::PanView { dx, dy } => {
                                        controller.borrow_mut().pan_view(dx, dy);
                                    }
                                    SketchEvent::ViewportWorldDiagonal { world_diagonal_m } => {
                                        controller
                                            .borrow_mut()
                                            .set_view_world_diagonal_m(world_diagonal_m);
                                    }
                                    SketchEvent::OrbitView { dyaw, dpitch } => {
                                        controller.borrow_mut().orbit_view(dyaw, dpitch);
                                    }
                                    SketchEvent::ResetView => {
                                        controller.borrow_mut().reset_view();
                                    }
                                    SketchEvent::ReplaceStorey => {
                                        log_err(
                                            "confirm_replace_storey",
                                            controller.borrow_mut().confirm_replace_storey(),
                                        );
                                    }
                                    SketchEvent::AddStoreyFromCurrent => {
                                        controller.borrow_mut().confirm_add_storey_from_current();
                                    }
                                    SketchEvent::IgnoreRedraw => {
                                        controller.borrow_mut().discard_pending_redraw();
                                    }
                                    SketchEvent::ChooseSnap { from, choice } => {
                                        controller.borrow_mut().choose_snap(
                                            domain::points::Point2f32::new(from.0, from.1),
                                            choice,
                                        );
                                    }
                                    SketchEvent::ApplyDimension(value) => {
                                        match controller.borrow_mut().apply_dimension(value) {
                                            Ok(()) => {
                                                self.sketch_state.dimension_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("apply_dimension: {}", e);
                                                self.sketch_state.dimension_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SkipDimension => {
                                        controller.borrow_mut().skip_current_dimension();
                                        self.sketch_state.dimension_error = None;
                                    }
                                    SketchEvent::CopyWallLengthIntoDimension(metres) => {
                                        // The buffer is mutably borrowed by the
                                        // in-render view — park the length and
                                        // insert after render (see the Cell).
                                        dim_copy_request.set(Some(metres));
                                    }
                                    SketchEvent::ApplyPositionDimension(value) => {
                                        match controller
                                            .borrow_mut()
                                            .apply_position_dimension(value)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.position_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("apply_position_dimension: {}", e);
                                                self.sketch_state.position_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SkipPositionDimension => {
                                        // A roof-ridge/rect skip at the last
                                        // location stage can trip the FINALIZE
                                        // placement check: surface it like a
                                        // typed rejection.
                                        match controller.borrow_mut().skip_position_dimension() {
                                            Ok(()) => {
                                                self.sketch_state.position_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("skip_position_dimension: {}", e);
                                                self.sketch_state.position_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::CyclePositionReference => {
                                        controller.borrow_mut().cycle_position_reference();
                                        self.sketch_state.position_error = None;
                                    }
                                    SketchEvent::SetCurveBulge(ratio) => {
                                        controller.borrow_mut().set_pending_arc_bulge(ratio);
                                    }
                                    SketchEvent::CommitCurve => {
                                        log_err(
                                            "commit_pending_arc",
                                            controller.borrow_mut().commit_pending_arc(),
                                        );
                                    }
                                    SketchEvent::SetLivingAreaMode(on) => {
                                        controller.borrow_mut().set_living_area_mode(on);
                                    }
                                    SketchEvent::LivingAreaStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_living_area_stroke_event(&points);
                                    }
                                    SketchEvent::DeleteLivingArea(index) => {
                                        controller.borrow_mut().delete_living_area(index);
                                    }
                                    SketchEvent::ChooseCutRegion { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .choose_cut_region(world_x as f32, world_y as f32);
                                    }
                                    SketchEvent::SetPhaseMode(on) => {
                                        controller.borrow_mut().set_phase_mode(on);
                                    }
                                    SketchEvent::PhaseRegionStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_phase_region_stroke_event(&points);
                                    }
                                    SketchEvent::AssignPhase(phase) => {
                                        controller.borrow_mut().assign_phase_to_pending(phase);
                                    }
                                    SketchEvent::ChoosePhaseCutRegion { world_x, world_y } => {
                                        controller.borrow_mut().choose_phase_cut_region(
                                            world_x as f32,
                                            world_y as f32,
                                        );
                                    }
                                    SketchEvent::CancelPendingPhase => {
                                        controller.borrow_mut().cancel_pending_phase();
                                    }
                                    SketchEvent::DeletePhaseRegion(index) => {
                                        controller.borrow_mut().delete_phase_region(index);
                                    }
                                    SketchEvent::AssignRestOriginal => {
                                        controller.borrow_mut().assign_rest_to_original();
                                    }
                                    SketchEvent::SetStepMode(on) => {
                                        controller.borrow_mut().set_step_mode(on);
                                    }
                                    SketchEvent::StepRegionStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_step_region_stroke_event(&points);
                                    }
                                    SketchEvent::AssignStepLevel(level_m) => {
                                        match controller
                                            .borrow_mut()
                                            .assign_step_level_to_pending(level_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.step_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_step_level_to_pending: {}", e);
                                                self.sketch_state.step_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::ChooseStepCutRegion { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .choose_step_cut_region(world_x as f32, world_y as f32);
                                    }
                                    SketchEvent::CancelPendingStep => {
                                        controller.borrow_mut().cancel_pending_step();
                                        self.sketch_state.step_error = None;
                                    }
                                    SketchEvent::DeleteStepRegion(index) => {
                                        controller.borrow_mut().delete_step_region(index);
                                    }
                                    SketchEvent::SetCeilingStepMode(on) => {
                                        controller.borrow_mut().set_ceiling_step_mode(on);
                                    }
                                    SketchEvent::CeilingStepStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_ceiling_step_stroke_event(&points);
                                    }
                                    SketchEvent::CeilingStepTap { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .ceiling_step_tap(world_x as f32, world_y as f32);
                                    }
                                    SketchEvent::ChooseCeilingStepOrientation { horizontal } => {
                                        controller
                                            .borrow_mut()
                                            .choose_ceiling_step_orientation(horizontal);
                                    }
                                    SketchEvent::AssignCeilingStepDelta(delta_m) => {
                                        match controller
                                            .borrow_mut()
                                            .assign_ceiling_step_delta(delta_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.ceiling_step_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_ceiling_step_delta: {}", e);
                                                self.sketch_state.ceiling_step_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::AssignCeilingStepAbsolute(h) => {
                                        match controller
                                            .borrow_mut()
                                            .assign_ceiling_step_absolute(h)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.ceiling_step_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("ceiling step (absolute): {}", e);
                                                self.sketch_state.ceiling_step_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::DeleteSelectedCeilingStep => {
                                        controller.borrow_mut().delete_selected_ceiling_step();
                                        self.sketch_state.ceiling_step_error = None;
                                    }
                                    SketchEvent::KeepCeilingSameHeight => {
                                        match controller.borrow_mut().keep_ceiling_same_height() {
                                            Ok(()) => {
                                                self.sketch_state.ceiling_step_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("keep_ceiling_same_height: {}", e);
                                                self.sketch_state.ceiling_step_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::UnsnapCeilingStepLine => {
                                        controller.borrow_mut().unsnap_ceiling_step_line();
                                        // The stage the error described is gone.
                                        self.sketch_state.ceiling_step_error = None;
                                    }
                                    SketchEvent::CancelPendingCeilingStep => {
                                        controller.borrow_mut().cancel_pending_ceiling_step();
                                        self.sketch_state.ceiling_step_error = None;
                                    }
                                    SketchEvent::DeleteCeilingStepRegion(ring) => {
                                        controller.borrow_mut().delete_ceiling_step_region(&ring);
                                    }
                                    SketchEvent::SetCeilingPointMode(on) => {
                                        controller.borrow_mut().set_roof_point_mode(on);
                                    }
                                    SketchEvent::CeilingPointTap { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .ceiling_point_tap(world_x as f32, world_y as f32);
                                    }
                                    SketchEvent::CeilingFanMarkerTap { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .toggle_ceiling_fan_vertex(world_x, world_y);
                                    }
                                    SketchEvent::RequestResetCeilingShape => {
                                        controller.borrow_mut().request_reset_ceiling_shape();
                                    }
                                    SketchEvent::ConfirmResetCeilingShape => {
                                        controller.borrow_mut().confirm_reset_ceiling_shape();
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::CancelResetCeilingShape => {
                                        controller.borrow_mut().cancel_reset_ceiling_shape();
                                    }
                                    SketchEvent::SetNeighbourContinuationMode(on) => {
                                        controller.borrow_mut().set_neighbour_continuation_mode(on);
                                    }
                                    SketchEvent::SetStoreyContinuationMode(on) => {
                                        controller.borrow_mut().set_storey_continuation_mode(on);
                                    }
                                    SketchEvent::CancelCeilingContinuation => {
                                        controller.borrow_mut().cancel_ceiling_continuation();
                                    }
                                    SketchEvent::ChooseCeilingContinuationSource(i) => {
                                        controller
                                            .borrow_mut()
                                            .choose_ceiling_continuation_source(i);
                                    }
                                    SketchEvent::AssignCeilingApexHeight(z_m) => {
                                        match controller.borrow_mut().assign_roof_apex(z_m) {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_apex: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::UnsnapCeilingPoint => {
                                        controller.borrow_mut().unsnap_roof_point();
                                        // The stage the error described is gone.
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::CancelPendingCeilingPoint => {
                                        controller.borrow_mut().cancel_pending_roof_point();
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::DeleteCeilingApex(index) => {
                                        controller.borrow_mut().delete_ceiling_apex(index);
                                    }
                                    SketchEvent::CeilingRidgeStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_roof_ridge_stroke_event(&points);
                                    }
                                    SketchEvent::AssignCeilingRidgeLength(len_m) => {
                                        match controller
                                            .borrow_mut()
                                            .assign_roof_ridge_length(len_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_ridge_length: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SkipCeilingRidgeLength => {
                                        // The skip can trip the FINALIZE placement
                                        // check (both positions snapped): surface it
                                        // like a typed rejection.
                                        match controller.borrow_mut().skip_roof_ridge_length() {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("skip_roof_ridge_length: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::AssignCeilingRidgeHeight(z_m) => {
                                        match controller.borrow_mut().assign_roof_ridge_height(z_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_ridge_height: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::ChooseCeilingRidgeArea(i) => {
                                        if let Err(e) =
                                            controller.borrow_mut().choose_roof_ridge_area(i)
                                        {
                                            log::error!("choose_roof_ridge_area: {}", e);
                                            self.sketch_state.roof_error = Some(e);
                                        }
                                    }
                                    SketchEvent::CentreRoofPosition => {
                                        // The rejection lands on the open
                                        // position prompt, like a typed value.
                                        match controller.borrow_mut().centre_roof_position() {
                                            Ok(()) => {
                                                self.sketch_state.position_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("centre_roof_position: {}", e);
                                                self.sketch_state.position_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::MeetOtherEdgeRidgeHeight => {
                                        // Sets the ridge to the raised neighbour's
                                        // height; a rejection lands on the height stage.
                                        match controller
                                            .borrow_mut()
                                            .meet_other_edge_roof_ridge_height()
                                        {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!(
                                                    "meet_other_edge_roof_ridge_height: {}",
                                                    e
                                                );
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::UnsnapCeilingRidge => {
                                        controller.borrow_mut().unsnap_roof_ridge();
                                        // The stage the error described is gone.
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::CancelPendingCeilingRidge => {
                                        controller.borrow_mut().cancel_pending_roof_ridge();
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::AssignCeilingRectLength(len_m) => {
                                        match controller.borrow_mut().assign_roof_rect_length(len_m)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_rect_length: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SkipCeilingRectLength => {
                                        // The LenY skip can trip the FINALIZE
                                        // placement check (no position question
                                        // opened): surface it like a typed
                                        // rejection.
                                        match controller.borrow_mut().skip_roof_rect_length() {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("skip_roof_rect_length: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::AssignCeilingRectHeight(z_m) => {
                                        match controller.borrow_mut().assign_roof_rect_height(z_m) {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_roof_rect_height: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::CancelPendingCeilingRect => {
                                        controller.borrow_mut().cancel_pending_roof_rect();
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::SetDormerMode(on) => {
                                        controller.borrow_mut().set_dormer_mode(on);
                                    }
                                    SketchEvent::DormerStrokeWorld { points } => {
                                        controller.borrow_mut().apply_dormer_stroke_event(&points);
                                    }
                                    SketchEvent::AssignDormerHeight(z_m) => {
                                        match controller.borrow_mut().assign_dormer_height(z_m) {
                                            Ok(()) => {
                                                self.sketch_state.roof_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_dormer_height: {}", e);
                                                self.sketch_state.roof_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::StartCopyHeight => {
                                        controller.borrow_mut().start_copy_height();
                                    }
                                    SketchEvent::CopyHeightTap {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .copy_height_tap(world_x, world_y, tol);
                                    }
                                    SketchEvent::CancelCopyHeight => {
                                        controller.borrow_mut().cancel_copy_height();
                                    }
                                    SketchEvent::SetExposureMode(on) => {
                                        controller.borrow_mut().set_exposure_mode(on);
                                    }
                                    SketchEvent::ExposurePointerDown {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .exposure_pointer_down(world_x, world_y, tol);
                                    }
                                    SketchEvent::ExposurePointerDrag {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .exposure_pointer_drag(world_x, world_y, tol);
                                    }
                                    SketchEvent::ExposurePointerUp => {
                                        controller.borrow_mut().exposure_pointer_up();
                                    }
                                    SketchEvent::SetExposureSectionRange { start_m, end_m } => {
                                        controller
                                            .borrow_mut()
                                            .set_exposure_section_range(start_m, end_m);
                                    }
                                    SketchEvent::SetWallExposure(exposure) => {
                                        controller.borrow_mut().set_wall_exposure(exposure);
                                    }
                                    SketchEvent::ClearWallExposure => {
                                        controller.borrow_mut().clear_wall_exposure();
                                    }
                                    SketchEvent::SetAllRemainingExposed => {
                                        controller.borrow_mut().set_all_remaining_exposed();
                                    }
                                    SketchEvent::OpenWallPartition => {
                                        controller.borrow_mut().open_wall_partition();
                                    }
                                    SketchEvent::CloseWallPartition => {
                                        controller.borrow_mut().close_wall_partition();
                                    }
                                    SketchEvent::PartitionStrokePoint { along_m, height_m } => {
                                        controller
                                            .borrow_mut()
                                            .partition_stroke_point(along_m, height_m);
                                    }
                                    SketchEvent::PartitionStrokeEnd => {
                                        controller.borrow_mut().partition_stroke_end();
                                    }
                                    SketchEvent::PartitionSelectRegion { along_m, height_m } => {
                                        controller
                                            .borrow_mut()
                                            .select_partition_region(along_m, height_m);
                                    }
                                    SketchEvent::SetPartitionRegionExposure(exposure) => {
                                        controller
                                            .borrow_mut()
                                            .set_partition_region_exposure(exposure);
                                    }
                                    SketchEvent::PartitionDimEntry(value) => {
                                        controller.borrow_mut().partition_dim_entry(value);
                                    }
                                    SketchEvent::PartitionDimSkip => {
                                        controller.borrow_mut().partition_dim_skip();
                                    }
                                    SketchEvent::UnsnapPartitionEnd { end_b } => {
                                        controller.borrow_mut().unsnap_partition_end(end_b);
                                    }
                                    SketchEvent::ClearWallPartition => {
                                        controller.borrow_mut().clear_active_wall_partition();
                                    }
                                    SketchEvent::SetOpeningsMode(on) => {
                                        controller.borrow_mut().set_openings_mode(on);
                                    }
                                    SketchEvent::SelectOpeningTool(tool) => {
                                        controller.borrow_mut().select_opening_tool(tool);
                                    }
                                    SketchEvent::OpeningsPointerDown {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .openings_pointer_down(world_x, world_y, tol);
                                    }
                                    // The placement methods set their own inline error
                                    // (read back via `opening_dim_error()`), so the
                                    // Result is intentionally dropped here.
                                    SketchEvent::PlaceOpening {
                                        width_m,
                                        height_m,
                                        full_width,
                                        full_height,
                                        door_sidelights,
                                        complex_window,
                                        window_type,
                                        overshading,
                                        photos,
                                    } => {
                                        let _ = controller.borrow_mut().place_opening(
                                            width_m,
                                            height_m,
                                            full_width,
                                            full_height,
                                            door_sidelights,
                                            complex_window,
                                            window_type,
                                            overshading,
                                            photos,
                                        );
                                    }
                                    SketchEvent::FillOpeningSurface {
                                        window_type,
                                        overshading,
                                        photos,
                                    } => {
                                        let _ = controller.borrow_mut().fill_opening_surface(
                                            window_type,
                                            overshading,
                                            photos,
                                        );
                                    }
                                    SketchEvent::CancelOpeningPlacement => {
                                        controller.borrow_mut().cancel_opening_placement();
                                    }
                                    SketchEvent::ElevationPointerDown { along_m, height_m } => {
                                        controller
                                            .borrow_mut()
                                            .elevation_pointer_down(along_m, height_m);
                                    }
                                    SketchEvent::ElevationPointerDrag { along_m, height_m } => {
                                        controller
                                            .borrow_mut()
                                            .elevation_pointer_drag(along_m, height_m);
                                    }
                                    SketchEvent::ElevationPointerUp => {
                                        controller.borrow_mut().elevation_pointer_up();
                                    }
                                    SketchEvent::EditSelectedOpening => {
                                        controller.borrow_mut().start_edit_selected_window();
                                    }
                                    // Sets its own inline error (read back via
                                    // `opening_dim_error()`) — Result dropped like
                                    // the placement events above.
                                    SketchEvent::ApplyOpeningEdit {
                                        width_m,
                                        height_m,
                                        full_width,
                                        full_height,
                                        window_type,
                                        overshading,
                                        photos,
                                    } => {
                                        let _ = controller.borrow_mut().apply_opening_edit(
                                            width_m,
                                            height_m,
                                            full_width,
                                            full_height,
                                            window_type,
                                            overshading,
                                            photos,
                                        );
                                    }
                                    SketchEvent::OpeningsPointerDrag { world_x, world_y } => {
                                        controller
                                            .borrow_mut()
                                            .openings_pointer_drag(world_x, world_y);
                                    }
                                    SketchEvent::OpeningsPointerUp => {
                                        controller.borrow_mut().openings_pointer_up();
                                    }
                                    SketchEvent::DeleteSelectedOpening => {
                                        controller.borrow_mut().request_delete_opening();
                                    }
                                    SketchEvent::ConfirmDeleteOpening => {
                                        controller.borrow_mut().confirm_delete_opening();
                                    }
                                    SketchEvent::CancelDeleteOpening => {
                                        controller.borrow_mut().cancel_delete();
                                    }
                                    SketchEvent::CloseElevation => {
                                        controller.borrow_mut().close_elevation();
                                    }
                                    SketchEvent::OpenMappingReview => {
                                        controller.borrow_mut().set_mapping_review_mode(true);
                                    }
                                    SketchEvent::CloseMappingReview => {
                                        controller.borrow_mut().set_mapping_review_mode(false);
                                    }
                                    SketchEvent::StepMappingReviewSide(delta) => {
                                        controller.borrow_mut().step_mapping_review_side(delta);
                                    }
                                    SketchEvent::ConfirmMappingReview => {
                                        controller.borrow_mut().confirm_mapping_review();
                                    }
                                    SketchEvent::MappingReviewPointerDown { x, z } => {
                                        controller.borrow_mut().mapping_review_pointer_down(x, z);
                                    }
                                    SketchEvent::MappingReviewPointerDrag { x, z } => {
                                        controller.borrow_mut().mapping_review_pointer_drag(x, z);
                                    }
                                    SketchEvent::MappingReviewPointerUp => {
                                        controller.borrow_mut().mapping_review_pointer_up();
                                    }
                                    SketchEvent::OpenStructureReview => {
                                        controller.borrow_mut().set_structure_review_mode(true)
                                    }
                                    SketchEvent::CloseStructureReview => {
                                        controller.borrow_mut().set_structure_review_mode(false)
                                    }
                                    SketchEvent::StepStructureReview(delta) => {
                                        controller.borrow_mut().step_structure_review(delta)
                                    }
                                    SketchEvent::ConfirmStructureReview => {
                                        controller.borrow_mut().confirm_structure_review()
                                    }
                                    // The three tap handlers below: a tap
                                    // while a room box is OPEN dismisses it —
                                    // that path goes through the DEFERRED
                                    // dismiss (`object_dismiss_request`) so a
                                    // non-empty entry commits first; with no
                                    // room open they open/create as usual.
                                    SketchEvent::ObjectsPointerDown {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        if controller.borrow().active_object_room().is_some() {
                                            object_dismiss_request.set(true);
                                        } else {
                                            controller
                                                .borrow_mut()
                                                .objects_pointer_down(world_x, world_y, tol);
                                        }
                                    }
                                    SketchEvent::ObjectsPointerDownAllStoreys {
                                        storey_idx,
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        if controller.borrow().active_object_room().is_some() {
                                            object_dismiss_request.set(true);
                                        } else {
                                            controller.borrow_mut().objects_pointer_down_on(
                                                storey_idx, world_x, world_y, tol,
                                            );
                                        }
                                    }
                                    SketchEvent::ObjectsPointerDown3D {
                                        storey_idx,
                                        world_x,
                                        world_y,
                                        z_m,
                                        pitch_deg,
                                        on_roof,
                                        tol,
                                    } => {
                                        if controller.borrow().active_object_room().is_some() {
                                            object_dismiss_request.set(true);
                                        } else {
                                            controller.borrow_mut().objects_pointer_down_3d(
                                                storey_idx, world_x, world_y, z_m, pitch_deg,
                                                on_roof, tol,
                                            );
                                        }
                                    }
                                    SketchEvent::CloseObjectRoom => {
                                        object_dismiss_request.set(true);
                                    }
                                    SketchEvent::RenameObjectRoom(name) => {
                                        controller.borrow_mut().rename_object_room(name);
                                    }
                                    SketchEvent::AddObject {
                                        kind,
                                        count,
                                        note,
                                        photos,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .add_object(kind, count, note, photos);
                                    }
                                    SketchEvent::DeleteObject { room, object } => {
                                        controller.borrow_mut().delete_object(room, object);
                                    }
                                    SketchEvent::DeleteObjectRoom(room) => {
                                        controller.borrow_mut().delete_object_room(room);
                                    }
                                    SketchEvent::OpenObjectReview => {
                                        controller.borrow_mut().set_object_review_mode(true);
                                    }
                                    SketchEvent::CloseObjectReview => {
                                        controller.borrow_mut().set_object_review_mode(false);
                                    }
                                    SketchEvent::SelectObjectReviewKind(idx) => {
                                        controller.borrow_mut().select_object_review(idx);
                                    }
                                    SketchEvent::ConfirmObjectAbsent(cat) => {
                                        controller.borrow_mut().confirm_object_absent(cat);
                                    }
                                    SketchEvent::SkipObjectAbsent(cat) => {
                                        controller.borrow_mut().skip_object_absent(cat);
                                    }
                                    SketchEvent::ConfirmObjectReview => {
                                        controller.borrow_mut().confirm_objects_review();
                                    }
                                    SketchEvent::OpenAtticRoom => {
                                        attic_open_request.set(true);
                                    }
                                    SketchEvent::ConfirmAtticAbsent => {
                                        controller.borrow_mut().confirm_attic_absent();
                                    }
                                    SketchEvent::SkipAtticAbsent => {
                                        controller.borrow_mut().skip_attic_absent();
                                    }
                                    SketchEvent::SetFinalFloor(floor) => {
                                        controller.borrow_mut().set_final_floor(floor);
                                    }
                                    SketchEvent::SetFinalWallFinish(field, finish) => {
                                        controller
                                            .borrow_mut()
                                            .set_final_wall_finish(field, finish);
                                    }
                                    SketchEvent::SetFinalHeating(heating) => {
                                        controller.borrow_mut().set_final_heating(heating);
                                    }
                                    SketchEvent::NorthImageGesture {
                                        pan_dx,
                                        pan_dy,
                                        zoom_factor,
                                        rotate_deg,
                                    } => {
                                        controller.borrow_mut().apply_north_gesture(
                                            pan_dx,
                                            pan_dy,
                                            zoom_factor,
                                            rotate_deg,
                                        );
                                    }
                                    SketchEvent::ResetNorthImage => {
                                        controller.borrow_mut().reset_north_image();
                                    }
                                    SketchEvent::CycleShelter(side) => {
                                        controller.borrow_mut().cycle_shelter(side);
                                    }
                                    SketchEvent::SetFinalMode(mode) => {
                                        controller.borrow_mut().set_final_mode(mode);
                                    }
                                    SketchEvent::ConfirmAlign => {
                                        controller.borrow_mut().confirm_align();
                                    }
                                    SketchEvent::LoadTestSatellite => {
                                        controller.borrow_mut().load_test_satellite_image();
                                    }
                                    SketchEvent::ConfirmFinalReview => {
                                        controller.borrow_mut().confirm_final_review();
                                    }
                                    SketchEvent::DismissBerComplete => {
                                        controller.borrow_mut().dismiss_ber_complete();
                                    }
                                    SketchEvent::OpenSummary => {
                                        controller.borrow_mut().open_summary();
                                    }
                                    SketchEvent::CloseSummary => {
                                        controller.borrow_mut().close_summary();
                                    }
                                    SketchEvent::SetDefaultCeilingHeight(value) => {
                                        match controller
                                            .borrow_mut()
                                            .set_default_ceiling_height(value)
                                        {
                                            Ok(()) => {
                                                // Clear-on-success: the button's label now
                                                // shows the stored value, so the entry box
                                                // resets to empty for the next entry (via
                                                // the one-shot flag — see its doc) and the
                                                // floating box closes. A rejected value
                                                // keeps the box open (it shows the error)
                                                // for correction.
                                                self.sketch_state.ceiling_height_error = None;
                                                self.sketch_state.ceiling_height_input_clear = true;
                                                self.sketch_state.ceiling_entry_open = false;
                                            }
                                            Err(e) => {
                                                log::error!("set_default_ceiling_height: {}", e);
                                                self.sketch_state.ceiling_height_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::OpenCeilingHeightEntry => {
                                        self.sketch_state.ceiling_entry_open = true;
                                    }
                                    SketchEvent::CloseCeilingHeightEntry => {
                                        self.sketch_state.ceiling_entry_open = false;
                                    }
                                    SketchEvent::SetCurrentStorey(target) => {
                                        log_err(
                                            "set_current_storey",
                                            controller.borrow_mut().set_current_storey(target),
                                        );
                                    }
                                    SketchEvent::AddStorey => {
                                        log_err("add_storey", controller.borrow_mut().add_storey());
                                    }
                                    // (The tab row disarms its confirm before
                                    // emitting — no page state to reset here.)
                                    SketchEvent::DeleteStorey => {
                                        log_err(
                                            "delete_current_storey",
                                            controller.borrow_mut().delete_current_storey(),
                                        );
                                    }
                                    SketchEvent::AssignFloorThickness(v) => {
                                        match controller.borrow_mut().assign_floor_thickness(v) {
                                            Ok(()) => {
                                                self.sketch_state.new_storey_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("assign_floor_thickness: {}", e);
                                                self.sketch_state.new_storey_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SetFloorThickness(v) => {
                                        match controller.borrow_mut().set_floor_thickness(v) {
                                            Ok(()) => {
                                                // Clear-on-success mirrors the ceiling-height
                                                // field: the label now shows the stored value,
                                                // so the entry box resets via the one-shot
                                                // flag; a rejected value stays for correction.
                                                self.sketch_state.floor_thickness_error = None;
                                                self.sketch_state.floor_thickness_input_clear =
                                                    true;
                                            }
                                            Err(e) => {
                                                log::error!("set_floor_thickness: {}", e);
                                                self.sketch_state.floor_thickness_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::ChooseDrawOutline => {
                                        controller.borrow_mut().choose_draw_outline();
                                        self.sketch_state.new_storey_error = None;
                                    }
                                    SketchEvent::ChooseSuggestOutlines => {
                                        match controller.borrow_mut().choose_suggest_outlines() {
                                            Ok(()) => {
                                                self.sketch_state.new_storey_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("choose_suggest_outlines: {}", e);
                                                self.sketch_state.new_storey_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::NextSuggestion => {
                                        controller.borrow_mut().cycle_suggestion(true);
                                    }
                                    SketchEvent::PrevSuggestion => {
                                        controller.borrow_mut().cycle_suggestion(false);
                                    }
                                    SketchEvent::AcceptSuggestion => {
                                        match controller.borrow_mut().accept_suggestion() {
                                            Ok(()) => {
                                                self.sketch_state.new_storey_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("accept_suggestion: {}", e);
                                                self.sketch_state.new_storey_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::SetAssessmentPhase(phase) => {
                                        controller.borrow_mut().set_current_phase(phase);
                                    }
                                    SketchEvent::SetStructureKind(kind) => {
                                        controller.borrow_mut().toggle_structure_kind(kind);
                                    }
                                    SketchEvent::SetStructureSelectMode(mode) => {
                                        controller.borrow_mut().set_structure_select_mode(mode);
                                    }
                                    SketchEvent::StructureSurfaceTap {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .toggle_structure_surface_at(world_x, world_y, tol);
                                    }
                                    SketchEvent::StructureSurfaceTapAllStoreys {
                                        storey_idx,
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller.borrow_mut().toggle_structure_surface_at_in(
                                            storey_idx, world_x, world_y, tol,
                                        );
                                    }
                                    SketchEvent::DeselectAllStructure => {
                                        controller.borrow_mut().clear_structure_surface_selection();
                                    }
                                    SketchEvent::StructureCarveStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_structure_carve_stroke(&points);
                                    }
                                    SketchEvent::StructureLassoStrokeWorld { per_storey } => {
                                        controller
                                            .borrow_mut()
                                            .apply_structure_lasso_stroke(&per_storey);
                                    }
                                    SketchEvent::ChooseStructureCutRegion { world_x, world_y } => {
                                        controller.borrow_mut().choose_structure_cut_region(
                                            world_x as f32,
                                            world_y as f32,
                                        );
                                    }
                                    SketchEvent::ResolveWallInclusion { include } => {
                                        controller.borrow_mut().resolve_wall_inclusion(include);
                                    }
                                    SketchEvent::BeginStructureEdit => {
                                        controller.borrow_mut().begin_structure_edit();
                                    }
                                    SketchEvent::SetActiveStructureGroup(i) => {
                                        controller.borrow_mut().set_active_structure_group(i);
                                    }
                                    SketchEvent::SetStructureGroupAttrs {
                                        group,
                                        ty,
                                        makeup,
                                        phase,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_attrs(group, ty, makeup, phase);
                                    }
                                    SketchEvent::BeginNewStructureGroup { group } => {
                                        controller.borrow_mut().begin_new_structure_group(group);
                                    }
                                    SketchEvent::ResetStructureGroup { group } => {
                                        controller.borrow_mut().reset_structure_group(group);
                                    }
                                    SketchEvent::SetStructureGroupName { group, name } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_name(group, name);
                                    }
                                    SketchEvent::SetStructureGroupExposure {
                                        group_index,
                                        exposure,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_exposure(group_index, exposure);
                                    }
                                    SketchEvent::BeginSpecifyLayers { group } => {
                                        controller.borrow_mut().begin_specify_layers(group);
                                    }
                                    SketchEvent::EndSpecifyLayers => {
                                        controller.borrow_mut().end_specify_layers();
                                    }
                                    SketchEvent::SetStructureGroupLayers { group, layers } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_layers(group, layers);
                                    }
                                    SketchEvent::SetStructureGroupInsulation {
                                        group,
                                        insulation,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .set_structure_group_insulation(group, insulation);
                                    }
                                    SketchEvent::BeginViewPhotos { group } => {
                                        controller.borrow_mut().begin_view_photos(group);
                                    }
                                    SketchEvent::EndViewPhotos => {
                                        controller.borrow_mut().end_view_photos();
                                    }
                                    SketchEvent::AddGroupPhoto {
                                        group,
                                        path,
                                        quality,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .add_group_photo(group, &path, quality);
                                    }
                                    SketchEvent::EndStructureEdit => {
                                        controller.borrow_mut().end_structure_edit();
                                    }
                                    SketchEvent::SetFloorExposureMode(on) => {
                                        controller.borrow_mut().set_floor_exposure_mode(on);
                                    }
                                    SketchEvent::ConfirmAllStoreysAdded => {
                                        controller.borrow_mut().confirm_all_storeys_added();
                                    }
                                    SketchEvent::DeferStoreyCount => {
                                        controller.borrow_mut().defer_storey_count();
                                    }
                                    SketchEvent::SetFloorExposureSelectMode(mode) => {
                                        controller
                                            .borrow_mut()
                                            .set_floor_exposure_select_mode(mode);
                                    }
                                    SketchEvent::FloorExposureTap {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .toggle_floor_exposure_at(world_x, world_y, tol);
                                    }
                                    SketchEvent::FloorExposureCarveStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_floor_exposure_carve_stroke(&points);
                                    }
                                    SketchEvent::ChooseFloorExposureCutRegion {
                                        world_x,
                                        world_y,
                                    } => {
                                        controller.borrow_mut().choose_floor_exposure_cut_region(
                                            world_x as f32,
                                            world_y as f32,
                                        );
                                    }
                                    SketchEvent::DeselectAllFloorExposure => {
                                        controller.borrow_mut().clear_floor_exposure_selection();
                                    }
                                    SketchEvent::AssignFloorExposure(exposure) => {
                                        controller.borrow_mut().set_floor_exposure_for(exposure);
                                    }
                                    SketchEvent::SetCeilingExposureMode(on) => {
                                        controller.borrow_mut().set_ceiling_exposure_mode(on);
                                    }
                                    SketchEvent::ConfirmAllStoreysAddedCeiling => {
                                        controller.borrow_mut().confirm_all_storeys_added_ceiling();
                                    }
                                    SketchEvent::DeferStoreyCountCeiling => {
                                        controller.borrow_mut().defer_storey_count_ceiling();
                                    }
                                    SketchEvent::SetRoofMode(on) => {
                                        controller.borrow_mut().set_roof_mode(on);
                                    }
                                    SketchEvent::ConfirmAllStoreysAddedRoof => {
                                        controller.borrow_mut().confirm_all_storeys_added_roof();
                                    }
                                    SketchEvent::DeferStoreyCountRoof => {
                                        controller.borrow_mut().defer_storey_count_roof();
                                    }
                                    SketchEvent::RoofRegionTap {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .roof_region_tap(world_x, world_y, tol);
                                    }
                                    SketchEvent::PickRoofShape(shape) => {
                                        controller.borrow_mut().pick_roof_area_shape(shape);
                                    }
                                    SketchEvent::RoofAreaRidgeStroke { points } => {
                                        let r =
                                            controller.borrow_mut().apply_roof_area_ridge(&points);
                                        if let Err(e) = r {
                                            self.sketch_state.roof_error = Some(e);
                                        } else {
                                            self.sketch_state.roof_error = None;
                                        }
                                    }
                                    SketchEvent::FinishRoofRidges => {
                                        let r = controller.borrow_mut().finish_roof_area_ridges();
                                        match r {
                                            Ok(()) => self.sketch_state.roof_error = None,
                                            Err(e) => self.sketch_state.roof_error = Some(e),
                                        }
                                    }
                                    SketchEvent::CentreRoofRidge => {
                                        controller.borrow_mut().centre_pending_roof_ridge();
                                    }
                                    SketchEvent::AssignRoofPitch(deg) => {
                                        let r = controller.borrow_mut().assign_roof_area_pitch(deg);
                                        match r {
                                            Ok(()) => self.sketch_state.roof_error = None,
                                            Err(e) => self.sketch_state.roof_error = Some(e),
                                        }
                                    }
                                    SketchEvent::CancelRoofArea => {
                                        controller.borrow_mut().cancel_roof_area();
                                        self.sketch_state.roof_error = None;
                                    }
                                    SketchEvent::SetCeilingExposureSelectMode(mode) => {
                                        controller
                                            .borrow_mut()
                                            .set_ceiling_exposure_select_mode(mode);
                                    }
                                    SketchEvent::CeilingExposureTap {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .toggle_ceiling_exposure_at(world_x, world_y, tol);
                                    }
                                    SketchEvent::CeilingExposureCarveStrokeWorld { points } => {
                                        controller
                                            .borrow_mut()
                                            .apply_ceiling_exposure_carve_stroke(&points);
                                    }
                                    SketchEvent::ChooseCeilingExposureCutRegion {
                                        world_x,
                                        world_y,
                                    } => {
                                        controller.borrow_mut().choose_ceiling_exposure_cut_region(
                                            world_x as f32,
                                            world_y as f32,
                                        );
                                    }
                                    SketchEvent::DeselectAllCeilingExposure => {
                                        controller.borrow_mut().clear_ceiling_exposure_selection();
                                    }
                                    SketchEvent::AssignCeilingExposure(exposure) => {
                                        controller.borrow_mut().set_ceiling_exposure_for(exposure);
                                    }
                                    SketchEvent::AddExternalPhoto {
                                        side,
                                        path,
                                        quality,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .add_external_photo(side, &path, quality);
                                    }
                                    SketchEvent::RemoveExternalPhoto { side } => {
                                        controller.borrow_mut().remove_external_photo(side);
                                    }
                                    SketchEvent::ToggleDeferExternalPhoto { side } => {
                                        controller.borrow_mut().toggle_defer_external_photo(side);
                                    }
                                    SketchEvent::DismissDeferredPhotosPrompt => {
                                        controller.borrow_mut().dismiss_deferred_photos_prompt();
                                    }
                                    SketchEvent::EnsureExternalPhotoThumbnail { side } => {
                                        controller
                                            .borrow_mut()
                                            .ensure_external_photo_thumbnail(side);
                                    }
                                    SketchEvent::RotateExternalPhoto { side, clockwise } => {
                                        controller
                                            .borrow_mut()
                                            .rotate_external_photo(side, clockwise);
                                    }
                                    // UI-local: opening the annotator needs the
                                    // page-state slot the in-render view already
                                    // borrows, so it's deferred to just after
                                    // render (see `annotate_request` below).
                                    SketchEvent::BeginAnnotateExternalPhoto { side } => {
                                        annotate_request.set(Some(side));
                                    }
                                    SketchEvent::AnnotateExternalPhoto { side, jpeg } => {
                                        controller
                                            .borrow_mut()
                                            .annotate_external_photo(side, &jpeg);
                                    }
                                    SketchEvent::EditMouseDown {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .edit_mouse_down(world_x, world_y, tol);
                                    }
                                    SketchEvent::EditMouseDrag { world_x, world_y } => {
                                        controller.borrow_mut().edit_mouse_drag(world_x, world_y);
                                    }
                                    SketchEvent::EditMouseUp => {
                                        log_err(
                                            "edit_mouse_up",
                                            controller.borrow_mut().edit_mouse_up(),
                                        );
                                    }
                                    SketchEvent::ApplyEditDrag(value) => {
                                        match controller.borrow_mut().apply_edit_drag(value) {
                                            Ok(()) => {
                                                self.sketch_state.edit_drag_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("apply_edit_drag: {}", e);
                                                self.sketch_state.edit_drag_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::CycleEditDragMeasurement => {
                                        controller.borrow_mut().cycle_edit_drag_measurement();
                                        self.sketch_state.edit_drag_error = None;
                                    }
                                    SketchEvent::CancelEditDrag => {
                                        log_err(
                                            "cancel_edit_drag",
                                            controller.borrow_mut().cancel_edit_drag(),
                                        );
                                        self.sketch_state.edit_drag_error = None;
                                    }
                                    SketchEvent::EditDoubleClick {
                                        world_x,
                                        world_y,
                                        tol,
                                    } => {
                                        controller
                                            .borrow_mut()
                                            .edit_double_click(world_x, world_y, tol);
                                    }
                                    SketchEvent::ApplyVertexMeasurement { idx, value } => {
                                        match controller
                                            .borrow_mut()
                                            .apply_vertex_measurement(idx, value)
                                        {
                                            Ok(()) => {
                                                self.sketch_state.vertex_drop_error = None;
                                            }
                                            Err(e) => {
                                                log::error!("apply_vertex_measurement: {}", e);
                                                self.sketch_state.vertex_drop_error = Some(e);
                                            }
                                        }
                                    }
                                    SketchEvent::Undo => {
                                        log_err("undo", controller.borrow_mut().undo());
                                    }
                                    SketchEvent::ExtendStrokeWorld { points } => {
                                        controller.borrow_mut().apply_extend_stroke_event(&points);
                                    }
                                    SketchEvent::OutlineStrokeWorld { points } => {
                                        controller.borrow_mut().apply_outline_stroke_world(&points);
                                    }
                                    SketchEvent::TrimStrokeWorld { points } => {
                                        controller.borrow_mut().apply_trim_stroke_event(&points);
                                    }
                                    SketchEvent::Redo => {
                                        log_err("redo", controller.borrow_mut().redo());
                                    }
                                    SketchEvent::SaveCheckpoint => {
                                        log_err(
                                            "save_checkpoint",
                                            controller.borrow_mut().save_checkpoint(),
                                        );
                                    }
                                    SketchEvent::LoadCheckpoint => {
                                        // DEV tool: replaces the sketch session with
                                        // the checkpoint (same reconstruction as a
                                        // BER_CHECKPOINT=1 launch). Key Details isn't
                                        // refreshed here — the closure can't re-borrow
                                        // self — acceptable for a dev-only path.
                                        if !controller.borrow_mut().load_checkpoint() {
                                            log::warn!("load_checkpoint: no checkpoint file found");
                                        }
                                    }
                                    SketchEvent::DebugDump => {
                                        // Stdout for the terminal user, clipboard
                                        // for pasting straight into a report.
                                        let dump = controller.borrow_mut().debug_dump_current();
                                        log::info!("{dump}");
                                        ctx.copy_text(dump);
                                    }
                                    SketchEvent::CancelVertexDrop => {
                                        // Note: the toolbar already cleared `inputs`
                                        // when the Cancel button was clicked; touching
                                        // `self.sketch_state.vertex_drop_inputs` here
                                        // would double-borrow it (the toolbar still
                                        // owns the &mut reference during the closure).
                                        log_err(
                                            "cancel_vertex_drop",
                                            controller.borrow_mut().cancel_vertex_drop(),
                                        );
                                        self.sketch_state.vertex_drop_error = None;
                                    }
                                },
                                // "Save & exit BER" in the Final-phase summary
                                // modal is rendered inside the sketch page, so its
                                // ExitBer arrives here (not through the chrome
                                // closure). Route it to the same exit flag.
                                UiEvent::ExitBer => exit_clicked.set(true),
                                _ => {}
                            });
                            // Deferred object-room dismissal (see
                            // `object_dismiss_request` above): commit any
                            // non-empty in-flight entry + parked photos like
                            // OK, then close the room. Opening the attic
                            // flushes the previous room the same way first.
                            let open_attic = attic_open_request.take();
                            if object_dismiss_request.take() || open_attic {
                                self.flush_and_close_object_room();
                            }
                            if open_attic {
                                self.app_controller.borrow_mut().open_attic_room();
                            }
                            // Deferred annotator open (see `annotate_request`
                            // above): decode the side's stored full-res photo
                            // into a fresh session. A missing/undecodable file
                            // simply doesn't open the takeover.
                            if let Some(side) = annotate_request.take() {
                                let path = self
                                    .app_controller
                                    .borrow()
                                    .external_photos()
                                    .into_iter()
                                    .find(|(s, _)| *s == side)
                                    .and_then(|(_, p)| p)
                                    .map(|p| p.file_path);
                                self.sketch_state.photo_annotator = path.and_then(|p| {
                                    crate::ui::widgets::PhotoAnnotator::open(side, &p)
                                });
                            }
                            // Deferred dimension wall-length copy (see
                            // `dim_copy_request` above): insert the tapped
                            // wall's displayed length into the dimension
                            // expression as the term at the caret. The caret
                            // is read from the dimension field's TextEdit
                            // state when it's focused (native-keyboard
                            // editing mid-expression); the keypad/laser
                            // flows are caret-less, so `None` targets the
                            // last term — the laser's re-shoot semantics.
                            if let Some(metres) = dim_copy_request.take() {
                                let focused = ctx.memory(|m| m.focused());
                                let cursor = focused.and_then(|id| {
                                    egui::widgets::text_edit::TextEditState::load(ctx, id)
                                        .and_then(|s| s.cursor.char_range())
                                        .map(|r| r.primary.index.min(r.secondary.index))
                                });
                                let caret = crate::number_entry::insert_copied_length(
                                    &mut self.sketch_state.dimension_input,
                                    cursor,
                                    metres,
                                );
                                // Keep the visible caret just after the
                                // inserted text (only when a caret was read —
                                // i.e. the field is focused and has state).
                                if let (Some(id), Some(_)) = (focused, cursor) {
                                    let mut st =
                                        egui::widgets::text_edit::TextEditState::load(ctx, id)
                                            .unwrap_or_default();
                                    st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                                        egui::text::CCursor::new(caret),
                                    )));
                                    st.store(ctx, id);
                                }
                            }
                        }
                    },
                );
                if exit_clicked.get() {
                    // Flush + clear the loaded record, then return Home with
                    // all per-record page state reset (autosave means nothing
                    // is lost; no confirm dialog).
                    log_err("exit_ber", self.app_controller.borrow_mut().exit_ber());
                    self.key_details_state = KeyDetailsState::default();
                    self.home_state = HomeState::default();
                    self.sketch_state = SketchState::default();
                    self.navigate_to(Page::Home);
                } else if let Some(p) = next_page {
                    // Leaving the current section drops the active sketch tool —
                    // CAD-style: returning lands in plain navigation, not mid-tool.
                    self.app_controller.borrow_mut().enter_navigation_mode();
                    self.navigate_to(p);
                }
            }
        }

        // Global bug-report overlay: a floating button + recording modal drawn
        // over whatever page is active (so the assessor can flag an issue at any
        // stage of a BER). Also drains the "WiFi came up" signal to poke uploads.
        self.bug_overlay(ctx);
    }
}
