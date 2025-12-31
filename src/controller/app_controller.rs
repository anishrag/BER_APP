use chrono::NaiveDate;
use rusqlite::Connection;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository};
use crate::engine::dataloader::DataLoader;
use datatypes::ber::ber::Ber;

pub struct AppController {
    conn: Connection,

    loader_engine: DataLoader,
    //   processing_engine: ProcessingEngine,
}

impl AppController {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn,
            loader_engine: DataLoader::new(),
            //            processing_engine: ProcessingEngine::new(),
        }
    }

    pub fn load_data_async(&self, tx: Sender<(String, f32)>) {
        let loader = self.loader_engine.clone();

        std::thread::spawn(move || {
            let _ = loader.load_general_data(tx);
        });
    }

    pub fn create_ber(&self, name: String, date_assessed: NaiveDate) -> Ber {
        let ber = Ber::new();
        let record = BerRecord {
            id: ber.id,
            name,
            date_assessed,
            data: ber.clone(),
        };

        let repo = BerRepository::new(&self.conn);
        repo.upsert(&record).unwrap();
        ber
    }

    pub fn get_ber(&self, id: &uuid::Uuid) -> Option<Ber> {
        let repo = BerRepository::new(&self.conn);
        repo.load(id).ok().map(|record| record.data)
    }

    //     pub fn create_evidence_doc(&self, tx: Sender<(String, f32)>) {
    //         let processing_engine = self.processing_engine.clone();
    //         std::thread::spawn(move || {
    //             let _ = processing_engine.generate_evidence_document(tx);
    //         });
    //     }
}
