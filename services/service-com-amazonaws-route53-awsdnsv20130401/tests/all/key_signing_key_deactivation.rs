// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of what a SERVED `DeactivateKeySigningKey` leaves behind.
//!
//! The rule under test: the call "changes the KSK status to INACTIVE"
//! (`smithy_query(query="doc",shape="DeactivateKeySigningKey")`) for the ONE key
//! its `(HostedZoneId, Name)` address resolves to — whichever spelling of the
//! zone id the request carries — and nothing else about the zone's DNSSEC state
//! moves: the key keeps the name, the customer managed key and the DNSSEC
//! material it publishes, every other key of the zone keeps its own status and
//! material, the zone keeps the signing state it had, and the key is re-dated
//! only when the call actually moved its status, so the same call made twice is
//! served and reports exactly what the first one left.
//!
//! The sibling teardown suite owns the refusals and the delete that follows a
//! deactivation; this one owns the AFTERMATH of the deactivations it serves, so
//! its rows span what a served call can differ along: the status the addressed
//! key starts in (so a status-changing call and a call that changes nothing are
//! both rows), the spelling the request addresses the zone by (the bare id and
//! the `/hostedzone/`-prefixed `Id` a response hands back, URI-encoded), whether
//! the zone is signing over the keys, how many keys stand beside the addressed
//! one, and the forms `KeySigningKey$Name` allows ("Name can include numbers,
//! letters, and underscores (_)", length 3..128).
//!
//! The harness, the key creation and the DNSSEC reads come from the suites that
//! own them, so these tests drive the service through the same router a
//! socket-served request goes through.

use std::collections::BTreeMap;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use smithy_server_runtime::MultiServiceRouter;

use crate::dnssec_status::{enable_dnssec, get_dnssec};
use crate::hosted_zone_records::create_zone_ok;
use crate::key_signing_key_deletion::deactivate_ksk;
use crate::key_signing_keys::{
    assert_polls_a_change, create_ksk_ok, create_signing_kms_key, emulator, member,
    PERSISTED_MEMBERS, REPORTED_KEY,
};

/// The only two members of a key-signing key a deactivation may move: the status
/// it sets, and the instant it records having set it
/// (`smithy_query(query="shape",shape="KeySigningKey")`). Every OTHER member of
/// [`PERSISTED_MEMBERS`] must come back exactly as it was, and these two are
/// asserted on their own below.
const MOVABLE_MEMBERS: &[&str] = &["Status", "LastModifiedDate"];

/// One key as `GetDNSSEC` publishes it: each persisted member against its text.
type PublishedKey = BTreeMap<&'static str, String>;

/// Every key a zone's DNSSEC read reports, by name, each against the text of
/// every member of [`PERSISTED_MEMBERS`].
///
/// Read through the implemented report rather than off the store, so what is
/// compared before and after a call is what a client sees. `StatusMessage` is
/// not among the members read: it is "provided for the following key-signing key
/// (KSK) statuses: ACTION_NEEDED or INTERNAL_FAILURE"
/// (`smithy_query(query="doc",shape="KeySigningKey",member="StatusMessage")`),
/// so a key in neither status reports none.
async fn published_keys(
    router: &MultiServiceRouter,
    case: &str,
    zone_id: &str,
) -> BTreeMap<String, PublishedKey> {
    let read = get_dnssec(router, zone_id).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: GetDNSSEC must be served, got {} with body {}",
        read.status, read.body
    );
    let names: Vec<String> = read
        .all(&format!("{REPORTED_KEY}/Name"))
        .into_iter()
        .map(str::to_string)
        .collect();
    let mut keys: BTreeMap<String, PublishedKey> = BTreeMap::new();
    for tracked in PERSISTED_MEMBERS {
        let values = read.all(&format!("{REPORTED_KEY}/{tracked}"));
        assert_eq!(
            values.len(),
            names.len(),
            "case {case:?}: every reported key carries {tracked}, got {} value(s) for {} key(s) \
             in body {}",
            values.len(),
            names.len(),
            read.body
        );
        for (name, value) in names.iter().zip(values) {
            keys.entry(name.clone())
                .or_default()
                .insert(tracked, value.to_string());
        }
    }
    keys
}

/// The key named `name` among those a zone published, or a panic naming the case.
fn published<'a>(
    case: &str,
    keys: &'a BTreeMap<String, PublishedKey>,
    name: &str,
) -> &'a PublishedKey {
    keys.get(name).unwrap_or_else(|| {
        panic!(
            "case {case:?}: the zone must report the key {name}, got {:?}",
            keys.keys().collect::<Vec<_>>()
        )
    })
}

/// One tracked member of one published key.
fn published_member<'a>(
    case: &str,
    keys: &'a BTreeMap<String, PublishedKey>,
    name: &str,
    tracked: &str,
) -> &'a str {
    published(case, keys, name)
        .get(tracked)
        .map(String::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the key {name} must report {tracked}"))
}

/// One reported instant as a comparable `(seconds, nanoseconds)` pair, read by
/// the maintained timestamp reader this project already depends on rather than
/// by looking at the text here.
fn instant(case: &str, text: &str) -> (i64, u32) {
    let at = DateTime::from_str(text, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {text:?} must be a modelled timestamp: {error}")
    });
    (at.secs(), at.subsec_nanos())
}

/// One row of the served-deactivation table.
struct DeactivationRow {
    case: &'static str,
    /// The name of the key the row deactivates.
    subject: &'static str,
    /// The status that key is created in.
    initial: &'static str,
    /// The keys created beside it, as `(name, initial status)`.
    kept: &'static [(&'static str, &'static str)],
    /// Whether the zone is signing when the deactivation arrives.
    signing: bool,
    /// Whether the request spells the hosted zone id with the `/hostedzone/`
    /// prefix a response hands back (URI-encoded) rather than bare.
    prefixed: bool,
}

const DEACTIVATION_ROWS: &[DeactivationRow] = &[
    DeactivationRow {
        case: "the only key of a zone that is not signing, addressed by its bare id",
        subject: "ksk_one",
        initial: "ACTIVE",
        kept: &[],
        signing: false,
        prefixed: false,
    },
    DeactivationRow {
        case: "one of the two active keys of a signing zone, addressed by the prefixed id",
        subject: "Key_Signing_Key_2024",
        initial: "ACTIVE",
        kept: &[("other_signer", "ACTIVE")],
        signing: true,
        prefixed: true,
    },
    DeactivationRow {
        case: "a key of a signing zone that is already inactive, addressed by the prefixed id",
        subject: "K99",
        initial: "INACTIVE",
        kept: &[("other_signer", "ACTIVE")],
        signing: true,
        prefixed: true,
    },
    DeactivationRow {
        case: "the shortest name allowed, beside an inactive key of a zone that is not signing",
        subject: "ksk",
        initial: "ACTIVE",
        // The zone holds the two keys `MAX_KEY_SIGNING_KEYS_PER_ZONE` allows,
        // the second named in the all-underscore form `KeySigningKey$Name`
        // permits.
        kept: &[("_____", "INACTIVE")],
        signing: false,
        prefixed: false,
    },
];

#[tokio::test]
async fn a_served_deactivation_moves_one_key_s_status_and_nothing_else() {
    for row in DEACTIVATION_ROWS {
        let case = row.case;
        let router = emulator();
        let zone_id = create_zone_ok(
            &router,
            "example.com",
            &format!("ref-deactivation-{}", row.subject),
        )
        .await;
        let arn = create_signing_kms_key(&router, case).await;
        create_ksk_ok(&router, case, &zone_id, row.subject, row.initial, &arn).await;
        for (name, status) in row.kept {
            let kept_arn = create_signing_kms_key(&router, case).await;
            create_ksk_ok(&router, case, &zone_id, name, status, &kept_arn).await;
        }
        if row.signing {
            let enabled = enable_dnssec(&router, &zone_id).await;
            assert_eq!(
                enabled.status, 200,
                "case {case:?}: the zone must start signing, got {} with body {}",
                enabled.status, enabled.body
            );
        }
        // Both spellings of the id name the same zone, so the key the call
        // resolves to is the same one either way.
        let addressed = if row.prefixed {
            format!("%2Fhostedzone%2F{zone_id}")
        } else {
            zone_id.clone()
        };
        let before = published_keys(&router, case, &zone_id).await;

        let deactivated = deactivate_ksk(&router, &addressed, row.subject).await;

        assert_eq!(
            deactivated.status, 200,
            "case {case:?}: expected 200 (501 = still scaffolded), got {} with body {}",
            deactivated.status, deactivated.body
        );
        assert_polls_a_change(case, &deactivated);

        let after = published_keys(&router, case, &zone_id).await;
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "case {case:?}: a deactivation neither adds nor removes a key"
        );
        let status = published_member(case, &after, row.subject, "Status");
        assert_eq!(
            status, "INACTIVE",
            "case {case:?}: the addressed key must end INACTIVE, got {status:?}"
        );
        for tracked in PERSISTED_MEMBERS
            .iter()
            .filter(|tracked| !MOVABLE_MEMBERS.contains(*tracked))
        {
            let was = published_member(case, &before, row.subject, tracked);
            let now = published_member(case, &after, row.subject, tracked);
            assert_eq!(
                was, now,
                "case {case:?}: a deactivation must leave the key's {tracked} as it was, got \
                 {now:?} after {was:?}"
            );
        }

        // The key is re-dated only when the call actually moved its status: a
        // key that was already INACTIVE is reported exactly as it was.
        let dated_before = published_member(case, &before, row.subject, "LastModifiedDate");
        let dated_after = published_member(case, &after, row.subject, "LastModifiedDate");
        if row.initial == "INACTIVE" {
            assert_eq!(
                dated_before, dated_after,
                "case {case:?}: a key that was already INACTIVE is not re-dated, got \
                 {dated_after:?} after {dated_before:?}"
            );
        } else {
            assert!(
                instant(case, dated_after) >= instant(case, dated_before),
                "case {case:?}: the moved key's LastModifiedDate must not go backwards, got \
                 {dated_after:?} after {dated_before:?}"
            );
        }

        // No other key of the zone moved, in any member it publishes.
        for (name, _) in row.kept {
            assert_eq!(
                published(case, &after, name),
                published(case, &before, name),
                "case {case:?}: the key {name} beside the addressed one must be untouched"
            );
        }
        // Nor did the zone's own signing state: taking a key out of use is not
        // an enable or a disable.
        let read = get_dnssec(&router, &zone_id).await;
        let signature = member(case, &read, "Status/ServeSignature");
        let expected = if row.signing {
            "SIGNING"
        } else {
            "NOT_SIGNING"
        };
        assert_eq!(
            signature, expected,
            "case {case:?}: a deactivation must leave the zone's ServeSignature {expected}, got \
             {signature:?}"
        );

        // The call names an END state, so making it again is served and reports
        // exactly what the first one left — including the date it was moved at.
        let again = deactivate_ksk(&router, &addressed, row.subject).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: the same deactivation is served again, got {} with body {}",
            again.status, again.body
        );
        assert_polls_a_change(case, &again);
        let twice = published_keys(&router, case, &zone_id).await;
        assert_eq!(
            twice, after,
            "case {case:?}: a repeated deactivation changes nothing a client can read"
        );
    }
}
