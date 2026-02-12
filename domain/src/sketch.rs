use strum::EnumIter;

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
}

#[derive(Default, Clone, Copy, PartialEq, EnumIter, Debug)]
pub enum EditTools {
    #[default]
    DrawPerpWalls,
    Select,
    Move,
    Extend,
    Trim,
}
