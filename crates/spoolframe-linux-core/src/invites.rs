//! An invite's code typed in at a server this device knows (SDK 0.14.4),
//! in place of the link it came in: checked as the SDK checks it, and
//! made into the link the rest of the join takes.

use std::net::SocketAddr;

use crate::backend::{BackendError, Server};

/// `typed` as an invite's code, normalized (`QW7K2MFPXR4T`), whatever
/// case, dashes or spaces it was typed with; nothing is sent.
/// [`BackendError::NotAnInviteCode`] for anything that cannot be one, a
/// link included.
pub fn parse_code(typed: &str) -> Result<String, BackendError> {
    spoolframe_client_sdk::parse_code(typed).map_err(crate::sdk::backend_error)
}

/// The invite link for code `code` at `server`: its key, name and the
/// addresses this device has for it, so that connecting, joining and
/// saving go as from a link the server's admin sent. The link says
/// nothing of the account's role or when it expires; the server's
/// lookup does.
pub fn code_link(server: &Server, code: &str) -> String {
    let addrs: Vec<SocketAddr> = server
        .advertised
        .iter()
        .chain(&server.addresses)
        .filter_map(|a| a.parse().ok())
        .collect();
    spoolframe_protocol::invite::Invite {
        server: server.key.0.clone(),
        name: Some(server.name.clone()),
        addrs,
        relays: server.relays.clone(),
        code: Some(code.to_owned()),
        ..Default::default()
    }
    .link()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ServerKey;

    fn server() -> Server {
        Server {
            key: ServerKey("c4b1".repeat(16)),
            name: "Cabin".into(),
            addresses: vec!["192.168.1.40:7430".into(), "cabin.local".into()],
            pinned: false,
            fingerprint: String::new(),
            verified: false,
            on_network: true,
            verified_at: None,
            last_connected_at: None,
            server_id: None,
            advertised: Vec::new(),
            relays: Vec::new(),
            advertised_at: None,
            use_relays: None,
            relays_edited: false,
        }
    }

    #[test]
    fn a_code_is_read_whatever_its_case_and_dashes() {
        assert_eq!(parse_code("qw7k-2mfp xr4t"), Ok("QW7K2MFPXR4T".to_owned()));
    }

    #[test]
    fn a_link_is_not_a_code() {
        let link = code_link(&server(), "QW7K2MFPXR4T");
        assert_eq!(parse_code(&link), Err(BackendError::NotAnInviteCode));
    }

    #[test]
    fn the_link_names_the_server_and_carries_the_code_and_its_addresses() {
        let link = code_link(&server(), "QW7K2MFPXR4T");
        let invite = spoolframe_protocol::invite::parse_link(&link).expect("a link");
        assert_eq!(
            (
                invite.server,
                invite.name.as_deref(),
                invite.code.as_deref(),
                invite.addrs.len()
            ),
            ("c4b1".repeat(16), Some("Cabin"), Some("QW7K2MFPXR4T"), 1)
        );
    }
}
