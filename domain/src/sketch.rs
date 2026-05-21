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
    AddSection,
    Select,
    Move,
    Extend,
    Trim,
}
