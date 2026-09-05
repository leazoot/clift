//! Answering "would a send work right now, and if not, which part is broken".

pub mod doctor;
pub mod dto;

pub use doctor::{
    CheckName, ConfigState, DoctorCheck, DoctorReport, Environment, InjectionFacts, InjectionState,
    LocalFacts, RelayProbe, diagnose, probe_relay,
};
pub use dto::{
    DoctorCheckDto, DoctorDto, SCHEMA_VERSION, StatusDto, StatusRelayDto, StatusTargetDto,
    status_word,
};
