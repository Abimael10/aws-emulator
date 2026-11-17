// SPDX-License-Identifier: CC0-1.0

//! A serial this CA mints IS the value the certificate carrying it holds.
//!
//! The rule under test.
//!
//! A certificate carries its serial number as a DER INTEGER, whose octets are the MINIMAL
//! encoding of the value: a leading zero octet is no part of the number, and one is carried
//! only to keep the value positive. A client reads its serial off the certificate in the
//! rendering X.509 tooling prints — pairs of lower-case hexadecimal digits separated by
//! colons, that minimal value and nothing else — and joins it to what this service states
//! about the certificate: the serial of an audit-report row, and the serial the certificate's
//! ARN ends in.
//!
//! So a serial this service mints is the value a certificate holds, octet for octet: its
//! leading octet is not zero, because a serial minted with one would be stated by this
//! service an octet longer than the certificate carries it and would join to no row and to
//! no ARN. That is a property of the MINT rather than of one serial, so it is read here over
//! a run of them rather than over a single draw: the mint takes a fresh value per call, and
//! one in 256 uniform draws starts with a zero octet.
//!
//! The rows span what the rule generalises over: many mints, from seeds of different shapes
//! and from one seed drawn repeatedly, each joined to the DER a certificate carrying it would
//! hold — built here with the same X.509 library a client parses that certificate with, never
//! with a rendering this suite chose.

use service_com_amazonaws_acmpca_acmprivateca::state::{
    mint_serial, normalized_serial, serial_octets,
};
use x509_cert::serial_number::SerialNumber;

/// How many serials each case draws. Enough that a mint leaving its leading octet to chance
/// is reported rather than passed: one draw in 256 starts with a zero octet, so a run this
/// long holds some with near-certainty.
const DRAWS: usize = 4096;

/// The serial a client reads off a certificate, rendered as `openssl x509 -serial` prints it:
/// the DER INTEGER's octets in lower-case hexadecimal pairs, colon-separated, without the
/// leading zero octet DER carries only to keep the value positive.
fn read_off_a_certificate(serial: &SerialNumber) -> String {
    let octets: Vec<String> = serial
        .as_bytes()
        .iter()
        .skip_while(|octet| **octet == 0)
        .map(|octet| format!("{octet:02x}"))
        .collect();
    octets.join(":")
}

/// The DER serial number a certificate issued under `minted` carries, built with the X.509
/// library rather than by laying the integer out here.
fn carried_by_a_certificate(minted: &str, case: &str) -> SerialNumber {
    let bytes = hex::decode(minted)
        .unwrap_or_else(|error| panic!("case {case:?}: a minted serial is hexadecimal: {error}"));
    SerialNumber::new(&bytes)
        .unwrap_or_else(|error| panic!("case {case:?}: a minted serial is a serial: {error}"))
}

/// How one case of the table below draws its seed: from the number of the draw, so a case
/// states the SHAPE of the seeds its serials are minted from.
type SeedOfDraw = fn(usize) -> String;

#[test]
fn every_minted_serial_joins_to_the_certificate_that_carries_it() {
    // (the case, the seed each draw is minted from)
    let seeds: [(&str, SeedOfDraw); 4] = [
        ("a serial per certificate of one CA", |draw| {
            format!("arn:aws:acm-pca:us-east-1:000000000000:certificate-authority/minted|{draw}")
        }),
        ("a serial per CA", |draw| format!("minted-ca-{draw}")),
        // The same seed twice over: the mint draws a fresh value per call, so the rule holds
        // for a repeat as it does for a new name.
        ("one seed drawn again and again", |_| "minted".to_string()),
        ("an empty seed", |_| String::new()),
    ];

    for (case, seed_of) in seeds {
        for draw in 0..DRAWS {
            let seed = seed_of(draw);
            let minted = mint_serial(&seed);
            assert_eq!(
                minted.len(),
                32,
                "case {case:?} draw {draw}: expected a 16-octet serial, got {minted:?}"
            );
            let carried = carried_by_a_certificate(&minted, case);

            // The join a client makes: the serial it read off the certificate against the one
            // this service states.
            let read = read_off_a_certificate(&carried);
            assert_eq!(
                read,
                serial_octets(&minted),
                "case {case:?} draw {draw}: expected the serial a client reads off the \
                 certificate to be the one this service states for {minted:?}, got {read:?}"
            );
            // And the same number, whichever spelling it is named in: the undelimited hex an
            // ARN ends in, and the octet pairs a row states.
            assert_eq!(
                normalized_serial(&minted),
                normalized_serial(&read),
                "case {case:?} draw {draw}: expected one number behind both spellings of \
                 {minted:?}, got {read:?}"
            );
        }
    }
}

#[test]
fn a_minted_serial_leads_with_an_octet_of_its_own_value() {
    // The property behind the join above, stated on its own so a report names the mint rather
    // than the rendering: the leading octet of a 16-octet value is not zero. The cases are
    // the draws, which differ in nothing but being separate draws — the leading octet is what
    // the mint leaves to chance.
    for draw in 0..DRAWS {
        let minted = mint_serial(&format!("minted-leading-octet-{draw}"));
        assert_eq!(
            minted.len(),
            32,
            "draw {draw}: expected a 16-octet serial, got {minted:?}"
        );
        let leading = minted.get(..2).unwrap_or_default();
        assert_ne!(
            leading, "00",
            "draw {draw}: expected a serial whose 16 octets are its value, got {minted:?}, \
             which a certificate would carry one octet shorter"
        );
    }
}
