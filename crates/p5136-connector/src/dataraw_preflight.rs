use std::{
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::Path,
    time::Duration,
};

use p5136_core::{
    dataraw_manifest::{
        DATARAW_PREFLIGHT_FRAME_LENGTH, DataRawManifest, DataRawManifestError,
        DataRawPreflightStatus, decode_dataraw_response, encode_dataraw_request,
    },
    ports::PortTopology,
};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

#[derive(Debug, Error)]
pub enum DataRawPreflightError {
    #[error(transparent)]
    Manifest(#[from] DataRawManifestError),
    #[error("DataRaw file-list scan task failed")]
    ManifestTask(#[from] tokio::task::JoinError),
    #[error("DataRaw preflight to {endpoint} timed out after {timeout:?}")]
    Timeout {
        endpoint: SocketAddr,
        timeout: Duration,
    },
    #[error("DataRaw preflight I/O with {endpoint} failed")]
    Io {
        endpoint: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error("server returned an invalid DataRaw preflight response")]
    InvalidResponse,
    #[error("the server has not enabled the experimental DataRaw feature")]
    ServerDisabled,
    #[error(
        "DataRaw file lists differ (client {client_files} files, server {server_files} files; client {client_digest}, server {server_digest})"
    )]
    ManifestMismatch {
        client_files: u32,
        server_files: u32,
        client_digest: String,
        server_digest: String,
    },
}

pub async fn verify_dataraw_preflight(
    game_directory: &Path,
    server: Ipv4Addr,
    ports: PortTopology,
    deadline: Duration,
) -> Result<DataRawManifest, DataRawPreflightError> {
    let root = game_directory.join("DataRaw");
    let manifest = tokio::task::spawn_blocking(move || DataRawManifest::scan(&root)).await??;
    let endpoint = SocketAddr::V4(SocketAddrV4::new(server, ports.xun_sidecar_tcp()));
    let operation = async {
        let mut stream = TcpStream::connect(endpoint).await?;
        stream.set_nodelay(true)?;
        stream.write_all(&encode_dataraw_request(manifest)).await?;
        let mut response = [0_u8; DATARAW_PREFLIGHT_FRAME_LENGTH];
        stream.read_exact(&mut response).await?;
        Ok::<_, io::Error>(response)
    };
    let response = timeout(deadline, operation)
        .await
        .map_err(|_| DataRawPreflightError::Timeout {
            endpoint,
            timeout: deadline,
        })?
        .map_err(|source| DataRawPreflightError::Io { endpoint, source })?;
    let (status, server_manifest) =
        decode_dataraw_response(&response).ok_or(DataRawPreflightError::InvalidResponse)?;
    match status {
        DataRawPreflightStatus::Match => Ok(manifest),
        DataRawPreflightStatus::ServerDisabled => Err(DataRawPreflightError::ServerDisabled),
        DataRawPreflightStatus::ManifestMismatch => {
            let server_manifest = server_manifest.ok_or(DataRawPreflightError::InvalidResponse)?;
            Err(DataRawPreflightError::ManifestMismatch {
                client_files: manifest.file_count,
                server_files: server_manifest.file_count,
                client_digest: manifest.digest_hex()[..12].to_owned(),
                server_digest: server_manifest.digest_hex()[..12].to_owned(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::Ipv4Addr, time::Duration};

    use p5136_core::dataraw_manifest::{
        DATARAW_PREFLIGHT_FRAME_LENGTH, DataRawManifest, DataRawPreflightStatus,
        decode_dataraw_request, encode_dataraw_response,
    };
    use tempfile::tempdir;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::*;

    #[tokio::test]
    async fn matching_server_manifest_allows_launch_preflight() {
        let client = tempdir().unwrap();
        let data_raw = client.path().join("DataRaw/kart_/fixture");
        fs::create_dir_all(&data_raw).unwrap();
        fs::write(data_raw.join("model.1s"), b"fixture").unwrap();
        let expected = DataRawManifest::scan(&client.path().join("DataRaw")).unwrap();

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let sidecar_port = listener.local_addr().unwrap().port();
        let ports = PortTopology::new(sidecar_port - 3).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; DATARAW_PREFLIGHT_FRAME_LENGTH];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(decode_dataraw_request(&request), Some(expected));
            stream
                .write_all(&encode_dataraw_response(
                    DataRawPreflightStatus::Match,
                    Some(expected),
                ))
                .await
                .unwrap();
        });

        let actual = verify_dataraw_preflight(
            client.path(),
            Ipv4Addr::LOCALHOST,
            ports,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(actual, expected);
        server.await.unwrap();
    }
}
