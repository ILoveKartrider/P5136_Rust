//! P5136's four-port topology.

use thiserror::Error;

pub const DEFAULT_CONFIGURED_PORT: u16 = 39_311;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[error("configured port {configured_port} cannot support offset +{required_offset}")]
pub struct PortOverflow {
    pub configured_port: u16,
    pub required_offset: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortTopology {
    configured_port: u16,
}

impl PortTopology {
    pub fn new(configured_port: u16) -> Result<Self, PortOverflow> {
        configured_port.checked_add(2).ok_or(PortOverflow {
            configured_port,
            required_offset: 2,
        })?;
        Ok(Self { configured_port })
    }

    #[must_use]
    pub fn configured(self) -> u16 {
        self.configured_port
    }

    #[must_use]
    pub fn login_tcp(self) -> u16 {
        self.configured_port + 1
    }

    #[must_use]
    pub fn game_udp(self) -> u16 {
        self.configured_port
    }

    #[must_use]
    pub fn p2p_udp(self) -> u16 {
        self.configured_port + 1
    }

    #[must_use]
    pub fn messenger_tcp(self) -> u16 {
        self.configured_port + 2
    }
}

impl Default for PortTopology {
    fn default() -> Self {
        Self::new(DEFAULT_CONFIGURED_PORT).expect("the P5136 default topology is valid")
    }
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_CONFIGURED_PORT, PortTopology};

    #[test]
    fn reproduces_p5136_port_offsets() {
        let ports = PortTopology::default();
        assert_eq!(ports.configured(), DEFAULT_CONFIGURED_PORT);
        assert_eq!(ports.login_tcp(), 39_312);
        assert_eq!(ports.game_udp(), 39_311);
        assert_eq!(ports.p2p_udp(), 39_312);
        assert_eq!(ports.messenger_tcp(), 39_313);
    }

    #[test]
    fn rejects_a_base_that_would_wrap() {
        assert!(PortTopology::new(u16::MAX - 1).is_err());
    }
}
