//! Testable connector primitives, independent of any GUI toolkit.

mod bml;
mod codec_error;
mod detection;
mod encoded_block;
mod execution;
mod file_safety;
mod identity;
mod installation;
mod launch;
mod limits;
mod pin;
mod probe;
#[cfg(test)]
mod test_fixture;
mod wire;
mod xml;

pub use bml::BmlObject;
pub use codec_error::PinCodecError;
pub use detection::{
    BuildDetectionError, BuildEvidence, P5136_CLIENT_LOCATION, P5136_EXECUTABLE_SHA256,
    P5136_LOCALE_ID, PinDetectionSource, detect_p5136,
};
pub use encoded_block::{
    BlockEncoding, DEFAULT_KART_CRYPTO_KEY, DecodedBlock, EncodedBlockError, FLAG_KART_CRYPTO,
    FLAG_ZLIB, decode as decode_encoded_block, encode as encode_encoded_block,
};
pub use execution::{
    ConnectorCancellation, ConnectorExecution, ConnectorExecutionError, ConnectorPlan,
    ConnectorPlanError, ConnectorRequest, ConnectorStage, execute_connector,
    execute_connector_with_progress, execute_connector_with_progress_and_cancellation,
};
pub use file_safety::{
    ConnectorFileError, PersistentFilePreparation, PristineAction, PristineState,
};
pub use identity::{IdentityError, MAXIMUM_NICKNAME_LENGTH, normalize_nickname};
pub use installation::{
    DEFAULT_INSTALLATION_LOCK_TIMEOUT, DEFAULT_MAXIMUM_PERSISTENT_FILE_BYTES, InstallationError,
    InstallationOptions, PreparedInstallation, prepare_installation,
};
pub use launch::{
    LaunchError, LaunchRequest, LaunchSpec, LaunchStatus, LaunchedProcess, Runner, RunnerBackend,
};
pub use limits::CodecLimits;
pub use pin::{
    AuthMethod, P5136_MINOR_VERSION, P5136_PIN_MAGIC, P5136_RIDER_DATA_DIRECTORY,
    P5136_SCREENSHOT_DIRECTORY, P5136_STORAGE_ROOT, PinDocument, PinHeader, PinPatchOptions,
    PinPatchReport, ShallowPinHeader, decode_shallow_pin_header,
    decode_shallow_pin_header_with_limits, patch_p5136_pin, patch_p5136_pin_with_limits,
};
pub use probe::{DEFAULT_PROBE_TIMEOUT, ProbeError, probe_messenger, probe_tcp};
pub use xml::{
    LauncherProfileRole, launcher_profile_xml, launcher_profile_xml_for_role, server_config_xml,
};
