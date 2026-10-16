//! [`X509AgentNames`]: the agent a client certificate names, read with
//! x509-parser (Plan.md P10.4; ADR-0019, S24).
//!
//! An agent certificate the server signed has exactly one SAN, the URI
//! `urn:uuid:` and its agent's ID, lowercase and hyphenated (S11). Anything
//! else names no agent: another number of SANs or of names in it, another
//! kind of name, another form of the UUID, an ID that isn't version 7, a
//! certificate that doesn't parse or has bytes after it. x509-parser's
//! errors are never passed on, since they can quote their input.

use fleet_core::id::AgentId;
use x509_parser::prelude::{FromDer as _, GeneralName, X509Certificate};

use crate::ports::pki::AgentNames;

/// The SAN reader behind [`AgentNames`].
#[derive(Debug, Clone, Copy, Default)]
pub struct X509AgentNames;

impl AgentNames for X509AgentNames {
    fn agent_of(&self, der: &[u8]) -> Option<AgentId> {
        let (rest, certificate) = X509Certificate::from_der(der).ok()?;
        if !rest.is_empty() {
            return None;
        }
        let names = certificate.subject_alternative_name().ok()??;
        let [GeneralName::URI(uri)] = names.value.general_names.as_slice() else {
            return None;
        };
        let text = uri.strip_prefix(URN_PREFIX)?;
        let agent: AgentId = text.parse().ok()?;
        // The UUID parser takes other forms too (uppercase, without
        // hyphens); the server writes only this one.
        (agent.to_string() == text).then_some(agent)
    }
}

/// What comes before the agent's ID in its SAN.
const URN_PREFIX: &str = "urn:uuid:";

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use fleet_testkit::system::SeededRandom;
    use rcgen::string::Ia5String;
    use rcgen::{CertificateParams, SanType};

    use super::*;
    use crate::infra::pki::{CertificateAuthority, Ed25519Key, ServerNames};

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }

    fn agent() -> AgentId {
        AgentId::new_v7(now(), [5; 10]).unwrap()
    }

    /// A self-signed certificate with `names` as its SANs.
    fn with_sans(names: Vec<SanType>) -> Vec<u8> {
        let key = Ed25519Key::generate(&SeededRandom::new(9)).unwrap();
        let mut params = CertificateParams::default();
        params.subject_alt_names = names;
        params.self_signed(&key).unwrap().der().to_vec()
    }

    fn uri(text: &str) -> SanType {
        SanType::URI(Ia5String::try_from(text).unwrap())
    }

    #[test]
    fn an_agent_certificate_names_its_agent() {
        let random = SeededRandom::new(5);
        let (ca, _) = CertificateAuthority::create(&random, now()).unwrap();
        let key = Ed25519Key::generate(&random).unwrap();
        let issued = ca
            .issue_agent(&key, agent(), TimeDelta::days(60), &random, now())
            .unwrap();

        assert_eq!(X509AgentNames.agent_of(&issued.der), Some(agent()));
    }

    #[test]
    fn a_server_certificate_names_no_agent() {
        let random = SeededRandom::new(5);
        let (ca, _) = CertificateAuthority::create(&random, now()).unwrap();
        let key = Ed25519Key::generate(&random).unwrap();
        let names = ServerNames::try_new(&["localhost".to_owned()], &[]).unwrap();
        let issued = ca.issue_server(&key, &names, &random, now()).unwrap();

        assert_eq!(X509AgentNames.agent_of(&issued.der), None);
    }

    #[test]
    fn the_one_uri_san_of_a_v7_id_names_the_agent() {
        let der = with_sans(vec![uri(&format!("urn:uuid:{}", agent()))]);

        assert_eq!(X509AgentNames.agent_of(&der), Some(agent()));
    }

    #[test]
    fn anything_but_one_canonical_urn_names_no_agent() {
        let canonical = format!("urn:uuid:{}", agent());
        let cases = [
            vec![],
            vec![uri(&canonical), uri(&canonical)],
            vec![uri(&canonical.to_uppercase())],
            vec![uri(&format!("urn:uuid:{}", agent().as_uuid().simple()))],
            vec![uri("urn:uuid:550e8400-e29b-41d4-a716-446655440000")],
            vec![uri(&format!("uuid:{}", agent()))],
            vec![SanType::DnsName(
                Ia5String::try_from(canonical.clone()).unwrap(),
            )],
        ];

        for sans in cases {
            let shown = format!("{sans:?}");
            assert_eq!(X509AgentNames.agent_of(&with_sans(sans)), None, "{shown}");
        }
    }

    #[test]
    fn bytes_that_arent_one_certificate_name_no_agent() {
        let mut der = with_sans(vec![uri(&format!("urn:uuid:{}", agent()))]);

        assert_eq!(X509AgentNames.agent_of(&[0x30, 0x03, 0x02, 0x01]), None);
        der.push(0);
        assert_eq!(X509AgentNames.agent_of(&der), None);
    }
}
