//! Testable connector primitives, independent of any GUI toolkit.

mod identity;
mod launch;
mod probe;
mod xml;

pub use identity::{IdentityError, MAXIMUM_NICKNAME_LENGTH, normalize_nickname};
pub use launch::{LaunchError, LaunchRequest, LaunchSpec, Runner};
pub use probe::{DEFAULT_PROBE_TIMEOUT, ProbeError, probe_messenger, probe_tcp};
pub use xml::{launcher_profile_xml, server_config_xml};
