use std::{
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    time::Duration,
};

use p5136_core::ports::PortTopology;
use thiserror::Error;
use tokio::{net::TcpStream, time};

pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("connection probe to {endpoint} timed out after {timeout:?}")]
    Timeout {
        endpoint: SocketAddr,
        timeout: Duration,
    },

    #[error("connection probe to {endpoint} failed")]
    Connect {
        endpoint: SocketAddr,
        #[source]
        source: io::Error,
    },
}

pub async fn probe_messenger(
    address: Ipv4Addr,
    ports: PortTopology,
    timeout: Duration,
) -> Result<(), ProbeError> {
    probe_tcp(
        SocketAddr::V4(SocketAddrV4::new(address, ports.messenger_tcp())),
        timeout,
    )
    .await
}

pub async fn probe_tcp(endpoint: SocketAddr, timeout: Duration) -> Result<(), ProbeError> {
    match time::timeout(timeout, TcpStream::connect(endpoint)).await {
        Ok(Ok(stream)) => {
            drop(stream);
            Ok(())
        }
        Ok(Err(source)) => Err(ProbeError::Connect { endpoint, source }),
        Err(_) => Err(ProbeError::Timeout { endpoint, timeout }),
    }
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv4Addr, time::Duration};

    use tokio::net::TcpListener;

    use super::probe_tcp;

    #[tokio::test]
    async fn probe_connects_to_a_reachable_tcp_endpoint() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move { listener.accept().await.unwrap() });

        probe_tcp(endpoint, Duration::from_secs(1)).await.unwrap();
        let _ = accept.await.unwrap();
    }
}
