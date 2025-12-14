// tests/database_test.rs
use ber_app::data::ber::Ber;
use ber_app::data::common::{ConstructionMakeup, ConstructionPhase, Photo};
use ber_app::data::structural::{
    ConstructionMakeup, Opes, PlanarElement, ShelteredSideInfo, Storey, ThermalMassCategory,
    ThermalMassInfo,
};
use ber_app::data::systems::{
    Direction, HeatingControls, HeatingSystem, HeatingSystemType, SolarElements, VentilationSystem,
    VentilationSystemType,
};
use chrono::NaiveDate;
use rusqlite::Connection;
use uuid::Uuid;

#[test]
fn test_init_db_creates_table() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    Ber::init_db(&conn)?;

    // Verify table exists by querying sqlite_master
    let mut stmt =
        conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='ber'")?;
    let table_exists = stmt.exists([])?;

    assert!(table_exists, "BER table should exist after init_db");
    Ok(())
}

#[test]
fn test_insert_and_load_minimal_ber() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    Ber::init_db(&conn)?;

    let ber_id = Uuid::new_v4();
    let ber = create_minimal_ber(ber_id);

    // Insert BER
    ber.upsert(&conn)?;

    // Load BER back
    let loaded = Ber::load(&conn, &ber_id)?;

    // Verify basic fields
    assert_eq!(loaded.id, ber.id);
    assert_eq!(loaded.address, ber.address);
    assert_eq!(loaded.date_assessed, ber.date_assessed);
    assert_eq!(loaded.number_of_storeys, ber.number_of_storeys);

    Ok(())
}

#[test]
fn test_insert_and_load_full_ber() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    Ber::init_db(&conn)?;

    let ber_id = Uuid::new_v4();
    let ber = create_full_ber(ber_id);

    // Insert BER
    ber.upsert(&conn)?;

    // Load BER back
    let loaded = Ber::load(&conn, &ber_id)?;

    // Verify all fields
    assert_eq!(loaded.id, ber.id);
    assert_eq!(loaded.address, ber.address);
    assert_eq!(loaded.construction_phases.len(), 2);
    assert_eq!(loaded.storeys.len(), 2);
    assert_eq!(loaded.photos.len(), 2);
    assert_eq!(loaded.solar.len(), 1);

    // Verify nested data
    assert_eq!(loaded.construction_phases[0].phase_name, "Original");
    assert_eq!(loaded.construction_phases[1].phase_name, "Extension");
    assert_eq!(loaded.storeys[0].name, "Ground Floor");
    assert_eq!(loaded.storeys[1].name, "First Floor");

    Ok(())
}

#[test]
fn test_upsert_updates_existing_record() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    Ber::init_db(&conn)?;

    let ber_id = Uuid::new_v4();
    let mut ber = create_minimal_ber(ber_id);

    // Insert initial BER
    ber.upsert(&conn)?;

    // Modify and update
    ber.address = "456 Updated Street".to_string();
    ber.number_of_storeys = 3;
    ber.upsert(&conn)?;

    // Load and verify update
    let loaded = Ber::load(&conn, &ber_id)?;
    assert_eq!(loaded.address, "456 Updated Street");
    assert_eq!(loaded.number_of_storeys, 3);

    Ok(())
}

#[test]
fn test_load_nonexistent_ber_returns_error() {
    let conn = Connection::open_in_memory().unwrap();
    Ber::init_db(&conn).unwrap();

    let nonexistent_id = Uuid::new_v4();
    let result = Ber::load(&conn, &nonexistent_id);

    assert!(
        result.is_err(),
        "Loading nonexistent BER should return error"
    );
}

#[test]
fn test_multiple_ber_entries() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    Ber::init_db(&conn)?;

    // Insert multiple BERs
    let ber1_id = Uuid::new_v4();
    let ber2_id = Uuid::new_v4();
    let ber3_id = Uuid::new_v4();

    let mut ber1 = create_minimal_ber(ber1_id);
    ber1.address = "123 First Street".to_string();

    let mut ber2 = create_minimal_ber(ber2_id);
    ber2.address = "456 Second Avenue".to_string();

    let mut ber3 = create_minimal_ber(ber3_id);
    ber3.address = "789 Third Road".to_string();

    ber1.upsert(&conn)?;
    ber2.upsert(&conn)?;
    ber3.upsert(&conn)?;

    // Load each one and verify
    let loaded1 = Ber::load(&conn, &ber1_id)?;
    let loaded2 = Ber::load(&conn, &ber2_id)?;
    let loaded3 = Ber::load(&conn, &ber3_id)?;

    assert_eq!(loaded1.address, "123 First Street");
    assert_eq!(loaded2.address, "456 Second Avenue");
    assert_eq!(loaded3.address, "789 Third Road");

    Ok(())
}

#[test]
fn test_ber_with_complex_structures() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    Ber::init_db(&conn)?;

    let ber_id = Uuid::new_v4();
    let mut ber = create_minimal_ber(ber_id);

    // Add complex thermal mass info
    ber.thermal_mass_info = ThermalMassInfo {
        id: Uuid::new_v4(),
        floors: ThermalMassCategory {
            category: "High".to_string(),
            description: Some("Concrete slab".to_string()),
        },
        partition_walls: ThermalMassCategory {
            category: "Medium".to_string(),
            description: Some("Plasterboard on studs".to_string()),
        },
        external_walls: ThermalMassCategory {
            category: "Low".to_string(),
            description: None,
        },
        separating_walls: ThermalMassCategory {
            category: "Medium".to_string(),
            description: None,
        },
    };

    ber.upsert(&conn)?;
    let loaded = Ber::load(&conn, &ber_id)?;

    assert_eq!(loaded.thermal_mass_info.floors.category, "High");
    assert_eq!(loaded.thermal_mass_info.partition_walls.category, "Medium");

    Ok(())
}

#[test]
fn test_date_serialization() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    Ber::init_db(&conn)?;

    let ber_id = Uuid::new_v4();
    let mut ber = create_minimal_ber(ber_id);

    // Test specific date
    ber.date_assessed = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();

    ber.upsert(&conn)?;
    let loaded = Ber::load(&conn, &ber_id)?;

    assert_eq!(
        loaded.date_assessed,
        NaiveDate::from_ymd_opt(2024, 6, 15).unwrap()
    );

    Ok(())
}

#[test]
fn test_load_from_file_database() -> anyhow::Result<()> {
    use std::path::Path;

    let db_path = "test_data/test_ber.db";

    // Check if test database exists
    if !Path::new(db_path).exists() {
        panic!(
            "Test database not found at {}. Run `cargo test create_test_database` first to generate it.",
            db_path
        );
    }

    // Open the file database
    let conn = Connection::open(db_path)?;

    // Query to get all BER IDs
    let mut stmt = conn.prepare("SELECT id FROM ber")?;
    let ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;

    assert!(
        !ids.is_empty(),
        "Test database should contain at least one BER entry"
    );

    // Load the first BER
    let first_id = Uuid::parse_str(&ids[0])?;
    let loaded = Ber::load(&conn, &first_id)?;

    // Verify the loaded data
    println!("Loaded BER:");
    println!("  ID: {}", loaded.id);
    println!("  Address: {}", loaded.address);
    println!("  Date Assessed: {}", loaded.date_assessed);
    println!("  Number of Storeys: {}", loaded.number_of_storeys);
    println!(
        "  Construction Phases: {}",
        loaded.construction_phases.len()
    );
    println!("  Photos: {}", loaded.photos.len());

    assert!(!loaded.address.is_empty(), "Address should not be empty");

    Ok(())
}

#[test]
#[ignore] // Run manually with: cargo test create_test_database -- --ignored --nocapture
fn create_test_database() -> anyhow::Result<()> {
    use std::fs;
    use std::path::Path;

    // Create test_data directory if it doesn't exist
    let test_data_dir = "test_data";
    if !Path::new(test_data_dir).exists() {
        fs::create_dir(test_data_dir)?;
        println!("Created directory: {}", test_data_dir);
    }

    let db_path = "test_data/test_ber.db";

    // Remove existing database if present
    if Path::new(db_path).exists() {
        fs::remove_file(db_path)?;
        println!("Removed existing database");
    }

    // Create new database
    let conn = Connection::open(db_path)?;
    Ber::init_db(&conn)?;
    println!("Initialized database at: {}", db_path);

    // Create a sample BER with known ID for testing
    let test_id = Uuid::parse_str("12345678-1234-5678-1234-567812345678").unwrap();
    let ber = create_sample_test_ber(test_id);

    ber.upsert(&conn)?;
    println!("Inserted BER with ID: {}", test_id);

    // Create a second BER with full data
    let full_id = Uuid::new_v4();
    let full_ber = create_full_ber(full_id);
    full_ber.upsert(&conn)?;
    println!("Inserted full BER with ID: {}", full_id);

    // Verify the data was inserted
    let loaded = Ber::load(&conn, &test_id)?;
    println!("\nVerified BER data:");
    println!("  Address: {}", loaded.address);
    println!("  Date: {}", loaded.date_assessed);
    println!("  Storeys: {}", loaded.number_of_storeys);

    println!("\n✅ Test database created successfully at: {}", db_path);
    println!("Run `cargo test test_load_from_file_database` to test loading from this file.");

    Ok(())
}

// Helper functions to create test data

fn create_sample_test_ber(id: Uuid) -> Ber {
    let mut ber = create_minimal_ber(id);

    ber.address = "42 Sample House Lane, Dublin".to_string();
    ber.date_assessed = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
    ber.number_of_storeys = 2;
    ber.orientation_deg = 90.0; // East-facing
    ber.sheltered_sides = 1.0;

    // Add some construction phases
    ber.construction_phases = vec![ConstructionPhase {
        id: Uuid::new_v4(),
        phase_name: "Original Build".to_string(),
        year_built: 1985,
    }];

    // Add a storey with a floor element
    ber.storeys = vec![Storey {
        id: Uuid::new_v4(),
        name: "Ground Floor".to_string(),

        floors: vec![PlanarElement::new_floor(
            vec![
                (0.0, 0.0, 0.0),
                (12.0, 0.0, 0.0),
                (12.0, 9.0, 0.0),
                (0.0, 9.0, 0.0),
            ],
            ber.construction_phases[0].id,
            ConstructionMakeup {
                layers: vec![], // or your custom construction
            },
        )],

        walls: vec![],
        roofs: vec![],
        windows: vec![],
        doors: vec![],
        rooms: vec![],
    }];

    // Add photos
    ber.photos = vec![
        Photo {
            id: Uuid::new_v4(),
            file_path: "test_data/images/front_elevation.jpg".to_string(),
            description: Some("Front view of property".to_string()),
        },
        Photo {
            id: Uuid::new_v4(),
            file_path: "test_data/images/rear_garden.jpg".to_string(),
            description: Some("Rear elevation".to_string()),
        },
    ];

    // Add heating system details
    ber.heating_system = HeatingSystem {
        id: Uuid::new_v4(),
        system_type: HeatingSystemType::GasBoiler,
        description: "Worcester Bosch 30kW combi boiler".to_string(),
        year_installed: 2015,
        controls: HeatingControls {
            id: Uuid::new_v4(),
            programmer: true,
            zones: 2,
            notes: Some("Smart thermostat with zone control".to_string()),
        },
        separate_hot_water: false,
    };

    ber
}

fn create_minimal_ber(id: Uuid) -> Ber {
    Ber {
        id,
        address: "123 Test Street".to_string(),
        date_assessed: NaiveDate::from_ymd_opt(2025, 12, 11).unwrap(),
        construction_phases: vec![],
        heating_system: HeatingSystem {
            id: Uuid::new_v4(),
            system_type: HeatingSystemType::GasBoiler,
            description: "Standard gas boiler".to_string(),
            year_installed: 2018,
            controls: HeatingControls {
                id: Uuid::new_v4(),
                programmer: true,
                zones: 2,
                notes: None,
            },
            separate_hot_water: true,
        },
        ventilation_system: VentilationSystem {
            id: Uuid::new_v4(),
            system_type: VentilationSystemType::Natural,
            description: None,
        },
        sheltered_sides: 2.0,
        number_of_storeys: 2,
        storeys: vec![],
        thermal_mass_info: ThermalMassInfo {
            id: Uuid::new_v4(),
            floors: ThermalMassCategory {
                category: "Medium".to_string(),
                description: None,
            },
            partition_walls: ThermalMassCategory {
                category: "Medium".to_string(),
                description: None,
            },
            external_walls: ThermalMassCategory {
                category: "Medium".to_string(),
                description: None,
            },
            separating_walls: ThermalMassCategory {
                category: "Medium".to_string(),
                description: None,
            },
        },
        orientation_deg: 0.0,
        solar: vec![],
        photos: vec![],
    }
}

fn create_full_ber(id: Uuid) -> Ber {
    let mut ber = create_minimal_ber(id);

    // Add construction phases
    ber.construction_phases = vec![
        ConstructionPhase {
            id: Uuid::new_v4(),
            phase_name: "Original".to_string(),
            year_built: 1950,
        },
        ConstructionPhase {
            id: Uuid::new_v4(),
            phase_name: "Extension".to_string(),
            year_built: 2010,
        },
    ];

    // Add storeys with planar elements
    let ground_floor_id = Uuid::new_v4();
    let first_floor_id = Uuid::new_v4();

    ber.storeys = vec![
        Storey {
            id: ground_floor_id,
            name: "Ground Floor".to_string(),
            floors: vec![PlanarElement::new_floor(
                vec![
                    (0.0, 0.0, 0.0),
                    (10.0, 0.0, 0.0),
                    (10.0, 8.0, 0.0),
                    (0.0, 8.0, 0.0),
                ],
                ber.construction_phases[0].id,
                ConstructionMakeup { layers: vec![] },
            )],
            walls: vec![],
            roofs: vec![],
            windows: vec![],
            doors: vec![],
            rooms: vec![],
        },
        Storey {
            id: first_floor_id,
            name: "First Floor".to_string(),
            floors: vec![],
            walls: vec![],
            roofs: vec![],
            windows: vec![],
            doors: vec![],
            rooms: vec![],
        },
    ];

    // Add photos
    ber.photos = vec![
        Photo {
            id: Uuid::new_v4(),
            file_path: "assets/front_view.jpg".to_string(),
            description: Some("Front elevation".to_string()),
        },
        Photo {
            id: Uuid::new_v4(),
            file_path: "assets/living_room.jpg".to_string(),
            description: Some("Living room".to_string()),
        },
    ];

    // Add solar elements
    ber.solar = vec![SolarElements {
        id: Uuid::new_v4(),
        num_panels: 12,
        roof_element_id: Uuid::new_v4(),
        estimated_angle_degs: 30.0,
        estimated_direction: Direction::South,
        photo: None,
        description: Some("South-facing solar array".to_string()),
    }];

    ber.orientation_deg = 45.0;
    ber.sheltered_sides = 1.5;

    ber
}
