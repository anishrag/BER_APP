use anyhow::Result;
use chrono::NaiveDate;
use domain::points::Point2f64;
use rusqlite::Connection;

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::{ConstructionPhase, ConstructionPhases, ItemId, Photo};
use ber_model::ber::structural::{FloorRegions, Storey, ThermalMassCategoryEnum, ThermalMassInfo};
use ber_model::ber::systems::{
    Direction, HeatingControls, HeatingSystem, HeatingSystemType, SolarElements, VentilationSystem,
    VentilationSystemType,
};

use ber_app::engine::ber_repository::{BerRecord, BerRepository};

/// ----------
/// Test setup
/// ----------
fn setup_repo() -> Result<BerRepository<'static>> {
    let conn = Connection::open_in_memory()?;
    let conn = Box::leak(Box::new(conn)); // OK for integration tests
    let repo = BerRepository::new(conn);
    repo.init_db()?;
    Ok(repo)
}

/// ------------------------------
/// Insert + load minimal BER
/// ------------------------------
#[test]
fn test_insert_and_load_minimal_ber() -> Result<()> {
    let repo = setup_repo()?;

    let id = ItemId::default();
    let ber = create_minimal_ber(id);

    repo.upsert(&BerRecord {
        id,
        name: "Minimal BER".to_string(),
        date_assessed: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        data: ber.clone(),
    })?;

    let loaded = repo.load(&id)?;

    assert_eq!(loaded.id, id);
    assert_eq!(loaded.name, "Minimal BER");
    assert_eq!(loaded.data.address, ber.address);

    Ok(())
}

/// ------------------------------
/// Insert + load full BER
/// ------------------------------
#[test]
fn test_insert_and_load_full_ber() -> Result<()> {
    let repo = setup_repo()?;

    let id = ItemId::default();
    let ber = create_full_ber(id);

    repo.upsert(&BerRecord {
        id,
        name: "Full BER".to_string(),
        date_assessed: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        data: ber.clone(),
    })?;

    let loaded = repo.load(&id)?;

    assert_eq!(loaded.data.construction_phases.years.len(), 2);
    assert_eq!(loaded.data.storeys.len(), 2);
    assert_eq!(loaded.data.photos.len(), 2);
    assert_eq!(loaded.data.solar.len(), 1);

    Ok(())
}

/// ------------------------------
/// Multiple BER entries
/// ------------------------------
#[test]
fn test_multiple_ber_entries() -> Result<()> {
    let repo = setup_repo()?;

    for i in 0..3 {
        let id = ItemId::default();
        let mut ber = create_minimal_ber(id);
        ber.address = format!("{} Test Street", i);

        repo.upsert(&BerRecord {
            id,
            name: format!("BER {}", i),
            date_assessed: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            data: ber,
        })?;
    }

    let list = repo.list()?;
    assert_eq!(list.len(), 3);

    Ok(())
}

/// ------------------------------
/// Thermal mass persistence
/// ------------------------------
#[test]
fn test_ber_with_complex_structures() -> Result<()> {
    let repo = setup_repo()?;

    let id = ItemId::default();
    let mut ber = create_minimal_ber(id);
    ber.thermal_mass_info = ThermalMassInfo {
        id: ItemId::default(),
        floors: ThermalMassCategoryEnum::Medium,
        partition_walls: ThermalMassCategoryEnum::High,
        external_walls: ThermalMassCategoryEnum::Medium,
        separating_walls: ThermalMassCategoryEnum::Medium,
    };

    repo.upsert(&BerRecord {
        id,
        name: "Thermal Test".to_string(),
        date_assessed: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        data: ber,
    })?;

    let loaded = repo.load(&id)?;

    assert_eq!(
        loaded.data.thermal_mass_info.floors,
        ThermalMassCategoryEnum::Medium
    );
    assert_eq!(
        loaded.data.thermal_mass_info.partition_walls,
        ThermalMassCategoryEnum::High
    );

    Ok(())
}

/// ------------------------------
/// Delete BER
/// ------------------------------
#[test]
fn test_delete_ber() -> Result<()> {
    let repo = setup_repo()?;

    let id = ItemId::default();
    let ber = create_minimal_ber(id);

    repo.upsert(&BerRecord {
        id,
        name: "Delete Me".to_string(),
        date_assessed: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
        data: ber,
    })?;

    repo.delete(&id)?;

    let err = repo.load(&id).unwrap_err();
    assert!(matches!(err, rusqlite::Error::QueryReturnedNoRows));

    Ok(())
}

/// ============================
/// Helper BER builders
/// ============================
fn create_minimal_ber(id: ItemId) -> Ber {
    Ber {
        id,
        address: "123 Test Street".to_string(),
        eircode: "D01TEST".to_string(),
        construction_phases: ConstructionPhases {
            years: [(ConstructionPhase::Original, 2000)].into_iter().collect(),
            nicknames: Default::default(),
        },
        heating_system: HeatingSystem {
            id: ItemId::default(),
            system_type: HeatingSystemType::GasBoiler,
            description: "Standard gas boiler".to_string(),
            year_installed: Some(2018),
            controls: HeatingControls {
                programmer: true,
                zones: 2,
                notes: None,
            },
            separate_hot_water: true,
        },
        ventilation_system: VentilationSystem {
            id: ItemId::default(),
            system_type: VentilationSystemType::Natural,
            description: None,
        },
        sheltered_sides: 2.0,
        number_of_storeys: 2,
        storeys: vec![],
        wall_constructions: vec![],
        floor_constructions: vec![],
        ceiling_constructions: vec![],
        window_types: vec![],
        thermal_mass_info: ThermalMassInfo {
            id: ItemId::default(),
            floors: ThermalMassCategoryEnum::Medium,
            partition_walls: ThermalMassCategoryEnum::Medium,
            external_walls: ThermalMassCategoryEnum::Medium,
            separating_walls: ThermalMassCategoryEnum::Medium,
        },
        orientation_deg: 0.0,
        solar: vec![],
        photos: vec![],
        mapping_reviewed: false,
    }
}

fn create_full_ber(id: ItemId) -> Ber {
    let mut ber = create_minimal_ber(id);

    ber.construction_phases = ConstructionPhases {
        years: [
            (ConstructionPhase::Original, 1950),
            (ConstructionPhase::Extension1, 1970),
        ]
        .into_iter()
        .collect(),
        nicknames: Default::default(),
    };

    // Shared floor construction the ground-floor region points at.
    let floor_cid =
        ber.ensure_floor_construction(Default::default(), Default::default(), Default::default());

    ber.storeys = vec![
        Storey {
            id: ItemId::default(),
            storey_number: 0,
            name: "Ground Floor".to_string(),
            floors: FloorRegions::new(
                vec![
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(10.0, 0.0),
                    Point2f64::new(10.0, 8.0),
                    Point2f64::new(0.0, 8.0),
                ],
                floor_cid,
            ),
            walls: Default::default(),
            roofs: Default::default(),
            windows: vec![],
            doors: vec![],
            rooms: vec![],
            living_areas: Default::default(),
            ceiling_height_m: None,
            roof_apexes: vec![],
            roof_apex_hints: vec![],
            floor_thickness_m: None,
            ceiling_steps: vec![],
            floor_splits: vec![],
            roof_splits: vec![],
            wall_splits: vec![],
        },
        Storey {
            id: ItemId::default(),
            storey_number: 1,
            name: "First Floor".to_string(),
            floors: Default::default(),
            walls: Default::default(),
            roofs: Default::default(),
            windows: vec![],
            doors: vec![],
            rooms: vec![],
            living_areas: Default::default(),
            ceiling_height_m: None,
            roof_apexes: vec![],
            roof_apex_hints: vec![],
            floor_thickness_m: None,
            ceiling_steps: vec![],
            floor_splits: vec![],
            roof_splits: vec![],
            wall_splits: vec![],
        },
    ];

    ber.photos = vec![
        Photo {
            id: ItemId::default(),
            file_path: "assets/front.jpg".to_string(),
            thumbnail_path: None,
            description: Some("Front".to_string()),
            side: None,
        },
        Photo {
            id: ItemId::default(),
            file_path: "assets/living.jpg".to_string(),
            thumbnail_path: None,
            description: Some("Living".to_string()),
            side: None,
        },
    ];

    ber.solar = vec![SolarElements {
        id: ItemId::default(),
        num_panels: 10,
        roof_element_id: ItemId::default(),
        estimated_angle_degs: 30.0,
        estimated_direction: Direction::South,
        photo: None,
        description: Some("Solar PV".to_string()),
    }];

    ber
}
