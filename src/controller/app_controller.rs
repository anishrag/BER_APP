use super::dataloader::DataLoader;
use std::sync::mpsc::Sender;

pub struct AppController {
    loader: DataLoader,
}

impl AppController {
    pub fn new() -> Self {
        Self {
            loader: DataLoader::new(),
        }
    }

    pub fn load_data_async(&self, tx: Sender<(String, f32)>) {
        let loader = self.loader.clone();

        std::thread::spawn(move || {
            let _ = loader.load_general_data(tx);
        });
    }
}
