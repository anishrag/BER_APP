use chrono::NaiveDate;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository};
use crate::engine::dataloader::DataLoader;
use crate::engine::Db;
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::ItemId;
use domain::points::Point2f32;
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_2d_plan::FloorPlan;
use sketch::model::Stroke;
use sketch::SketchEngine;

pub struct AppController {
    conn: Db,
    sketch_engine: SketchEngine,
    floor_plan: FloorPlan,
    loader_engine: DataLoader,
    //   processing_engine: ProcessingEngine,
}

impl AppController {
    pub fn new(conn: Db) -> Self {
        Self {
            conn,
            loader_engine: DataLoader::new(),
            floor_plan: FloorPlan::default(),
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

    pub fn set_display_mode(&mut self, mode: DisplayMode) {
        self.sketch_engine.set_display(mode);
    }
    pub fn display_mode(&self) -> DisplayMode {
        self.sketch_engine.display
    }

    pub fn set_edit_mode(&mut self, mode: EditTools) {
        self.sketch_engine.set_edit_tools(mode);
    }
    pub fn edit_mode(&self) -> EditTools {
        self.sketch_engine.edit_tool
    }

    pub fn sketch_start_stroke(&mut self, start: Point2f32) -> Result<(), String> {
        self.sketch_engine
            .start_stroke(start)
            .map_err(|e| format!("Failed to start stroke: {:?}", e))
    }

    pub fn sketch_add_point(&mut self, point: Point2f32) -> Result<(), String> {
        self.sketch_engine
            .add_point(point)
            .map_err(|e| format!("Failed to add point: {:?}", e))
    }

    pub fn sketch_end_stroke(&mut self, point: Point2f32) -> Result<(), String> {
        if let Some(stroke) = self
            .sketch_engine
            .end_stroke()
            .map_err(|e| format!("Failed to end stroke: {:?}", e))?
        {
            // deal with stroke -- depends on the engine mode
            match self.sketch_engine.mode_behaviour.as_ref() {
                // For example, if it's a Storey2DPerpWalls, we might want to convert the stroke into walls and add them to the floor plan
                Storey2DPerpWalls => {
                    self.floor_plan = FloorPlan::from_points_to_orthogonal(&stroke.points, 10.0)
                        .map_err(|e| format!("Failed to add area: {:?}", e))?;
                    println!("floor plan: {:?}", self.floor_plan);
                }
                _ => {}
            }
            println!("Stroke completed with {} points", stroke.points.len());
        }
        Ok(())
    }
}

/// Statistics about a floor plan
#[derive(Debug, Clone)]
pub struct FloorPlanStats {
    pub perimeter: f32,
    pub area: f32,
    pub num_vertices: usize,
}
