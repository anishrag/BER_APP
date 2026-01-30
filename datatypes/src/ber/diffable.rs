use super::common::ItemId;
use super::fenestration::{Door, Window};

pub trait AddRemoveTrait {
    fn id(&self) -> ItemId;
    fn clone_box(&self) -> Box<dyn AddRemoveTrait>;
}

impl AddRemoveTrait for PlanarElement {
    fn id(&self) -> ItemId {
        self.common().id
    }
    fn clone_box(&self) -> Box<dyn AddRemoveTrait> {
        Box::new(self.clone())
    }
}

impl AddRemoveTrait for Window {
    fn id(&self) -> ItemId {
        self.id
    }
    fn clone_box(&self) -> Box<dyn AddRemoveTrait> {
        Box::new(self.clone())
    }
}

impl AddRemoveTrait for Door {
    fn id(&self) -> ItemId {
        self.id
    }
    fn clone_box(&self) -> Box<dyn AddRemoveTrait> {
        Box::new(self.clone())
    }
}
