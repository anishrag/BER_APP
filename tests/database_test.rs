use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::Connection;
use uuid::Uuid;

use ber_app::datatypes::ber::ber::Ber;
use ber_app::datatypes::ber::common::{ConstructionPhase, ConstructionPhases, Photo};
use ber_app::datatypes::ber::structural::{
    ConstructionMakeup, PlanarElement, Storey, ThermalMassCategoryEnum, ThermalMassInfo,
};
use ber_app::datatypes::ber::systems::{
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

    let id = Uuid::new_v4();
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

    let id = Uuid::new_v4();
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
        let id = Uuid::new_v4();
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

    let id = Uuid::new_v4();
    let mut ber = create_minimal_ber(id);
    ber.thermal_mass_info = ThermalMassInfo {
        id: Uuid::new_v4(),
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

    let id = Uuid::new_v4();
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

fn create_minimal_ber(id: Uuid) -> Ber {
    Ber {
        id,
        address: "123 Test Street".to_string(),
        eircode: "D01TEST".to_string(),
        construction_phases: ConstructionPhases {
            years: [(ConstructionPhase::Original, 2000)].into_iter().collect(),
        },
        heating_system: HeatingSystem {
            id: Uuid::new_v4(),
            system_type: HeatingSystemType::GasBoiler,
            description: "Standard gas boiler".to_string(),
            year_installed: Some(2018),
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
            floors: ThermalMassCategoryEnum::Medium,
            partition_walls: ThermalMassCategoryEnum::Medium,
            external_walls: ThermalMassCategoryEnum::Medium,
            separating_walls: ThermalMassCategoryEnum::Medium,
        },
        orientation_deg: 0.0,
        solar: vec![],
        photos: vec![],
    }
}

fn create_full_ber(id: Uuid) -> Ber {
    let mut ber = create_minimal_ber(id);

    ber.construction_phases = ConstructionPhases {
        years: [
            (ConstructionPhase::Original, 1950),
            (ConstructionPhase::Extension1, 1970),
        ]
        .into_iter()
        .collect(),
    };

    ber.storeys = vec![
        Storey {
            id: Uuid::new_v4(),
            name: "Ground Floor".to_string(),
            floors: vec![PlanarElement::new_floor(
                vec![
                    (0.0, 0.0, 0.0),
                    (10.0, 0.0, 0.0),
                    (10.0, 8.0, 0.0),
                    (0.0, 8.0, 0.0),
                ],
                ConstructionPhase::Original,
                ConstructionMakeup::Default,
            )],
            walls: vec![],
            roofs: vec![],
            windows: vec![],
            doors: vec![],
            rooms: vec![],
        },
        Storey {
            id: Uuid::new_v4(),
            name: "First Floor".to_string(),
            floors: vec![],
            walls: vec![],
            roofs: vec![],
            windows: vec![],
            doors: vec![],
            rooms: vec![],
        },
    ];

    ber.photos = vec![
        Photo {
            id: Uuid::new_v4(),
            file_path: "assets/front.jpg".to_string(),
            description: Some("Front".to_string()),
        },
        Photo {
            id: Uuid::new_v4(),
            file_path: "assets/living.jpg".to_string(),
            description: Some("Living".to_string()),
        },
    ];

    ber.solar = vec![SolarElements {
        id: Uuid::new_v4(),
        num_panels: 10,
        roof_element_id: Uuid::new_v4(),
        estimated_angle_degs: 30.0,
        estimated_direction: Direction::South,
        photo: None,
        description: Some("Solar PV".to_string()),
    }];

    ber
}
