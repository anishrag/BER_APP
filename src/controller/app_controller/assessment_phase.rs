//! The assessor's current workflow phase (Mapping → Structure → Objects). This
//! is app-shell state that spans steps, so it lives on the controller (not the
//! per-storey `Document`). The current phase drives which toolset + checklist +
//! canvas overlays the Sketch page shows. Switching ONTO Structure is gated on
//! Mapping being complete (every Stage A checklist item green); going back to
//! Mapping is always allowed. Objects is not built yet.
use super::*;

/// The canvas display mode that shades a given surface kind — Structure-phase
/// Walls/Roofs/Floors highlighting. (`SurfaceKind` itself is the data-model
/// taxonomy in `datatypes`; this canvas mapping is app-shell.)
fn display_mode_for(kind: SurfaceKind) -> DisplayMode {
    match kind {
        SurfaceKind::Walls => DisplayMode::Walls,
        SurfaceKind::Roofs => DisplayMode::Roofs,
        SurfaceKind::Floors => DisplayMode::Floors,
    }
}

/// Which Select subtool is active for the selected surface kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    /// Pick surfaces one at a time by tapping them.
    Click,
    /// Draw a carve (the living-area cut): the surfaces inside the resolved
    /// region get selected.
    Carve,
    /// Every surface of the kind.
    All,
    /// The surfaces still left to set (criteria wired later — fills like `All`).
    Remaining,
}

/// The active Structure-phase selection: a surface kind + which Select subtool
/// is on. Drives the canvas shading and is the target the (placeholder) Set
/// subtools will act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructureSelection {
    pub kind: SurfaceKind,
    pub mode: SelectMode,
}

/// Which assessment phase the workspace is currently showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AssessmentPhase {
    /// Phase 1 — geometry + inferred construction (the drawing tools).
    #[default]
    Mapping,
    /// Phase 2 — identify/inspect the wall / roof / floor surfaces.
    Structure,
    /// Phase 3 — discrete plant / appliance counts (not built yet).
    Objects,
}

impl AppController {
    /// The phase the workspace is currently in (default [`AssessmentPhase::Mapping`]).
    pub fn current_phase(&self) -> AssessmentPhase {
        self.current_phase
    }

    /// Switch the workspace onto `phase`. Resets the canvas display mode to the
    /// phase's default so a Structure-phase surface highlight doesn't bleed back
    /// into Mapping (and vice-versa): Mapping shows the dimensioned floor map,
    /// Structure shows every surface until a Walls/Roofs/Floors tool is picked.
    /// The caller (UI dispatch) is responsible for only offering reachable
    /// phases — see [`mapping_complete`](Self::mapping_complete).
    pub fn set_current_phase(&mut self, phase: AssessmentPhase) {
        // Leaving a phase deselects all of its interaction tools — a Mapping
        // mode (exposure, living area, …) must not stay live once the assessor
        // moves to Structure, and a Structure selection must not linger back in
        // Mapping. Clear first, then set the phase's default canvas display.
        self.exit_mapping_modes();
        self.structure_selection = None;
        self.structure_selected.clear();
        self.pending_structure_cut = None;
        self.structure_split_kind = None;
        self.current_phase = phase;
        let display = match phase {
            AssessmentPhase::Mapping => DisplayMode::FloorMapDims,
            AssessmentPhase::Structure | AssessmentPhase::Objects => DisplayMode::AllSurfaces,
        };
        self.set_display_mode(display);
    }

    /// Turn off every Mapping-phase interaction mode (each setter also clears
    /// its own transient pick/drag state). Used on a phase switch so no Mapping
    /// tool stays selected once the assessor leaves the drawing tools.
    fn exit_mapping_modes(&mut self) {
        self.set_openings_mode(false);
        self.set_living_area_mode(false);
        self.set_exposure_mode(false);
        self.set_phase_mode(false);
        self.set_step_mode(false);
        self.set_ceiling_step_mode(false);
        self.set_roof_point_mode(false);
    }

    /// The active Structure-phase surface selection, if any.
    pub fn structure_selection(&self) -> Option<StructureSelection> {
        self.structure_selection
    }

    /// Set (or clear) the Structure-phase surface selection from a Select
    /// subtool. Seeds the selected set: `Select all` / `Select remaining` fill
    /// it with every region of the kind, `Select` keeps the current set (so it
    /// refines by tapping); switching kind invalidates the old ids. `None`
    /// clears everything. The canvas shades the selected kind only.
    pub fn set_structure_selection(&mut self, sel: Option<StructureSelection>) {
        let prev_kind = self.structure_selection.map(|s| s.kind);
        self.structure_selection = sel;
        match sel {
            Some(s) => {
                if prev_kind != Some(s.kind) {
                    self.structure_selected.clear();
                }
                match s.mode {
                    // Placeholder: `Remaining` fills like `All` until its
                    // "not yet set" criteria are wired in.
                    SelectMode::All | SelectMode::Remaining => {
                        self.select_all_structure_surfaces(s.kind)
                    }
                    // Click/Carve keep the current set; taps / the carved
                    // region refine it.
                    SelectMode::Click | SelectMode::Carve => {}
                }
                self.set_display_mode(display_mode_for(s.kind));
            }
            None => {
                self.structure_selected.clear();
                self.set_display_mode(DisplayMode::AllSurfaces);
            }
        }
    }

    /// Surface counts for the current storey (walls / roofs / floors) — the
    /// Structure-phase tool read-out. Zeroed when there's no current storey
    /// (the read-only "All storeys" tab) or no BER is loaded.
    pub fn current_surface_counts(&self) -> SurfaceCounts {
        let Some(record) = self.current_record.as_ref() else {
            return SurfaceCounts::default();
        };
        self.current_storey_idx
            .and_then(|i| record.data.storeys.get(i))
            .map(Storey::surface_counts)
            .unwrap_or_default()
    }
}
