//! The certificates' profiles (S11): what the CA, the server certificate
//! and an agent certificate say, all decided here and never by a CSR.
//!
//! | | CA | Server | Agent |
//! |---|---|---|---|
//! | Subject | CN `afkfleet CA` | CN `afkfleet server` | CN `afkfleet agent` |
//! | Lifetime | [`CA_LIFETIME`] | [`SERVER_LIFETIME`] | `[grpc] agent_cert_lifetime_days` |
//! | Basic constraints | CA, path length 0 | not a CA | not a CA |
//! | Key usage | keyCertSign | digitalSignature | digitalSignature |
//! | Extended key usage | none | serverAuth | clientAuth |
//! | SAN | none | [`ServerNames`] | `urn:uuid:<agent ID>` |
//!
//! - **Serial numbers** are 16 bytes from the [`SecureRandom`] port, the
//!   first in `0x40..=0x7F`: positive, always 16 octets, 126 random bits.
//!   rcgen's own default is a hash of the public key, which repeats when one
//!   key is certified twice.
//! - **Validity** runs from an hour before the clock's time, in whole
//!   seconds, so an agent whose clock is a little behind still accepts a
//!   fresh certificate, to that time plus the lifetime ([`Validity`]). It
//!   stays within 1970 to 9999. rcgen takes `time`'s type, which isn't a
//!   dependency of the server: it's reached through rcgen's
//!   [`rcgen::date_time_ymd`] and checked additions, so nothing can panic.

use core::fmt;
use core::time::Duration as StdDuration;
use std::net::IpAddr;

use chrono::{DateTime, NaiveDate, SubsecRound as _, TimeDelta, Utc};
use fleet_core::id::AgentId;
use fleet_core::system::{RandomError, SecureRandom};
use rcgen::string::Ia5String;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyUsagePurpose, SanType, SerialNumber,
};
use rustls::pki_types::DnsName;

use super::keys::KeyError;

/// How long the CA is valid: ten years. rustls doesn't check a trust
/// anchor's validity, so it matters only to other tools.
pub const CA_LIFETIME: TimeDelta = TimeDelta::days(3650);

/// How long the server certificate is valid: a year, renewed with
/// `pki server-cert`.
pub const SERVER_LIFETIME: TimeDelta = TimeDelta::days(365);

/// How far before the clock's time a certificate starts being valid.
pub const BACKDATE: TimeDelta = TimeDelta::hours(1);

/// The length of a serial number in bytes.
const SERIAL_LEN: usize = 16;

/// A certificate's validity would reach outside 1970 to 9999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the certificate's validity is out of range")]
pub struct OutOfRange;

/// Why the CA couldn't make or sign a certificate.
#[derive(Debug, thiserror::Error)]
pub enum PkiError {
    /// A key couldn't be made.
    #[error(transparent)]
    Key(#[from] KeyError),
    /// The serial number couldn't be drawn.
    #[error("the certificate's serial number couldn't be drawn")]
    Random(#[from] RandomError),
    /// The certificate would be valid outside the years 1970 to 9999.
    #[error(transparent)]
    OutOfRange(#[from] OutOfRange),
    /// A name couldn't be written into the certificate.
    #[error("a name couldn't be written into the certificate")]
    Name,
    /// rcgen couldn't build or sign the certificate.
    #[error("the certificate couldn't be signed")]
    Signing(#[source] rcgen::Error),
}

/// When a certificate is valid, in whole seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Validity {
    /// The first second it's valid.
    pub not_before: DateTime<Utc>,
    /// Its notAfter: valid up to and including this second.
    pub not_after: DateTime<Utc>,
}

impl Validity {
    /// Valid from [`BACKDATE`] before `now` to `now` plus `lifetime`, `now`
    /// cut to whole seconds.
    ///
    /// # Errors
    /// [`OutOfRange`] when either end falls outside 1970-01-01T00:00:00Z to
    /// 9999-12-31T23:59:59Z.
    pub fn starting(now: DateTime<Utc>, lifetime: TimeDelta) -> Result<Self, OutOfRange> {
        let now = now.trunc_subsecs(0);
        let not_before = now.checked_sub_signed(BACKDATE).ok_or(OutOfRange)?;
        let not_after = now.checked_add_signed(lifetime).ok_or(OutOfRange)?;
        let last = NaiveDate::from_ymd_opt(9999, 12, 31)
            .and_then(|day| day.and_hms_opt(23, 59, 59))
            .ok_or(OutOfRange)?
            .and_utc();
        if not_before < DateTime::UNIX_EPOCH || not_after > last {
            return Err(OutOfRange);
        }
        Ok(Self {
            not_before,
            not_after,
        })
    }

    /// Sets `params`' validity to this one, through rcgen's time type: the
    /// start of 1970 plus each end's whole seconds, added with a check. The
    /// type is never named, so `time` needn't be a dependency.
    fn set_on(&self, params: &mut CertificateParams) -> Result<(), OutOfRange> {
        let epoch = rcgen::date_time_ymd(1970, 1, 1);
        let since_epoch = |at: DateTime<Utc>| {
            u64::try_from(at.timestamp())
                .map(StdDuration::from_secs)
                .map_err(|_| OutOfRange)
        };
        params.not_before = epoch
            .checked_add(
                since_epoch(self.not_before)?
                    .try_into()
                    .map_err(|_| OutOfRange)?,
            )
            .ok_or(OutOfRange)?;
        params.not_after = epoch
            .checked_add(
                since_epoch(self.not_after)?
                    .try_into()
                    .map_err(|_| OutOfRange)?,
            )
            .ok_or(OutOfRange)?;
        Ok(())
    }
}

/// Why the server certificate's names were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ServerNamesError {
    /// No name was given.
    #[error("the server certificate needs at least one name (--dns or --ip)")]
    None,
    /// More than [`ServerNames::MAX`] names.
    #[error("the server certificate takes at most {max} names")]
    TooMany {
        /// The most allowed.
        max: usize,
    },
    /// A `--dns` value that isn't a DNS name.
    #[error("--dns value {index} isn't a DNS name")]
    NotADnsName {
        /// Its position among the `--dns` values, from 1.
        index: usize,
    },
    /// A `--dns` value with a wildcard.
    #[error("--dns value {index} is a wildcard, which a server certificate here never names")]
    Wildcard {
        /// Its position among the `--dns` values, from 1.
        index: usize,
    },
    /// An `--ip` value that's unspecified (`0.0.0.0` or `::`).
    #[error("--ip value {index} is unspecified; name the address agents dial")]
    Unspecified {
        /// Its position among the `--ip` values, from 1.
        index: usize,
    },
}

/// The names the server certificate is valid for: how agents dial the
/// server, given to `pki init` and `pki server-cert` as `--dns` and `--ip`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerNames {
    /// The DNS names, each a valid name without a wildcard.
    dns: Vec<String>,
    /// The IP addresses, none unspecified.
    ips: Vec<IpAddr>,
}

impl ServerNames {
    /// The most names a server certificate takes.
    pub const MAX: usize = 8;

    /// The names `dns` and `ips`.
    ///
    /// # Errors
    /// A [`ServerNamesError`] naming the first problem.
    pub fn try_new(dns: &[String], ips: &[IpAddr]) -> Result<Self, ServerNamesError> {
        let total = dns.len().saturating_add(ips.len());
        if total == 0 {
            return Err(ServerNamesError::None);
        }
        if total > Self::MAX {
            return Err(ServerNamesError::TooMany { max: Self::MAX });
        }
        for (index, name) in (1..).zip(dns) {
            if name.contains('*') {
                return Err(ServerNamesError::Wildcard { index });
            }
            // An address written as a DNS name would only match as a name.
            if name.parse::<IpAddr>().is_ok() || DnsName::try_from(name.as_str()).is_err() {
                return Err(ServerNamesError::NotADnsName { index });
            }
        }
        if let Some(index) = (1..)
            .zip(ips)
            .find_map(|(index, ip)| ip.is_unspecified().then_some(index))
        {
            return Err(ServerNamesError::Unspecified { index });
        }
        Ok(Self {
            dns: dns.to_vec(),
            ips: ips.to_vec(),
        })
    }

    /// The first name, the one a check can verify the certificate against.
    #[must_use]
    pub fn first(&self) -> Option<String> {
        self.dns
            .first()
            .cloned()
            .or_else(|| self.ips.first().map(ToString::to_string))
    }
}

impl fmt::Display for ServerNames {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names = self
            .dns
            .iter()
            .map(String::as_str)
            .map(str::to_owned)
            .chain(self.ips.iter().map(ToString::to_string));
        for (index, name) in names.enumerate() {
            if index > 0 {
                f.write_str(", ")?;
            }
            f.write_str(&name)?;
        }
        Ok(())
    }
}

/// A serial number: 16 bytes of `random`, the first in `0x40..=0x7F`.
///
/// # Errors
/// [`RandomError`] when the random source fails.
pub(super) fn serial(random: &dyn SecureRandom) -> Result<SerialNumber, RandomError> {
    let mut bytes = vec![0; SERIAL_LEN];
    random.fill(&mut bytes)?;
    if let Some(first) = bytes.first_mut() {
        // Positive (the top bit clear) and never shortened by DER (the next
        // bit set): always 16 octets.
        *first = 0x40 | (*first & 0x3f);
    }
    Ok(SerialNumber::from(bytes))
}

/// The parameters every profile shares: the subject, the serial and the
/// validity.
fn base(
    common_name: &str,
    validity: &Validity,
    serial: SerialNumber,
) -> Result<CertificateParams, PkiError> {
    let mut params = CertificateParams::default();
    let mut subject = DistinguishedName::new();
    subject.push(DnType::CommonName, common_name);
    params.distinguished_name = subject;
    params.serial_number = Some(serial);
    validity.set_on(&mut params)?;
    Ok(params)
}

/// The CA's profile.
///
/// # Errors
/// [`PkiError::OutOfRange`] when rcgen's time can't hold `validity`.
pub(super) fn ca_params(
    validity: &Validity,
    serial: SerialNumber,
) -> Result<CertificateParams, PkiError> {
    let mut params = base("afkfleet CA", validity, serial)?;
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    params.extended_key_usages = Vec::new();
    params.subject_alt_names = Vec::new();
    params.use_authority_key_identifier_extension = false;
    Ok(params)
}

/// The server certificate's profile.
///
/// # Errors
/// [`PkiError::OutOfRange`] when rcgen's time can't hold `validity`.
pub(super) fn server_params(
    names: &ServerNames,
    validity: &Validity,
    serial: SerialNumber,
) -> Result<CertificateParams, PkiError> {
    let mut params = base("afkfleet server", validity, serial)?;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let mut sans = Vec::with_capacity(names.dns.len() + names.ips.len());
    for name in &names.dns {
        sans.push(SanType::DnsName(
            Ia5String::try_from(name.as_str()).map_err(|_| PkiError::Name)?,
        ));
    }
    sans.extend(names.ips.iter().copied().map(SanType::IpAddress));
    params.subject_alt_names = sans;
    params.use_authority_key_identifier_extension = true;
    Ok(params)
}

/// An agent certificate's profile.
///
/// # Errors
/// [`PkiError::OutOfRange`] when rcgen's time can't hold `validity`.
pub(super) fn agent_params(
    agent: AgentId,
    validity: &Validity,
    serial: SerialNumber,
) -> Result<CertificateParams, PkiError> {
    let mut params = base("afkfleet agent", validity, serial)?;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.subject_alt_names = vec![SanType::URI(
        Ia5String::try_from(format!("urn:uuid:{agent}")).map_err(|_| PkiError::Name)?,
    )];
    params.use_authority_key_identifier_extension = true;
    Ok(params)
}

#[cfg(test)]
mod tests {
    use fleet_testkit::system::SeededRandom;
    use proptest::prelude::*;
    use rcgen::{
        BasicConstraints, DnType, DnValue, ExtendedKeyUsagePurpose, IsCa, KeyUsagePurpose, SanType,
    };
    use rstest::rstest;

    use super::*;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn validity() -> Validity {
        Validity::starting(at("2027-01-15T08:00:00.789Z"), TimeDelta::days(60)).unwrap()
    }

    fn names() -> ServerNames {
        ServerNames::try_new(
            &["localhost".to_owned(), "fleet.example.com".to_owned()],
            &["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()],
        )
        .unwrap()
    }

    fn agent() -> AgentId {
        "018bcfe5-6800-7bab-abab-abababababab".parse().unwrap()
    }

    fn serial_of(byte: u8) -> SerialNumber {
        SerialNumber::from(vec![byte; 16])
    }

    fn common_name(params: &CertificateParams) -> Option<String> {
        match params.distinguished_name.get(&DnType::CommonName) {
            Some(DnValue::Utf8String(name)) => Some(name.clone()),
            _ => None,
        }
    }

    #[test]
    fn validity_starts_an_hour_back_in_whole_seconds_and_ends_after_the_lifetime() {
        let validity = validity();

        assert_eq!(validity.not_before, at("2027-01-15T07:00:00Z"));
        assert_eq!(validity.not_after, at("2027-03-16T08:00:00Z"));
    }

    #[rstest]
    #[case::before_1970("1970-01-01T00:30:00Z", TimeDelta::days(1))]
    #[case::after_9999("9999-12-01T00:00:00Z", TimeDelta::days(60))]
    #[case::the_last_second_plus_one("9999-12-31T23:59:59Z", TimeDelta::seconds(1))]
    fn validity_outside_1970_to_9999_is_out_of_range(
        #[case] now: &str,
        #[case] lifetime: TimeDelta,
    ) {
        assert_eq!(Validity::starting(at(now), lifetime), Err(OutOfRange));
    }

    #[test]
    fn validity_may_reach_both_ends_of_the_range() {
        let first = Validity::starting(at("1970-01-01T01:00:00Z"), TimeDelta::seconds(1)).unwrap();
        let last = Validity::starting(at("9999-12-31T23:59:58Z"), TimeDelta::seconds(1)).unwrap();

        assert_eq!(first.not_before, DateTime::UNIX_EPOCH);
        assert_eq!(last.not_after, at("9999-12-31T23:59:59Z"));
    }

    #[test]
    fn the_ca_profile() {
        let params = ca_params(&validity(), serial_of(0x41)).unwrap();

        assert_eq!(common_name(&params).as_deref(), Some("afkfleet CA"));
        assert!(matches!(
            params.is_ca,
            IsCa::Ca(BasicConstraints::Constrained(0))
        ));
        assert_eq!(params.key_usages, [KeyUsagePurpose::KeyCertSign]);
        assert_eq!(params.extended_key_usages, []);
        assert_eq!(params.subject_alt_names, []);
        assert_eq!(params.serial_number, Some(serial_of(0x41)));
        assert!(!params.use_authority_key_identifier_extension);
    }

    #[test]
    fn the_server_profile() {
        let params = server_params(&names(), &validity(), serial_of(0x42)).unwrap();

        assert_eq!(common_name(&params).as_deref(), Some("afkfleet server"));
        assert!(matches!(params.is_ca, IsCa::ExplicitNoCa));
        assert_eq!(params.key_usages, [KeyUsagePurpose::DigitalSignature]);
        assert_eq!(
            params.extended_key_usages,
            [ExtendedKeyUsagePurpose::ServerAuth]
        );
        assert_eq!(
            params.subject_alt_names,
            [
                SanType::DnsName("localhost".try_into().unwrap()),
                SanType::DnsName("fleet.example.com".try_into().unwrap()),
                SanType::IpAddress("127.0.0.1".parse().unwrap()),
                SanType::IpAddress("::1".parse().unwrap()),
            ]
        );
        assert_eq!(params.serial_number, Some(serial_of(0x42)));
        assert!(params.use_authority_key_identifier_extension);
    }

    #[test]
    fn the_agent_profile() {
        let params = agent_params(agent(), &validity(), serial_of(0x43)).unwrap();

        assert_eq!(common_name(&params).as_deref(), Some("afkfleet agent"));
        assert!(matches!(params.is_ca, IsCa::ExplicitNoCa));
        assert_eq!(params.key_usages, [KeyUsagePurpose::DigitalSignature]);
        assert_eq!(
            params.extended_key_usages,
            [ExtendedKeyUsagePurpose::ClientAuth]
        );
        assert_eq!(
            params.subject_alt_names,
            [SanType::URI(
                "urn:uuid:018bcfe5-6800-7bab-abab-abababababab"
                    .try_into()
                    .unwrap()
            )]
        );
        assert_eq!(params.serial_number, Some(serial_of(0x43)));
        assert!(params.use_authority_key_identifier_extension);
    }

    #[test]
    fn server_names_are_written_as_given() {
        assert_eq!(
            names().to_string(),
            "localhost, fleet.example.com, 127.0.0.1, ::1"
        );
        assert_eq!(names().first().as_deref(), Some("localhost"));
    }

    #[rstest]
    #[case::none(&[], &[], ServerNamesError::None)]
    #[case::too_many(&["a1", "a2", "a3", "a4", "a5"], &["192.0.2.1", "192.0.2.2", "192.0.2.3", "192.0.2.4"], ServerNamesError::TooMany { max: 8 })]
    #[case::space(&["localhost", "exa mple"], &[], ServerNamesError::NotADnsName { index: 2 })]
    #[case::empty(&[""], &[], ServerNamesError::NotADnsName { index: 1 })]
    #[case::ip_as_dns(&["127.0.0.1"], &[], ServerNamesError::NotADnsName { index: 1 })]
    #[case::wildcard(&["*.example.com"], &[], ServerNamesError::Wildcard { index: 1 })]
    #[case::unspecified_v4(&[], &["0.0.0.0"], ServerNamesError::Unspecified { index: 1 })]
    #[case::unspecified_v6(&[], &["127.0.0.1", "::"], ServerNamesError::Unspecified { index: 2 })]
    fn bad_server_names_are_refused(
        #[case] dns: &[&str],
        #[case] ips: &[&str],
        #[case] error: ServerNamesError,
    ) {
        let dns: Vec<String> = dns.iter().map(|name| (*name).to_owned()).collect();
        let ips: Vec<IpAddr> = ips.iter().map(|ip| ip.parse().unwrap()).collect();

        assert_eq!(ServerNames::try_new(&dns, &ips), Err(error));
    }

    #[test]
    fn a_failing_random_source_draws_no_serial() {
        let random = SeededRandom::new(1);
        random.fail_next(RandomError::Unavailable);

        assert_eq!(serial(&random), Err(RandomError::Unavailable));
    }

    proptest! {
        #[test]
        fn every_serial_is_16_positive_octets(seed in any::<u64>()) {
            let serial = serial(&SeededRandom::new(seed)).unwrap();
            let bytes = serial.to_bytes();

            prop_assert_eq!(bytes.len(), 16);
            prop_assert!((0x40..=0x7f).contains(&bytes[0]), "{:#04x}", bytes[0]);
        }
    }
}
