use strum_macros::EnumIter;

#[derive(Default, Clone, Copy, PartialEq, EnumIter, Debug)]
pub enum SketchMode {
    #[default]
    Storey2D,
    Storey3D,
    FullHouse,
}

#[derive(Default, Clone, Copy, PartialEq, EnumIter, Debug)]
pub enum DisplayMode {
    #[default]
    FloorMapDims,
    Roofs,
    Walls,
    Floors,
    AllSurfaces,
}

#[derive(Default, Clone, Copy, PartialEq, EnumIter, Debug)]
pub enum EditTools {
    /// Draw the primary orthogonal outline of the storey. Each stroke replaces
    /// any prior outline. Always available (the prerequisite for everything else).
    #[default]
    DrawPerpWalls,
    /// Draw an additional disjoint polygon on the same storey (e.g. an
    /// extension on an upper floor that's separate from another section).
    /// Each stroke appends a new ring rather than replacing.
    AddDetachedSection,
    /// Select-and-edit mode. Primary click on a wall selects it; primary drag
    /// translates it (parameter update). Double-click on a wall inserts a new
    /// vertex at the click point that the user can drag freely. Replaces the
    /// older Select + Move tools.
    Edit,
    /// Extend a wall with an orthogonal (stepped) path that bulges it out,
    /// adding area. The drawn path is orthogonalized and spliced onto the
    /// shorter arc.
    ExtendOrtho,
    /// Extend a wall with a freeform CURVE that bulges out, adding area. The
    /// drawn arc is faceted into a polyline (not dimensioned individually;
    /// scales with the plan).
    ExtendCurve,
    /// Trim with a straight cut line: the line crosses the outline, the smaller
    /// side is discarded, and the cut is a single (possibly diagonal) edge.
    Trim,
    /// Trim with an orthogonal cut path (removing area): the drawn path is
    /// orthogonalized, its endpoints attach to the outline, and the smaller
    /// side is replaced by the stepped cut — a notch or orthogonal chamfer.
    TrimOrtho,
    /// Trim with a freeform CURVE that carves a curved recess (removing area) —
    /// e.g. a wall recessed around a tower. The drawn arc is faceted; the curve
    /// isn't individually dimensioned and scales with the plan.
    TrimCurve,
}
