use std::net::{Ipv4Addr, SocketAddrV4};

use crate::{
    bml::{BmlBudget, BmlObject, read_optional_bml, write_optional_bml},
    codec_error::PinCodecError,
    encoded_block::{self, BlockEncoding},
    limits::CodecLimits,
    wire::{WireReader, WireWriter, enforce_limit, reserve_items},
};

pub const P5136_PIN_MAGIC: u32 = 0x10EF_037E;
pub const P5136_MINOR_VERSION: u16 = 5136;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShallowPinHeader {
    pub locale_id: u16,
    pub client_location: u16,
    pub minor_version: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PinHeader {
    pub object_version: u8,
    pub locale_id: u16,
    pub client_location: u16,
    pub locale_type: u8,
    pub unknown_3: u8,
    pub minor_version: u16,
    pub unknown_4: u8,
    pub unknown_5: u8,
    pub login_type: u8,
    pub aes_key: String,
    pub url: String,
    pub patch_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthMethod {
    pub index: u8,
    pub name: String,
    pub account_config: Option<BmlObject>,
    pub login_servers: Vec<SocketAddrV4>,
    pub extra_config: Option<BmlObject>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinDocument {
    pub header: PinHeader,
    pub auth_methods: Vec<AuthMethod>,
    pub storage_config: Option<BmlObject>,
    pub extra_config: Option<BmlObject>,
    pub encoding: BlockEncoding,
    /// Unknown decoded bytes after the two top-level BML slots are retained
    /// rather than silently discarded.
    trailing_payload: Vec<u8>,
    /// Unknown bytes after the outer length-delimited encoded block are also
    /// retained.
    trailing_envelope: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PinPatchOptions {
    pub remove_ngs_on: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinPatchReport {
    pub authentication_methods: usize,
    pub removed_ngs_on_entries: usize,
}

impl PinDocument {
    pub fn decode(input: &[u8]) -> Result<Self, PinCodecError> {
        Self::decode_with_limits(input, &CodecLimits::default())
    }

    pub fn decode_with_limits(input: &[u8], limits: &CodecLimits) -> Result<Self, PinCodecError> {
        enforce_limit("PIN file", input.len(), limits.max_pin_file_bytes)?;
        let mut envelope = WireReader::new(input);
        let encoded_length =
            envelope.read_count("encoded block", limits.max_encoded_block_bytes)?;
        let encoded = envelope.take(encoded_length)?;
        let decoded = encoded_block::decode(encoded, limits)?;
        let trailing_envelope = copy_bytes(envelope.remaining(), "PIN envelope trailing bytes")?;

        let mut payload = WireReader::new(&decoded.bytes);
        let magic = payload.read_u32()?;
        if magic != P5136_PIN_MAGIC {
            return Err(PinCodecError::InvalidPinMagic {
                expected: P5136_PIN_MAGIC,
                actual: magic,
            });
        }

        let mut header = PinHeader {
            object_version: payload.read_u8()?,
            locale_id: payload.read_u16()?,
            client_location: payload.read_u16()?,
            locale_type: payload.read_u8()?,
            unknown_3: payload.read_u8()?,
            minor_version: payload.read_u16()?,
            unknown_4: payload.read_u8()?,
            unknown_5: payload.read_u8()?,
            ..PinHeader::default()
        };

        let mut budget = BmlBudget::new();
        let (auth_methods, storage_config, extra_config) = if header.object_version == 1 {
            header.login_type = 2;
            header.url = payload.read_string(limits)?;
            let endpoint_count =
                payload.read_count("legacy login server count", limits.max_collection_items)?;
            let login_servers = read_endpoints(&mut payload, endpoint_count)?;
            (
                vec![AuthMethod {
                    index: 1,
                    name: "Default".to_owned(),
                    account_config: None,
                    login_servers,
                    extra_config: None,
                }],
                None,
                None,
            )
        } else {
            header.login_type = payload.read_u8()?;
            header.aes_key = payload.read_string(limits)?;
            header.url = payload.read_string(limits)?;
            header.patch_url = payload.read_string(limits)?;

            let auth_count =
                payload.read_count("authentication method count", limits.max_collection_items)?;
            let mut auth_methods = Vec::new();
            reserve_items(&mut auth_methods, auth_count, "authentication methods")?;
            for _ in 0..auth_count {
                let index = payload.read_u8()?;
                let name = payload.read_string(limits)?;
                let account_config = read_optional_bml(&mut payload, limits, &mut budget)?;
                let endpoint_count =
                    payload.read_count("login server count", limits.max_collection_items)?;
                let login_servers = read_endpoints(&mut payload, endpoint_count)?;
                let extra_config = read_optional_bml(&mut payload, limits, &mut budget)?;
                auth_methods.push(AuthMethod {
                    index,
                    name,
                    account_config,
                    login_servers,
                    extra_config,
                });
            }

            let storage_config = if payload.remaining().is_empty() {
                None
            } else {
                read_optional_bml(&mut payload, limits, &mut budget)?
            };
            let extra_config = if payload.remaining().is_empty() {
                None
            } else {
                read_optional_bml(&mut payload, limits, &mut budget)?
            };
            (auth_methods, storage_config, extra_config)
        };

        let trailing_payload = copy_bytes(payload.remaining(), "PIN trailing payload")?;
        Ok(Self {
            header,
            auth_methods,
            storage_config,
            extra_config,
            encoding: decoded.encoding,
            trailing_payload,
            trailing_envelope,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, PinCodecError> {
        self.encode_with_limits(&CodecLimits::default())
    }

    pub fn encode_with_limits(&self, limits: &CodecLimits) -> Result<Vec<u8>, PinCodecError> {
        let payload = self.encode_payload(limits)?;
        let encoded = encoded_block::encode(&payload, self.encoding, limits)?;
        let encoded_length = i32::try_from(encoded.len())
            .map_err(|_| PinCodecError::LengthOverflow("encoded block"))?;
        let total_length = 4_usize
            .checked_add(encoded.len())
            .and_then(|length| length.checked_add(self.trailing_envelope.len()))
            .ok_or(PinCodecError::LengthOverflow("PIN file"))?;
        enforce_limit("PIN file", total_length, limits.max_pin_file_bytes)?;

        let mut envelope = WireWriter::new(limits.max_pin_file_bytes);
        envelope.write_i32(encoded_length)?;
        envelope.write_bytes(&encoded)?;
        envelope.write_bytes(&self.trailing_envelope)?;
        Ok(envelope.into_inner())
    }

    pub fn patch_p5136_endpoint(
        &mut self,
        endpoint: SocketAddrV4,
        options: PinPatchOptions,
    ) -> Result<PinPatchReport, PinCodecError> {
        if self.header.minor_version != P5136_MINOR_VERSION {
            return Err(PinCodecError::WrongProtocol {
                expected: P5136_MINOR_VERSION,
                actual: self.header.minor_version,
            });
        }
        if self.auth_methods.is_empty() {
            return Err(PinCodecError::MissingAuthenticationMethods);
        }

        for auth_method in &mut self.auth_methods {
            auth_method.login_servers.clear();
            auth_method.login_servers.push(endpoint);
        }

        let removed_ngs_on_entries = if options.remove_ngs_on {
            [&mut self.storage_config, &mut self.extra_config]
                .into_iter()
                .filter_map(Option::as_mut)
                .filter(|config| config.name.eq_ignore_ascii_case("extra"))
                .map(|config| config.remove_direct_children_named("NgsOn"))
                .sum()
        } else {
            0
        };

        Ok(PinPatchReport {
            authentication_methods: self.auth_methods.len(),
            removed_ngs_on_entries,
        })
    }

    #[must_use]
    pub fn trailing_payload(&self) -> &[u8] {
        &self.trailing_payload
    }

    #[must_use]
    pub fn trailing_envelope(&self) -> &[u8] {
        &self.trailing_envelope
    }

    fn encode_payload(&self, limits: &CodecLimits) -> Result<Vec<u8>, PinCodecError> {
        let mut payload = WireWriter::new(limits.max_decoded_block_bytes);
        payload.write_u32(P5136_PIN_MAGIC)?;
        payload.write_u8(self.header.object_version)?;
        payload.write_u16(self.header.locale_id)?;
        payload.write_u16(self.header.client_location)?;
        payload.write_u8(self.header.locale_type)?;
        payload.write_u8(self.header.unknown_3)?;
        payload.write_u16(self.header.minor_version)?;
        payload.write_u8(self.header.unknown_4)?;
        payload.write_u8(self.header.unknown_5)?;

        if self.header.object_version == 1 {
            payload.write_string(&self.header.url, limits)?;
            let endpoint_count = self
                .auth_methods
                .iter()
                .try_fold(0_usize, |total, auth| {
                    total.checked_add(auth.login_servers.len())
                })
                .ok_or(PinCodecError::LengthOverflow("legacy login server count"))?;
            payload.write_count(
                endpoint_count,
                "legacy login server count",
                limits.max_collection_items,
            )?;
            for endpoint in self
                .auth_methods
                .iter()
                .flat_map(|auth| &auth.login_servers)
            {
                write_endpoint(&mut payload, *endpoint)?;
            }
        } else {
            payload.write_u8(self.header.login_type)?;
            payload.write_string(&self.header.aes_key, limits)?;
            payload.write_string(&self.header.url, limits)?;
            payload.write_string(&self.header.patch_url, limits)?;
            payload.write_count(
                self.auth_methods.len(),
                "authentication method count",
                limits.max_collection_items,
            )?;
            let mut budget = BmlBudget::new();
            for auth_method in &self.auth_methods {
                payload.write_u8(auth_method.index)?;
                payload.write_string(&auth_method.name, limits)?;
                write_optional_bml(
                    &mut payload,
                    auth_method.account_config.as_ref(),
                    limits,
                    &mut budget,
                )?;
                payload.write_count(
                    auth_method.login_servers.len(),
                    "login server count",
                    limits.max_collection_items,
                )?;
                for endpoint in &auth_method.login_servers {
                    write_endpoint(&mut payload, *endpoint)?;
                }
                write_optional_bml(
                    &mut payload,
                    auth_method.extra_config.as_ref(),
                    limits,
                    &mut budget,
                )?;
            }
            write_optional_bml(
                &mut payload,
                self.storage_config.as_ref(),
                limits,
                &mut budget,
            )?;
            write_optional_bml(
                &mut payload,
                self.extra_config.as_ref(),
                limits,
                &mut budget,
            )?;
        }
        payload.write_bytes(&self.trailing_payload)?;
        Ok(payload.into_inner())
    }
}

pub fn patch_p5136_pin(
    input: &[u8],
    endpoint: SocketAddrV4,
    options: PinPatchOptions,
) -> Result<(Vec<u8>, PinPatchReport), PinCodecError> {
    patch_p5136_pin_with_limits(input, endpoint, options, &CodecLimits::default())
}

pub fn patch_p5136_pin_with_limits(
    input: &[u8],
    endpoint: SocketAddrV4,
    options: PinPatchOptions,
    limits: &CodecLimits,
) -> Result<(Vec<u8>, PinPatchReport), PinCodecError> {
    let mut pin = PinDocument::decode_with_limits(input, limits)?;
    let report = pin.patch_p5136_endpoint(endpoint, options)?;
    let output = pin.encode_with_limits(limits)?;

    // Mirror the C# connector's write-then-reparse verification before a
    // caller atomically replaces the live file.
    let verified = PinDocument::decode_with_limits(&output, limits)?;
    if verified.header.minor_version != P5136_MINOR_VERSION
        || verified.auth_methods.len() != report.authentication_methods
        || verified
            .auth_methods
            .iter()
            .any(|auth| auth.login_servers.as_slice() != [endpoint])
    {
        return Err(PinCodecError::EndpointVerificationFailed);
    }

    Ok((output, report))
}

pub fn decode_shallow_pin_header(input: &[u8]) -> Result<ShallowPinHeader, PinCodecError> {
    decode_shallow_pin_header_with_limits(input, &CodecLimits::default())
}

pub fn decode_shallow_pin_header_with_limits(
    input: &[u8],
    limits: &CodecLimits,
) -> Result<ShallowPinHeader, PinCodecError> {
    enforce_limit("PIN file", input.len(), limits.max_pin_file_bytes)?;
    let mut envelope = WireReader::new(input);
    let encoded_length = envelope.read_count("encoded block", limits.max_encoded_block_bytes)?;
    let decoded = encoded_block::decode(envelope.take(encoded_length)?, limits)?;
    let mut payload = WireReader::new(&decoded.bytes);
    let magic = payload.read_u32()?;
    if magic != P5136_PIN_MAGIC {
        return Err(PinCodecError::InvalidPinMagic {
            expected: P5136_PIN_MAGIC,
            actual: magic,
        });
    }

    payload.read_u8()?;
    let locale_id = payload.read_u16()?;
    let client_location = payload.read_u16()?;
    payload.read_u8()?;
    payload.read_u8()?;
    let minor_version = payload.read_u16()?;
    Ok(ShallowPinHeader {
        locale_id,
        client_location,
        minor_version,
    })
}

fn read_endpoints(
    payload: &mut WireReader<'_>,
    count: usize,
) -> Result<Vec<SocketAddrV4>, PinCodecError> {
    let mut endpoints = Vec::new();
    reserve_items(&mut endpoints, count, "login servers")?;
    for _ in 0..count {
        let octets = [
            payload.read_u8()?,
            payload.read_u8()?,
            payload.read_u8()?,
            payload.read_u8()?,
        ];
        let port = payload.read_u16()?;
        endpoints.push(SocketAddrV4::new(Ipv4Addr::from(octets), port));
    }
    Ok(endpoints)
}

fn write_endpoint(payload: &mut WireWriter, endpoint: SocketAddrV4) -> Result<(), PinCodecError> {
    payload.write_bytes(&endpoint.ip().octets())?;
    payload.write_u16(endpoint.port())
}

fn copy_bytes(input: &[u8], kind: &'static str) -> Result<Vec<u8>, PinCodecError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(input.len())
        .map_err(|_| PinCodecError::Allocation(kind))?;
    output.extend_from_slice(input);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use sha2::{Digest, Sha256};

    use super::{P5136_MINOR_VERSION, PinDocument, PinPatchOptions, patch_p5136_pin};
    use crate::{
        bml::BmlObject,
        encoded_block::{DEFAULT_KART_CRYPTO_KEY, FLAG_KART_CRYPTO, FLAG_ZLIB},
        limits::CodecLimits,
        test_fixture::{CSHARP_SYNTHETIC_PIN_SHA256, csharp_synthetic_pin},
    };

    #[test]
    fn decodes_and_roundtrips_the_csharp_synthetic_fixture() {
        let fixture = csharp_fixture();
        assert_eq!(fixture.len(), 356);
        assert_eq!(
            format!("{:X}", Sha256::digest(&fixture)),
            CSHARP_SYNTHETIC_PIN_SHA256
        );

        let pin = PinDocument::decode(&fixture).unwrap();
        assert_eq!(pin.header.locale_id, 1002);
        assert_eq!(pin.header.client_location, 118);
        assert_eq!(pin.header.minor_version, P5136_MINOR_VERSION);
        assert_eq!(pin.encoding.flags, FLAG_ZLIB | FLAG_KART_CRYPTO);
        assert_eq!(pin.encoding.kart_crypto_key, Some(DEFAULT_KART_CRYPTO_KEY));
        assert_eq!(pin.auth_methods.len(), 2);
        assert_eq!(pin.auth_methods[0].login_servers.len(), 2);
        assert_eq!(pin.auth_methods[1].login_servers.len(), 1);

        let extra = pin.extra_config.as_ref().unwrap();
        assert!(extra.name.eq_ignore_ascii_case("extra"));
        assert!(
            extra
                .children
                .iter()
                .any(|child| child.name.eq_ignore_ascii_case("NgsOn"))
        );
        assert!(
            extra
                .children
                .iter()
                .any(|child| child.name == "UnknownFeature")
        );
        assert!(
            extra
                .children
                .iter()
                .find(|child| child.name == "UnknownFeature")
                .is_some_and(|child| !child.attributes.is_empty())
        );

        let reencoded = pin.encode().unwrap();
        let reparsed = PinDocument::decode(&reencoded).unwrap();
        assert_eq!(reparsed, pin);
    }

    #[test]
    fn endpoint_patch_replaces_every_auth_list_and_removes_only_ngs_on() {
        let fixture = csharp_fixture();
        let endpoint = SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 10), 45_001);
        let (patched_bytes, report) = patch_p5136_pin(
            &fixture,
            endpoint,
            PinPatchOptions {
                remove_ngs_on: true,
            },
        )
        .unwrap();

        assert_eq!(report.authentication_methods, 2);
        assert_eq!(report.removed_ngs_on_entries, 1);
        let patched = PinDocument::decode(&patched_bytes).unwrap();
        assert_eq!(patched.encoding.flags, FLAG_ZLIB | FLAG_KART_CRYPTO);
        assert_eq!(
            patched.encoding.kart_crypto_key,
            Some(DEFAULT_KART_CRYPTO_KEY)
        );
        assert!(
            patched
                .auth_methods
                .iter()
                .all(|auth| auth.login_servers == [endpoint])
        );

        let extra = patched.extra_config.as_ref().unwrap();
        assert!(
            extra
                .children
                .iter()
                .all(|child| !child.name.eq_ignore_ascii_case("NgsOn"))
        );
        assert!(
            extra
                .children
                .iter()
                .any(|child| child.name == "UnknownFeature")
        );
        assert!(
            extra
                .children
                .iter()
                .find(|child| child.name == "UnknownFeature")
                .is_some_and(|child| !child.attributes.is_empty())
        );
    }

    #[test]
    fn ngs_on_is_retained_when_removal_is_disabled() {
        let mut pin = PinDocument::decode(&csharp_fixture()).unwrap();
        pin.patch_p5136_endpoint(
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 39_312),
            PinPatchOptions {
                remove_ngs_on: false,
            },
        )
        .unwrap();
        assert!(
            pin.extra_config
                .unwrap()
                .children
                .iter()
                .any(|child| child.name.eq_ignore_ascii_case("NgsOn"))
        );
    }

    #[test]
    fn encoding_rejects_bml_beyond_the_depth_limit() {
        let mut pin = PinDocument::decode(&csharp_fixture()).unwrap();
        let mut root = BmlObject {
            name: "root".to_owned(),
            ..BmlObject::default()
        };
        let mut cursor = &mut root;
        for index in 0..4 {
            cursor.children.push(BmlObject {
                name: format!("child-{index}"),
                ..BmlObject::default()
            });
            cursor = cursor.children.last_mut().unwrap();
        }
        pin.extra_config = Some(root);
        let limits = CodecLimits {
            max_bml_depth: 2,
            ..CodecLimits::default()
        };
        assert!(pin.encode_with_limits(&limits).is_err());
    }

    #[test]
    fn decoding_rejects_declared_strings_before_allocating_them() {
        let mut pin = PinDocument::decode(&csharp_fixture()).unwrap();
        pin.auth_methods[0].name = "long authentication name".to_owned();
        let encoded = pin.encode().unwrap();
        let limits = CodecLimits {
            max_string_code_units: 4,
            ..CodecLimits::default()
        };
        assert!(PinDocument::decode_with_limits(&encoded, &limits).is_err());
    }

    fn csharp_fixture() -> Vec<u8> {
        csharp_synthetic_pin()
    }
}
