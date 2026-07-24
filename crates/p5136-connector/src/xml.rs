use std::net::SocketAddrV4;

#[must_use]
pub fn server_config_xml(login_endpoint: SocketAddrV4) -> Vec<u8> {
    format!(
        "<?xml version='1.0' encoding='UTF-16'?>\r\n\
         <config>\r\n\
         \t<server addr='{login_endpoint}'/>\r\n\
         </config>"
    )
    .into_bytes()
}

#[must_use]
pub fn launcher_profile_xml(nickname: &str) -> Vec<u8> {
    let escaped = escape_element_text(nickname);
    format!(
        "<?xml version='1.0' encoding='UTF-16'?>\r\n\
         <profile>\r\n\
         <username>{escaped}</username>\r\n\
         </profile>"
    )
    .into_bytes()
}

fn escape_element_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use super::{launcher_profile_xml, server_config_xml};

    #[test]
    fn p5136_server_xml_is_utf8_without_bom_despite_declaration() {
        let bytes = server_config_xml(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 20), 46_001));
        assert_eq!(
            bytes,
            b"<?xml version='1.0' encoding='UTF-16'?>\r\n\
              <config>\r\n\
              \t<server addr='192.0.2.20:46001'/>\r\n\
              </config>"
        );
        assert!(!bytes.starts_with(&[0xef, 0xbb, 0xbf]));
    }

    #[test]
    fn launcher_profile_matches_xelement_escaping() {
        assert_eq!(
            launcher_profile_xml("A&B<C>"),
            b"<?xml version='1.0' encoding='UTF-16'?>\r\n\
              <profile>\r\n\
              <username>A&amp;B&lt;C&gt;</username>\r\n\
              </profile>"
        );
    }
}
