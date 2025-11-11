use ber_app::data::ber::{Ber, Photo};
use ber_app::data::fenestration::WindowTypeGroup;
use ber_app::data::structural::{ConstructionPhase, PlanarElement};
use ber_app::data::systems::{HeatingSystem, ShelteredSideInfo, VentilationSystem};
use rusqlite::Connection;
use uuid::Uuid;

#[test]
fn test_insert_and_load_ber() -> anyhow::Result<()> {
    // Use an in-memory SQLite database for testing
    let conn = Connection::open_in_memory()?;
    Ber::init_db(&conn)?;

    // Create a sample construction phase
    let phase = ConstructionPhase {
        id: Uuid::new_v4(),
        phase_name: "Original".to_string(),
        year_built: 2000,
        floors: vec![PlanarElement {
            id: Uuid::new_v4(),
            vertices: vec![
                (0.0, 0.0, 0.0),
                (10.0, 0.0, 0.0),
                (10.0, 8.0, 0.0),
                (0.0, 8.0, 0.0),
            ],
            element_type: "floor".to_string(),
            u_value: Some(0.25),
            description: Some("Ground floor slab".to_string()),
        }],
        walls: vec![],
        roofs: vec![],
    };

    // Create a sample BER with photos
    let ber = Ber {
        id: Uuid::new_v4(),
        address: "123 Test Street".to_string(),
        year_assessed: 2025,
        construction_phases: vec![phase],
        window_types: vec![],
        door_types: vec![],
        heating_system: HeatingSystem {
            id: Uuid::new_v4(),
            system_type: "Gas boiler".to_string(),
            efficiency_percent: 91.0,
            year_installed: 2018,
        },
        ventilation_system: VentilationSystem {
            id: Uuid::new_v4(),
            system_type: "MVHR".to_string(),
            air_change_rate: Some(0.5),
            year_installed: Some(2018),
        },
        sheltered_sides: ShelteredSideInfo {
            id: Uuid::new_v4(),
            sheltered_exposure: vec!["North".to_string(), "West".to_string()],
        },
        floor_area_m2: 80.0,
        number_of_storeys: 2,
        thermal_mass_category: "Medium".to_string(),
        photos: vec![
            Photo {
                id: Uuid::new_v4(),
                file_path: "assets/living_room.jpg".to_string(),
                description: Some("Living room wall".to_string()),
            },
            Photo {
                id: Uuid::new_v4(),
                file_path: "assets/front_elevation.jpg".to_string(),
                description: Some("Front elevation".to_string()),
            },
        ],
    };

    // Insert BER into database
    ber.upsert(&conn)?;

    // Load BER from database
    let loaded = Ber::load(&conn, &ber.id)?;

    // Check equality
    assert_eq!(loaded.id, ber.id);
    assert_eq!(loaded.address, ber.address);
    assert_eq!(loaded.photos.len(), ber.photos.len());

    Ok(())
}
