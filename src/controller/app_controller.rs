use chrono::NaiveDate;
use rusqlite::Connection;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository};
use crate::engine::dataloader::DataLoader;
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::ItemId;
use domain::sketch::SketchMode;

use sketch::model::Stroke;
use sketch::SketchEngine;

pub struct AppController {
    conn: Connection,
    sketch_engine: SketchEngine,
    loader_engine: DataLoader,
    //   processing_engine: ProcessingEngine,
}

impl AppController {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn,
            loader_engine: DataLoader::new(),
            sketch_engine: SketchEngine::new(), //            processing_engine: ProcessingEngine::new(),
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

    pub fn get_ber(&self, id: &ItemId) -> Option<Ber> {
        let repo = BerRepository::new(&self.conn);
        repo.load(id).ok().map(|record| record.data)
    }

    //     pub fn create_evidence_doc(&self, tx: Sender<(String, f32)>) {
    //         let processing_engine = self.processing_engine.clone();
    //         std::thread::spawn(move || {
    //             let _ = processing_engine.generate_evidence_document(tx);
    //         });
    //     }

    /// SKETCHING STUFF--------------------------------------------
    ///
    pub fn sketch_stroke(&self) -> Option<Stroke> {
        self.sketch_engine.stroke.clone()
    }

    pub fn sketch_mode(&self) -> SketchMode {
        self.sketch_engine.mode
    }

    pub fn set_sketch_mode(&mut self, mode: SketchMode) {
        self.sketch_engine.set_mode(mode);
    }
}

/// Statistics about a floor plan
#[derive(Debug, Clone)]
pub struct FloorPlanStats {
    pub perimeter: f32,
    pub area: f32,
    pub num_vertices: usize,
}
