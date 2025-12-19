use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Polygon3D {
    pub exterior: Vec<Vertex3D>,
}

pub type Year = u32;
type LocalId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct YearId {
    pub year: Year,
    pub id: LocalId,
}

#[derive(Debug)]
pub struct YearIdAllocator {
    next_id: HashMap<Year, LocalId>,
}

impl YearIdAllocator {
    pub fn new() -> Self {
        Self {
            next_id: HashMap::new(),
        }
    }

    pub fn alloc(&mut self, year: Year) -> YearId {
        let id = self.next_id.entry(year).or_insert(0);
        let key = YearId { year, id: *id };
        *id += 1;
        key
    }

    /// Optional: peek without allocating
    pub fn next_for_year(&self, year: Year) -> LocalId {
        *self.next_id.get(&year).unwrap_or(&0)
    }
}

/// A 3D vertex (x, y, z)
#[derive(Clone, Debug, PartialEq)]
pub struct Vertex3D {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// A 2D vertex (x, y)
#[derive(Clone, Debug, PartialEq)]
pub struct Vertex2D {
    pub x: f64,
    pub y: f64,
}

/// Construction types (example: 5-6 options)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstructionType {
    Concrete,
    Timber,
    Steel,
    Brick,
    Mixed,
    Other,
}

/// Construction layers (optional multiple layers)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstructionLayer {
    Insulation,
    Reinforcement,
    Waterproofing,
    Finish,
    // add more as needed
}
