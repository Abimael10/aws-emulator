// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of the two calls that move a key-signing key's status —
//! `ActivateKeySigningKey` and `DeactivateKeySigningKey` — and of what a SERVED
//! one leaves behind.
//!
//! The rule under test: each call names an END state for the ONE key its
//! `(HostedZoneId, Name)` address resolves to — ACTIVE for the activate
//! ("activates a key-signing key (KSK) so that it can be used for signing by
//! DNSSEC. This operation changes the KSK status to ACTIVE",
//! `smithy_query(query="doc",shape="ActivateKeySigningKey")`), INACTIVE for the
//! deactivate ("changes the KSK status to INACTIVE",
//! `smithy_query(query="doc",shape="DeactivateKeySigningKey")`) — so whichever
//! status the addressed key stands in, the call is served with a pollable
//! `ChangeInfo` and leaves the key in that end state, which makes the same call
//! made again, or several of them made at once, answer alike. Nothing else about
//! the zone's DNSSEC state moves: the key keeps the name, the customer managed
//! key and the DNSSEC material it publishes, every other key of the zone keeps
//! its own status and material, the zone keeps the signing state it had, and the
//! key is re-dated only when the call actually moved its status.
//!
//! A call is refused only where a modelled condition holds: the activate refuses
//! a key that is on its way out of the zone (`InvalidKeySigningKeyStatus`) and a
//! key whose customer managed key the service can no longer sign with
//! (`InvalidKMSArn`), and the deactivate refuses the one ACTIVE key a signing
//! zone would be left with nothing to sign with (`KeySigningKeyInUse`) — which
//! the activation of a second key clears just as disabling signing does. A
//! refusal moves nothing.
//!
//! The ADDRESS is the pair, so the zone half is what tells two keys of the same
//! name apart: a name is "unique for each key-signing key in the same hosted
//! zone" (`smithy_query(query="doc",shape="KeySigningKey",member="Name")`) and no
//! wider, so a call resolves the key of the zone it names and the identically
//! named key of every other zone is left exactly as it was.
//!
//! The rows below therefore span what a status call can differ along: the status
//! the addressed key stands in when it arrives — every value of
//! `KeySigningKey$Status`, including the two a failed request leaves behind,
//! which the same member's documentation names this very pair of operations as
//! the correction for — the spelling the request addresses the zone by (the bare
//! id and the `/hostedzone/`-prefixed `Id` a response hands back, URI-encoded),
//! whether the zone is signing over the keys, how many keys stand beside the
//! addressed one, the forms `KeySigningKey$Name` allows ("Name can include
//! numbers, letters, and underscores (_)", length 3..128), and how many callers
//! make the same call at once.
//!
//! The harness, the key creation and the DNSSEC reads come from the suites that
//! own them, so these tests drive the service through the same router a
//! socket-served request goes through.

use std::collections::BTreeMap;
use std::sync::Barrier;

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53_awsdnsv20130401::state::service_state;

use crate::dnssec_status::{disable_dnssec, enable_dnssec, get_dnssec};
use crate::hosted_zone_records::{create_zone_ok, Reply};
use crate::key_signing_key_deletion::{
    activate_ksk, deactivate_ksk, in_use_message, reported_status, unusable_key_message,
};
use crate::key_signing_keys::{
    assert_polls_a_change, create_ksk_ok, create_ksk_with_reference, create_signing_kms_key,
    emulator, emulator_with_store, member, schedule_kms_key_deletion, set_kms_key_enabled,
    PERSISTED_MEMBERS, REPORTED_KEY,
};
use crate::refusal_message_rules::message_of;

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

/// Put the key `name` of the zone `zone_id` into `status`, by writing the store
/// the handlers read.
///
/// `CreateKeySigningKey` creates a key ACTIVE or INACTIVE only
/// (`smithy_query(query="doc",shape="CreateKeySigningKeyRequest",member="Status")`),
/// so the statuses a request LEAVES BEHIND — the two
/// `KeySigningKey$StatusMessage` is "provided for" and the one a delete passes
/// through — are reached the way the sibling suites reach them. A key in one of
/// the two statuses that carry a message is given one, so what a served call
/// does with it is observable.
fn force_status(
    engine: &engine::InMemoryEngine,
    case: &str,
    zone_id: &str,
    name: &str,
    status: &str,
) {
    let state = service_state(engine).expect("the Route 53 store must be reachable");
    let mut store = state.lock();
    let zone = store
        .zones
        .get_mut(zone_id)
        .unwrap_or_else(|| panic!("case {case:?}: the zone must be in the store"));
    let key = zone
        .key_signing_keys
        .get_mut(name)
        .unwrap_or_else(|| panic!("case {case:?}: the key {name} must be in the store"));
    key.status = Some(status.to_string());
    key.status_message = match status {
        "ACTION_NEEDED" | "INTERNAL_FAILURE" => Some(format!(
            "The customer managed key for the key-signing key {name} needs attention."
        )),
        _ => None,
    };
}

/// Every `StatusMessage` a zone's DNSSEC read reports, over all of its keys.
async fn reported_status_messages(router: &MultiServiceRouter, zone_id: &str) -> Vec<String> {
    get_dnssec(router, zone_id)
        .await
        .all(&format!("{REPORTED_KEY}/StatusMessage"))
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// One row of the served-activation table.
struct ActivationRow {
    case: &'static str,
    /// The name of the key the row activates.
    subject: &'static str,
    /// The status that key is created in — ACTIVE or INACTIVE, the only two a
    /// create accepts.
    created: &'static str,
    /// The status it is then put into, for a status no create reaches.
    forced: Option<&'static str>,
    /// The keys created beside it, as `(name, initial status)`.
    kept: &'static [(&'static str, &'static str)],
    /// Whether the zone is signing when the activation arrives.
    signing: bool,
    /// Whether the request spells the hosted zone id with the `/hostedzone/`
    /// prefix a response hands back (URI-encoded) rather than bare.
    prefixed: bool,
}

/// The rows cross every status `KeySigningKey$Status` lists other than the one a
/// key on its way out stands in (that one is refused, below) with the zone's
/// signing state, the key population, the two spellings of the zone id and the
/// name forms the member allows.
const ACTIVATION_ROWS: &[ActivationRow] = &[
    ActivationRow {
        case: "the only key of a zone that is not signing, addressed by its bare id",
        subject: "ksk_one",
        created: "INACTIVE",
        forced: None,
        kept: &[],
        signing: false,
        prefixed: false,
    },
    ActivationRow {
        case: "the key a signing zone is already signing with, addressed by the prefixed id",
        subject: "Key_Signing_Key_2024",
        created: "ACTIVE",
        forced: None,
        kept: &[],
        signing: true,
        prefixed: true,
    },
    ActivationRow {
        case: "a key left in INTERNAL_FAILURE beside the active key of a signing zone",
        subject: "K99",
        created: "INACTIVE",
        forced: Some("INTERNAL_FAILURE"),
        kept: &[("other_signer", "ACTIVE")],
        signing: true,
        prefixed: true,
    },
    ActivationRow {
        case: "the shortest name allowed, left in ACTION_NEEDED beside an inactive key",
        subject: "ksk",
        created: "INACTIVE",
        forced: Some("ACTION_NEEDED"),
        // The zone holds the two keys `MAX_KEY_SIGNING_KEYS_PER_ZONE` allows,
        // the second named in the all-underscore form `KeySigningKey$Name`
        // permits.
        kept: &[("_____", "INACTIVE")],
        signing: false,
        prefixed: false,
    },
];

#[tokio::test]
async fn a_served_activation_moves_one_key_s_status_and_nothing_else() {
    for row in ACTIVATION_ROWS {
        let case = row.case;
        let (router, engine) = emulator_with_store();
        let zone_id = create_zone_ok(
            &router,
            "example.com",
            &format!("ref-activation-{}", row.subject),
        )
        .await;
        let arn = create_signing_kms_key(&router, case).await;
        create_ksk_ok(&router, case, &zone_id, row.subject, row.created, &arn).await;
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
        // After the enable: a zone holding a key left in error cannot be set
        // signing, so the status a failed request left is written once the zone
        // is in the state the row is about.
        if let Some(status) = row.forced {
            force_status(&engine, case, &zone_id, row.subject, status);
            assert_eq!(
                reported_status_messages(&router, &zone_id).await.len(),
                1,
                "case {case:?}: a key in {status} reports the status message it was left with"
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

        let activated = activate_ksk(&router, &addressed, row.subject).await;

        assert_eq!(
            activated.status, 200,
            "case {case:?}: expected 200 (501 = still scaffolded), got {} with body {}",
            activated.status, activated.body
        );
        assert_polls_a_change(case, &activated);

        let after = published_keys(&router, case, &zone_id).await;
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "case {case:?}: an activation neither adds nor removes a key"
        );
        let status = published_member(case, &after, row.subject, "Status");
        assert_eq!(
            status, "ACTIVE",
            "case {case:?}: the addressed key must end ACTIVE, got {status:?}"
        );
        for tracked in PERSISTED_MEMBERS
            .iter()
            .filter(|tracked| !MOVABLE_MEMBERS.contains(*tracked))
        {
            let was = published_member(case, &before, row.subject, tracked);
            let now = published_member(case, &after, row.subject, tracked);
            assert_eq!(
                was, now,
                "case {case:?}: an activation must leave the key's {tracked} as it was, got \
                 {now:?} after {was:?}"
            );
        }
        // ACTIVE is neither of the statuses a status message is reported for, so
        // a key this call moved out of one carries none afterwards.
        let messages = reported_status_messages(&router, &zone_id).await;
        assert!(
            messages.is_empty(),
            "case {case:?}: an activated key reports no StatusMessage, got {messages:?}"
        );

        // The key is re-dated only when the call actually moved its status: a
        // key that was already ACTIVE is reported exactly as it was.
        let dated_before = published_member(case, &before, row.subject, "LastModifiedDate");
        let dated_after = published_member(case, &after, row.subject, "LastModifiedDate");
        if row.created == "ACTIVE" && row.forced.is_none() {
            assert_eq!(
                dated_before, dated_after,
                "case {case:?}: a key that was already ACTIVE is not re-dated, got \
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
        // Nor did the zone's own signing state: putting a key into use is not an
        // enable or a disable.
        let read = get_dnssec(&router, &zone_id).await;
        let signature = member(case, &read, "Status/ServeSignature");
        let expected = if row.signing {
            "SIGNING"
        } else {
            "NOT_SIGNING"
        };
        assert_eq!(
            signature, expected,
            "case {case:?}: an activation must leave the zone's ServeSignature {expected}, got \
             {signature:?}"
        );

        // The call names an END state, so making it again is served and reports
        // exactly what the first one left — including the date it was moved at.
        let again = activate_ksk(&router, &addressed, row.subject).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: the same activation is served again, got {} with body {}",
            again.status, again.body
        );
        assert_polls_a_change(case, &again);
        let twice = published_keys(&router, case, &zone_id).await;
        assert_eq!(
            twice, after,
            "case {case:?}: a repeated activation changes nothing a client can read"
        );
    }
}

/// Send one `ActivateKeySigningKey` for `(zone_id, name)` per caller, each from
/// its own OS thread and all released from one barrier, and answer every reply.
///
/// Separate threads rather than tasks on one runtime, as the contention suite
/// beside this one does: what is under test is several callers making the same
/// status call at once, which only genuinely overlapping requests show.
fn activations_in_flight_together(
    router: &MultiServiceRouter,
    zone_id: &str,
    name: &str,
    callers: usize,
) -> Vec<Reply> {
    let barrier = Barrier::new(callers);
    std::thread::scope(|scope| {
        let racers: Vec<_> = (0..callers)
            .map(|_| {
                let router = router.clone();
                let barrier = &barrier;
                let zone_id = zone_id.to_string();
                let name = name.to_string();
                scope.spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("each caller must get a runtime of its own");
                    // Built BEFORE the barrier, so what is released at once is
                    // the request rather than the runtime's construction.
                    barrier.wait();
                    runtime.block_on(activate_ksk(&router, &zone_id, &name))
                })
            })
            .collect();
        racers
            .into_iter()
            .map(|racer| racer.join().expect("a caller must not panic"))
            .collect()
    })
}

#[tokio::test]
async fn concurrent_activations_of_one_key_are_all_served_and_leave_it_active() {
    // The caller count is what the rule generalises over: the call names an end
    // state rather than a transition, so however many callers ask for it at
    // once, each is served and the zone ends up holding ONE key of that name, in
    // that state.
    for callers in [2usize, 3, 5, 8] {
        let case = format!("{callers} callers activating one key at once");
        let router = emulator();
        let zone_id = create_zone_ok(&router, "example.com", &format!("ref-race-{callers}")).await;
        let arn = create_signing_kms_key(&router, &case).await;
        create_ksk_ok(&router, &case, &zone_id, "raced_key", "INACTIVE", &arn).await;

        let replies = activations_in_flight_together(&router, &zone_id, "raced_key", callers);

        for (index, reply) in replies.iter().enumerate() {
            assert_eq!(
                reply.status, 200,
                "case {case:?}: caller {index} expected HTTP 200, got {} with body {}",
                reply.status, reply.body
            );
            assert_polls_a_change(&case, reply);
        }
        // One key of that name, ACTIVE: no caller added a second entry and none
        // left it behind in the status it started in.
        let keys = published_keys(&router, &case, &zone_id).await;
        assert_eq!(
            keys.keys().collect::<Vec<_>>(),
            vec!["raced_key"],
            "case {case:?}: the zone must report exactly one key for the name"
        );
        let status = published_member(&case, &keys, "raced_key", "Status");
        assert_eq!(
            status, "ACTIVE",
            "case {case:?}: the raced key must converge to ACTIVE, got {status:?}"
        );
        // And the zone can be signed over it, which is what an ACTIVE key is
        // for.
        let enabled = enable_dnssec(&router, &zone_id).await;
        assert_eq!(
            enabled.status, 200,
            "case {case:?}: the activated key must be one the zone can sign over, got {} with \
             body {}",
            enabled.status, enabled.body
        );
    }
}

/// The message an activation of a key that is on its way out of the zone is
/// refused with: fixed text around the `Name` the request addressed and the
/// status that key stands in.
fn not_activatable_message(name: &str, status: &str) -> String {
    format!(
        "Key Signing Key with name {name} cannot be activated because current status is {status}."
    )
}

#[tokio::test]
async fn a_key_that_is_being_deleted_cannot_be_activated() {
    // "DELETING: The KSK is in the process of being deleted"
    // (`smithy_query(query="doc",shape="KeySigningKey",member="Status")`) — the
    // one status a key cannot be put into use from, the others being the two a
    // create accepts and the two the same member names this operation as the
    // correction for.
    for (name_case, name) in [
        ("a name of the usual form", "key_on_its_way_out"),
        ("the shortest name a key may carry", "abc"),
    ] {
        let case = format!("a key in DELETING under {name_case}");
        let (router, engine) = emulator_with_store();
        let zone_id = create_zone_ok(&router, "example.com", "ref-deleting").await;
        let arn = create_signing_kms_key(&router, &case).await;
        create_ksk_ok(&router, &case, &zone_id, name, "INACTIVE", &arn).await;
        force_status(&engine, &case, &zone_id, name, "DELETING");

        let refused = activate_ksk(&router, &zone_id, name).await;

        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "InvalidKeySigningKeyStatus",
            "case {case:?}: expected the InvalidKeySigningKeyStatus refusal, got body {}",
            refused.body
        );
        let message = message_of(&refused, &case);
        let expected = not_activatable_message(name, "DELETING");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // Nothing moved: the key is still the one it was refused for.
        assert_eq!(
            reported_status(&router, &zone_id, name).await,
            Some("DELETING".to_string()),
            "case {case:?}: a refused activation leaves the key in the status it was in"
        );
    }
}

/// How a row takes the customer managed key behind a key-signing key out of
/// service.
#[derive(Debug, Clone, Copy)]
enum UnusableKey {
    /// `KeySigningKey$KmsArn` requires "Status Enabled", so a disabled key is
    /// one this service cannot sign with.
    Disabled,
    /// "Check to make sure that the customer managed key has not been deleted"
    /// (`InvalidKMSArn`'s own refusal wording): a key awaiting deletion is no
    /// longer usable either.
    AwaitingDeletion,
}

#[tokio::test]
async fn a_key_whose_customer_managed_key_cannot_sign_cannot_be_activated() {
    for taken_out in [UnusableKey::Disabled, UnusableKey::AwaitingDeletion] {
        let case = format!("the customer managed key is {taken_out:?}");
        let router = emulator();
        let zone_id = create_zone_ok(&router, "example.com", "ref-unusable-key").await;
        let arn = create_signing_kms_key(&router, &case).await;
        create_ksk_ok(&router, &case, &zone_id, "orphaned_key", "INACTIVE", &arn).await;
        match taken_out {
            UnusableKey::Disabled => set_kms_key_enabled(&router, &case, &arn, false).await,
            UnusableKey::AwaitingDeletion => {
                schedule_kms_key_deletion(&router, &case, &arn).await
            }
        }

        let refused = activate_ksk(&router, &zone_id, "orphaned_key").await;

        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected 400, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code(),
            "InvalidKMSArn",
            "case {case:?}: expected the InvalidKMSArn refusal, got body {}",
            refused.body
        );
        let message = message_of(&refused, &case);
        let expected = unusable_key_message(&arn);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // Nothing moved, and the zone cannot be signed over the key either.
        assert_eq!(
            reported_status(&router, &zone_id, "orphaned_key").await,
            Some("INACTIVE".to_string()),
            "case {case:?}: a refused activation leaves the key INACTIVE"
        );

        // The guard is about the key as it stands NOW: putting the customer
        // managed key back in service makes the same activation serve.
        if let UnusableKey::Disabled = taken_out {
            set_kms_key_enabled(&router, &case, &arn, true).await;
            let activated = activate_ksk(&router, &zone_id, "orphaned_key").await;
            assert_eq!(
                activated.status, 200,
                "case {case:?}: the same activation is served once the key can sign again, got \
                 {} with body {}",
                activated.status, activated.body
            );
            assert_eq!(
                reported_status(&router, &zone_id, "orphaned_key").await,
                Some("ACTIVE".to_string()),
                "case {case:?}: the served activation leaves the key ACTIVE"
            );
        }
    }
}

#[tokio::test]
async fn activating_a_second_key_frees_the_one_a_signing_zone_was_holding() {
    // `KeySigningKeyInUse` names two ways out — "disable DNSSEC signing, or add
    // or enable another KSK" — and the sibling teardown suite owns the first; the
    // second is what an activation does, so the refusal follows the ACTIVE-key
    // count rather than the zone's signing state alone.
    let case = "a signing zone whose second key is activated";
    let router = emulator();
    let zone_id = create_zone_ok(&router, "example.com", "ref-freed-key").await;
    for (name, status) in [("first_signer", "ACTIVE"), ("second_signer", "INACTIVE")] {
        let arn = create_signing_kms_key(&router, case).await;
        create_ksk_ok(&router, case, &zone_id, name, status, &arn).await;
    }
    let enabled = enable_dnssec(&router, &zone_id).await;
    assert_eq!(
        enabled.status, 200,
        "case {case:?}: the zone must start signing, got {} with body {}",
        enabled.status, enabled.body
    );

    // While it is the zone's one ACTIVE key, it cannot be taken out of use.
    let refused = deactivate_ksk(&router, &zone_id, "first_signer").await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected 400, got {} with body {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_code(),
        "KeySigningKeyInUse",
        "case {case:?}: expected the KeySigningKeyInUse refusal, got body {}",
        refused.body
    );
    let message = message_of(&refused, case);
    let expected = in_use_message("first_signer");
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );

    // Activating the other key gives the zone a second key to sign with, which
    // is exactly what the refusal asked for.
    let activated = activate_ksk(&router, &zone_id, "second_signer").await;
    assert_eq!(
        activated.status, 200,
        "case {case:?}: the second key must activate, got {} with body {}",
        activated.status, activated.body
    );
    let freed = deactivate_ksk(&router, &zone_id, "first_signer").await;
    assert_eq!(
        freed.status, 200,
        "case {case:?}: the first key is no longer the only ACTIVE one, got {} with body {}",
        freed.status, freed.body
    );
    assert_eq!(
        reported_status(&router, &zone_id, "first_signer").await,
        Some("INACTIVE".to_string()),
        "case {case:?}: the freed key ends INACTIVE"
    );
    assert_eq!(
        reported_status(&router, &zone_id, "second_signer").await,
        Some("ACTIVE".to_string()),
        "case {case:?}: the key the zone now signs with stays ACTIVE"
    );
    let read = get_dnssec(&router, &zone_id).await;
    let signature = member(case, &read, "Status/ServeSignature");
    assert_eq!(
        signature, "SIGNING",
        "case {case:?}: the zone keeps signing throughout, got {signature:?}"
    );

    // And now the second key is the one the zone cannot be left without, so the
    // guard has MOVED with the ACTIVE-key count rather than staying on one key.
    let now_in_use = deactivate_ksk(&router, &zone_id, "second_signer").await;
    assert_eq!(
        now_in_use.error_code(),
        "KeySigningKeyInUse",
        "case {case:?}: the zone's one remaining ACTIVE key is now the guarded one, got body {}",
        now_in_use.body
    );
    let disabled = disable_dnssec(&router, &zone_id).await;
    assert_eq!(
        disabled.status, 200,
        "case {case:?}: the zone must stop signing, got {} with body {}",
        disabled.status, disabled.body
    );
    let last = deactivate_ksk(&router, &zone_id, "second_signer").await;
    assert_eq!(
        last.status, 200,
        "case {case:?}: a zone that is not signing may have every key taken out of use, got {} \
         with body {}",
        last.status, last.body
    );
}

/// One row of the two-zone addressing table: a name BOTH zones hold a key under.
struct SharedNameRow {
    case: &'static str,
    /// The name both zones' keys are created with.
    name: String,
    /// Whether the activation spells the addressed zone's id with the
    /// `/hostedzone/` prefix a response hands back (URI-encoded) rather than
    /// bare.
    prefixed: bool,
    /// Whether the two keys stand on ONE customer managed key — which a create
    /// allows across zones — rather than on one each.
    shared_kms: bool,
}

/// How long a `SigningKeyName` may be: `len 3..128`
/// (`smithy_query(query="shape",shape="SigningKeyName")`). The rows are checked
/// against this where they are used, so a row whose name left the member's range
/// fails as itself rather than as a request the boundary refused.
const NAME_LENGTHS: std::ops::RangeInclusive<usize> = 3..=128;

/// The rows span what the NAME half of the address can differ along — both
/// bounds of [`NAME_LENGTHS`] and the characters `KeySigningKey$Name` allows in
/// between — crossed with the two spellings of the zone id a request addresses
/// and whether the two keys publish one customer managed key or one each.
fn shared_name_rows() -> Vec<SharedNameRow> {
    vec![
        SharedNameRow {
            case: "the shortest name the member allows",
            name: "abc".to_string(),
            prefixed: false,
            shared_kms: false,
        },
        SharedNameRow {
            case: "a name over letters, digits and underscores",
            name: "K9_signer_2024".to_string(),
            prefixed: true,
            shared_kms: true,
        },
        SharedNameRow {
            case: "the longest name the member allows",
            // Built rather than written out, so the length is the member's bound
            // by construction.
            name: format!("ksk{}", "_".repeat(*NAME_LENGTHS.end() - 3)),
            prefixed: true,
            shared_kms: false,
        },
    ]
}

#[tokio::test]
async fn an_activation_moves_only_the_addressed_zone_s_key_of_that_name() {
    for row in shared_name_rows() {
        let case = row.case;
        let length = row.name.chars().count();
        assert!(
            NAME_LENGTHS.contains(&length),
            "case {case:?}: the row's name must be one SigningKeyName allows \
             ({NAME_LENGTHS:?}), got {length} character(s)"
        );
        let router = emulator();
        // Two zones, each holding a key of the SAME name and in the same status:
        // the zone half of the address is the only thing that tells them apart.
        let shared_arn = if row.shared_kms {
            Some(create_signing_kms_key(&router, case).await)
        } else {
            None
        };
        let mut zone_ids = Vec::new();
        for (index, apex) in ["example.com", "other.example.net"]
            .into_iter()
            .enumerate()
        {
            let zone_id =
                create_zone_ok(&router, apex, &format!("ref-shared-name-{length}-{index}")).await;
            let arn = match &shared_arn {
                Some(arn) => arn.clone(),
                None => create_signing_kms_key(&router, case).await,
            };
            // The caller reference is named here rather than composed from the
            // name: the longest name a key may carry leaves no room for one
            // built around it.
            let created = create_ksk_with_reference(
                &router,
                &format!("ref-ksk-{zone_id}"),
                &zone_id,
                &row.name,
                "INACTIVE",
                &arn,
            )
            .await;
            assert_eq!(
                created.status, 201,
                "case {case:?}: the key of zone {index} must be created, got {} with body {}",
                created.status, created.body
            );
            zone_ids.push(zone_id);
        }
        let addressed = &zone_ids[0];
        let other = &zone_ids[1];
        let requested = if row.prefixed {
            format!("%2Fhostedzone%2F{addressed}")
        } else {
            addressed.clone()
        };
        let other_before = published_keys(&router, case, other).await;

        let activated = activate_ksk(&router, &requested, &row.name).await;

        assert_eq!(
            activated.status, 200,
            "case {case:?}: expected 200 (501 = still scaffolded), got {} with body {}",
            activated.status, activated.body
        );
        assert_polls_a_change(case, &activated);

        let addressed_keys = published_keys(&router, case, addressed).await;
        assert_eq!(
            addressed_keys.keys().collect::<Vec<_>>(),
            vec![&row.name],
            "case {case:?}: the addressed zone must report exactly its one key of that name, got \
             {:?}",
            addressed_keys.keys().collect::<Vec<_>>()
        );
        let status = published_member(case, &addressed_keys, &row.name, "Status");
        assert_eq!(
            status, "ACTIVE",
            "case {case:?}: the addressed zone's key must end ACTIVE, got {status:?}"
        );

        // The other zone's key of the same name was not reached: every member it
        // publishes, its status and the instant it was last moved at included, is
        // what it was before the call.
        let other_after = published_keys(&router, case, other).await;
        assert_eq!(
            other_after, other_before,
            "case {case:?}: the identically named key of the other zone must be untouched, got \
             {other_after:?} after {other_before:?}"
        );
        let other_status = published_member(case, &other_after, &row.name, "Status");
        assert_eq!(
            other_status, "INACTIVE",
            "case {case:?}: the other zone's key stays in the status it was created in, got \
             {other_status:?}"
        );

        // And that key is reached through its OWN zone's address, which moves it
        // and leaves the first zone's key exactly as the first call left it.
        let second = activate_ksk(&router, other, &row.name).await;
        assert_eq!(
            second.status, 200,
            "case {case:?}: the other zone's key of the same name is addressable, got {} with \
             body {}",
            second.status, second.body
        );
        assert_polls_a_change(case, &second);
        let other_moved = published_keys(&router, case, other).await;
        let moved_status = published_member(case, &other_moved, &row.name, "Status");
        assert_eq!(
            moved_status, "ACTIVE",
            "case {case:?}: the other zone's key must end ACTIVE too, got {moved_status:?}"
        );
        let addressed_again = published_keys(&router, case, addressed).await;
        assert_eq!(
            addressed_again, addressed_keys,
            "case {case:?}: an activation addressed at another zone changes nothing a client \
             reads of this zone's key, got {addressed_again:?} after {addressed_keys:?}"
        );
    }
}
