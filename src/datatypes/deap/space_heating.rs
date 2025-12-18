use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ------------------- SECTION 3: Space Heating -------------------
//

#[derive(Debug, Clone, Serialize, Deserialize)]
//currently heat source is just a string. later we can expand it to an enum with more details
pub enum HeatSource {
    Other(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceHeating {
    pub main_heating_category: HeatingCategory,

    pub main_heating_subcategory_1: Option<HeatingSubCategory1>,
    pub main_heating_subcategory_2: Option<GasOilSubCategory2>,
    pub heating_system_type: HeatingSystemType,
    pub heat_emitter_type: HeatEmitterType,
    pub heating_system_controls: HeatingSystemControls,
    pub other_heating_system_bools: OtherHeatingSystemBools,
    pub pumps_and_fans: PumpsAndFans,
    pub main_heat_source: HeatSource,
    pub secondary_heat_source: Option<HeatSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PumpsAndFans {
    pub central_heating_pumps: u16,
    pub central_heating_pumps_controlled_by_room_thermostat: bool,
    pub central_heating_pumps_inside_house: bool,
    pub oil_boiler_pumps: u16,
    pub oil_boiler_pumps_controlled_by_room_thermostat: bool,
    pub oil_boiler_pumps_inside_house: bool,
    pub gas_boiler_flue_fans: u16,
    pub warm_air_heating_or_fan_coil_radiators_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HeatingCategory {
    CentralHeatingRadsOrUnderfloor,
    ElectricStorageHeaters,
    ElectricUnderfloor,
    RoomHeatersOnly,
    WarmAirSystem,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HeatingSubCategory1 {
    ElectricBoilers,
    GasOilBoilers,
    HeatPumps,
    SolidFuelBoilers,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GasOilSubCategory2 {
    CPSU,
    GasPost1998,
    GasPre1998_nfa_flue,
    GasPre1998_fa_flue,
    Oil,
    RangeCookerOil,
    RangeCookerGas,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HeatingSystemType {
    DirectActingElectricBoiler,
    ElectricCPSURadsOrUnderfloor,
    ElectricDryCoreStorageHeaters,
    ElectricDryCoreStorageBoilerInUnheated,
    ElectricDryCoreStorageBoilerInHeated,
    ElectricWaterStorageBoilerInUnheated,
    ElectricWaterStorageBoilerInHeated,

    AirToAirHeatPump,
    AirToWaterHeatPump,
    BrineToAirHeatPump,
    BrineToWaterHeatPump,
    ExhaustAirToWaterHeatPump,
    WaterToAirHeatPump,
    WaterToWaterHeatPump,

    AutoFeedIndependentBoilerInUnheated,
    AutoFeedIndependentBoilerInHeated,
    ClosedRoomHeaterWithBoilerToRads,
    ManualFeedIndependentBoilerInUnheated,
    ManualFeedIndependentBoilerInHeated,
    OpenFireWithBackBoilerToRads,
    RangeCookerBoilerIndependentOvenAndBoiler,
    RangeCookerBoilerIntegralOvenAndBoiler,
    StoveWithBoilerToRads,
    WoodChipPelletIndependentBoiler,

    BackBoilerToRads,
    WithAutomaticIgnitionCondensing,
    WithAutomaticIgnitionNonCondensing,
    WithPermanentPilotCondensing,
    WithPermanentPilotNonCondensing,
    CombiWithAutomaticIgnitionCondensing,
    CombiWithAutomaticIgnitionNonCondensing,
    CombiWithPermanentPilotCondensing,
    CombiWithPermanentPilotNonCondensing,

    Combi,
    FloorMounted1979To1997,
    FloorMounted1998Onwards,
    FloorMountedPre1979,
    WallMounted,

    Condensing,
    CondensingCombi,
    HighOrUnknownThermalCapacity,
    LowThermalCapacity,

    OilRoomHeaterWithBoilerToRadsPost2000,
    OilRoomHeaterWithBoilerToRadsPre2000,
    StandardOilBoilerPre1985,
    StandardOilBoilerPost1998,
    StandardOilBoiler1985To1997,

    SingleBurnerAutomaticIgnition,
    SingleBurnerPermanentPilot,
    TwinBurnerAutomaticIgnitionNonCondensingPost1998,
    TwinBurnerPermanentPilotNonCondensingPre1998,
    TwinBurnerAutomaticIgnitionNonCondensingPre1998,
    TwinBurnerPermanentPilotNonCondensingPost1998,

    SingleBurner,
    TwinBurnerNonCondensingPost1998,
    TwinBurnerNonCondensingPre1998,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HeatEmitterType {
    Radiators,
    UnderfloorInConcreteSlab,
    UnderfloorInScreed,
    UnderfloorInInsulatedTimber,
    RadsAndFanCoilOrLowTempRads,
    FainCoilOrLowTempRadsOnly,
    RadsAndUnderfloorInConcreteSlabLivingRads,
    RadsAndUnderfloorInConcreteSlabLivingUnderfloor,
    LowTempRadsAndUnderfloorInConcreteSlabLivingRads,
    LowTempRadsAndUnderfloorInConcreteSlabLivingUnderfloor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HeatingSystemControls {
    ProgrammerNoThermostat,
    ProgrammerRoomThermostatTRV,
    ProgrammerTRVLikelyBypass,
    ProgrammerTRVBoilerEnergyManager,
    ProgrammerTRVFlowSwitch,
    TimeAndTemperatureZoneControl,
    RoomThermostatOnly,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtherHeatingSystemBools {
    pub has_load_or_weather_compensation: bool,
    pub has_boiler_interlock: bool,
    pub has_delayed_start: bool,
    pub has_passive_flue_gas_heat_recovery: bool,
    pub has_integrated_thermal_store: bool,
    pub condensing_boiler_fueled_by_main_gas: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FuelType {
    Oil,
    Gas,
    Electricity,
    Peat,
    Biomass,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmitterType {
    Radiators,
    Underfloor,
    Electric,
}
