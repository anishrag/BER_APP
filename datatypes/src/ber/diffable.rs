use uuid::Uuid;

use super::fenestration::{Door, Window};
use super::structural::PlanarElement;

pub trait AddRemoveTrait {
    fn id(&self) -> Uuid;
    fn clone_box(&self) -> Box<dyn AddRemoveTrait>;
}

impl AddRemoveTrait for PlanarElement {
    fn id(&self) -> Uuid {
        self.common().id
    }
    fn clone_box(&self) -> Box<dyn AddRemoveTrait> {
        Box::new(self.clone())
    }
}

impl AddRemoveTrait for Window {
    fn id(&self) -> Uuid {
        self.id
    }
    fn clone_box(&self) -> Box<dyn AddRemoveTrait> {
        Box::new(self.clone())
    }
}

impl AddRemoveTrait for Door {
    fn id(&self) -> Uuid {
        self.id
    }
    fn clone_box(&self) -> Box<dyn AddRemoveTrait> {
        Box::new(self.clone())
    }
}
