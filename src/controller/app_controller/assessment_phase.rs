//! The assessor's current workflow phase (Mapping → Structure → Objects). This
//! is app-shell state that spans steps, so it lives on the controller (not the
//! per-storey `Document`). The current phase drives which toolset + checklist +
//! canvas overlays the Sketch page shows. Switching ONTO Structure is gated on
//! Mapping being complete (every Stage A checklist item green); going back to
//! Mapping is always allowed. Objects is not built yet.
use super::*;

/// The canvas display mode that shades a given surface kind — Structure-phase
/// Walls/Roofs/Floors highlighting. (`SurfaceKind` itself is the data-model
/// taxonomy in `ber_model`; this canvas mapping is app-shell.)
fn display_mode_for(kind: SurfaceKind) -> DisplayMode {
    match kind {
        SurfaceKind::Walls => DisplayMode::Walls,
        SurfaceKind::Roofs => DisplayMode::Roofs,
        SurfaceKind::Floors => DisplayMode::Floors,
    }
}

/// The canvas display mode for a SET of active modes: a single kind shades just
/// that kind; several active kinds fall back to `AllSurfaces` (there's no
/// per-pair display mode, and shading the extra kind is a minor cosmetic cost).
/// An empty set also reads as `AllSurfaces` (nothing kind-specific to emphasise).
fn display_mode_for_kinds(kinds: &std::collections::BTreeSet<SurfaceKind>) -> DisplayMode {
    match kinds.iter().next() {
        Some(only) if kinds.len() == 1 => display_mode_for(*only),
        _ => DisplayMode::AllSurfaces,
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
    /// The surfaces still left to do — every surface of the kind whose
    /// construction isn't complete yet (type or build-up still unset).
    Remaining,
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
        // Reset the Structure tools: default to Wall mode with no Select subtool
        // active and an empty selection ("first we're in wall mode").
        self.structure_kinds = std::collections::BTreeSet::from([SurfaceKind::Walls]);
        self.structure_select = None;
        self.structure_selected.clear();
        self.pending_structure_carves.clear();
        self.pending_structure_cut = None;
        self.structure_split_kind = None;
        self.close_structure_edit();
        self.current_phase = phase;
        let display = match phase {
            AssessmentPhase::Mapping => DisplayMode::FloorMapDims,
            AssessmentPhase::Structure => display_mode_for_kinds(&self.structure_kinds),
            AssessmentPhase::Objects => DisplayMode::AllSurfaces,
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

    /// The active Structure-phase MODES — the surface kinds the global Select
    /// tools act on (any of Walls / Roofs / Floors; non-exclusive).
    pub fn structure_kinds(&self) -> &std::collections::BTreeSet<SurfaceKind> {
        &self.structure_kinds
    }

    /// The active global Select subtool, if any.
    pub fn structure_select(&self) -> Option<SelectMode> {
        self.structure_select
    }

    /// TOGGLE a Structure-phase MODE in/out of the active set (the mode buttons
    /// are non-exclusive). Turning a kind ON adds its surfaces when a seed tool
    /// (`All`/`Remaining`) is active; turning one OFF drops that kind's ids from
    /// the selection. Either way the open "Edit attributes" session closes (its
    /// groups belong to the old kind set) and the canvas re-shades.
    pub fn toggle_structure_kind(&mut self, kind: SurfaceKind) {
        let turning_off = self.structure_kinds.contains(&kind);
        if turning_off {
            // Keep at least one mode active — toggling the last one off is a
            // no-op (an empty mode set selects nothing and reads as odd).
            if self.structure_kinds.len() == 1 {
                return;
            }
            self.structure_kinds.remove(&kind);
        } else {
            self.structure_kinds.insert(kind);
        }
        self.pending_structure_cut = None;
        self.structure_split_kind = None;
        self.close_structure_edit();
        match self.structure_select {
            // A seed tool re-seeds across the whole new active set.
            Some(SelectMode::All | SelectMode::Remaining) => self.reseed_structure_selection(),
            // Otherwise just drop the de-activated kind's ids (additive modes
            // keep the rest of the selection so the assessor doesn't lose work).
            _ if turning_off => self.drop_selection_of_kind(kind),
            _ => {}
        }
        self.set_display_mode(display_mode_for_kinds(&self.structure_kinds));
    }

    /// Set (or clear) the global Select subtool — it acts on every active
    /// [`structure_kinds`](Self::structure_kinds) mode. `All`/`Remaining` seed the
    /// selection across all active kinds; `Click`/`Carve` keep the current set
    /// (refined by taps / the carved region); `None` just deactivates the tool
    /// (the selection + modes stay — "Deselect all" is the explicit clear).
    pub fn set_structure_select_mode(&mut self, mode: Option<SelectMode>) {
        self.structure_select = mode;
        if matches!(mode, Some(SelectMode::All | SelectMode::Remaining)) {
            self.reseed_structure_selection();
        }
        self.set_display_mode(display_mode_for_kinds(&self.structure_kinds));
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
