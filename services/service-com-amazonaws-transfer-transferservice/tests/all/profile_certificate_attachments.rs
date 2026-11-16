// SPDX-License-Identifier: CC0-1.0

//! What a profile WRITE establishes about an AS2 profile's certificate attachments, and
//! what the profile read then reports.
//!
//! RULE under test: a write to a stored profile that NAMES `CertificateIds` replaces
//! that profile's whole attachment list with the ids it carries, and a write that leaves
//! the member out names no new value at all, so every later `DescribeProfile` of that
//! profile reports exactly the ids its last write established, in the order that write
//! carried them — a profile the store does not hold is refused by the write and nothing
//! is stored for it.
//!
//! The rows differ along what the rule generalises over: the COUNT of attached ids
//! before and after the write (none, one, several), the DIRECTION of the replacement
//! (attach, grow, shrink, reorder, detach every one), the id VALUES across the modelled
//! `CertificateId` space, whether the write names the member at all, and whether the ids
//! are catalog ids or ones this service MINTED for a real imported certificate.

use serde_json::json;

use crate::harness::{
    is_modelled_certificate_id, member_of, Answer, Emulator, CERTIFICATE_IDS, PROFILE_TYPES,
};

/// Further identifiers satisfying `CertificateIds`' member shape `CertificateId`
/// (`len 22..22`, pattern `^cert-([0-9a-f]{17})$`, from
/// `smithy_query(query="shape",shape="CertificateIds")`), so a row can replace one set
/// of attachments with a disjoint one. The harness holds the low, mid and high ends of
/// the id space; these sit between them.
const MORE_CERTIFICATE_IDS: [&str; 3] = [
    "cert-00000000000000001",
    "cert-0a1b2c3d4e5f60718",
    "cert-fedcba98765432100",
];

/// Create one profile and hand back its `ProfileId`. `attached` is what the CREATE
/// carries; an empty list sends no `CertificateIds` member at all, so the row starts
/// from a profile with no attachments.
async fn profile_with(emulator: &Emulator, profile_type: &str, attached: &[&str]) -> String {
    let mut request = json!({"As2Id": "attachment-party", "ProfileType": profile_type});
    if !attached.is_empty() {
        request
            .as_object_mut()
            .expect("the request body is an object")
            .insert("CertificateIds".to_string(), json!(attached));
    }
    emulator.create_profile(request).await
}

/// Drive one `UpdateProfile` and hand back the answer. `attached` of `None` sends no
/// `CertificateIds` member.
async fn update(emulator: &Emulator, profile_id: &str, attached: Option<&[&str]>) -> Answer {
    let mut request = json!({"ProfileId": profile_id});
    if let Some(attached) = attached {
        request
            .as_object_mut()
            .expect("the request body is an object")
            .insert("CertificateIds".to_string(), json!(attached));
    }
    emulator.call("UpdateProfile", request).await
}

/// `DescribeProfile`'s `Profile.CertificateIds`, as the list of ids it reports.
async fn attachments_of(emulator: &Emulator, case: &str, profile_id: &str) -> Vec<String> {
    let answer = emulator
        .call("DescribeProfile", json!({"ProfileId": profile_id}))
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected DescribeProfile 200, got {} with body {}",
        answer.status, answer.body
    );
    let reported = member_of(answer.get("Profile"), "CertificateIds").clone();
    let ids = reported.as_array().unwrap_or_else(|| {
        panic!("case {case:?}: expected CertificateIds to be a list, got {reported:?}")
    });
    ids.iter()
        .map(|id| {
            id.as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected a string CertificateId, got {id:?}")
                })
                .to_string()
        })
        .collect()
}

/// One replacement: the attachments the profile is created with, what the update names,
/// and the list every read after it must report.
struct Case {
    /// What this row varies, named in every assertion it makes.
    name: &'static str,
    created: Vec<&'static str>,
    /// What the update carries; `None` sends no `CertificateIds` member.
    updated: Option<Vec<&'static str>>,
    expected: Vec<&'static str>,
}

/// The table: every row differs in the count, the order or the identity of the ids on
/// either side of the write, or in whether the write names the member at all.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "attaching one certificate to a profile created with none",
            created: Vec::new(),
            updated: Some(vec![CERTIFICATE_IDS[1]]),
            expected: vec![CERTIFICATE_IDS[1]],
        },
        Case {
            name: "attaching three certificates to a profile created with none",
            created: Vec::new(),
            updated: Some(CERTIFICATE_IDS.to_vec()),
            expected: CERTIFICATE_IDS.to_vec(),
        },
        Case {
            name: "growing one attachment to three",
            created: vec![CERTIFICATE_IDS[0]],
            updated: Some(vec![
                CERTIFICATE_IDS[0],
                MORE_CERTIFICATE_IDS[1],
                MORE_CERTIFICATE_IDS[2],
            ]),
            expected: vec![
                CERTIFICATE_IDS[0],
                MORE_CERTIFICATE_IDS[1],
                MORE_CERTIFICATE_IDS[2],
            ],
        },
        Case {
            name: "shrinking three attachments to one",
            created: CERTIFICATE_IDS.to_vec(),
            updated: Some(vec![CERTIFICATE_IDS[2]]),
            expected: vec![CERTIFICATE_IDS[2]],
        },
        Case {
            name: "replacing the attachments with a disjoint set",
            created: CERTIFICATE_IDS.to_vec(),
            updated: Some(MORE_CERTIFICATE_IDS.to_vec()),
            expected: MORE_CERTIFICATE_IDS.to_vec(),
        },
        Case {
            name: "reordering the attached ids",
            created: vec![CERTIFICATE_IDS[0], CERTIFICATE_IDS[1]],
            updated: Some(vec![CERTIFICATE_IDS[1], CERTIFICATE_IDS[0]]),
            expected: vec![CERTIFICATE_IDS[1], CERTIFICATE_IDS[0]],
        },
        Case {
            name: "detaching every certificate with an empty list",
            created: CERTIFICATE_IDS.to_vec(),
            updated: Some(Vec::new()),
            expected: Vec::new(),
        },
        Case {
            name: "an update that names no CertificateIds keeps three attachments",
            created: CERTIFICATE_IDS.to_vec(),
            updated: None,
            expected: CERTIFICATE_IDS.to_vec(),
        },
        Case {
            name: "an update that names no CertificateIds leaves a profile with none empty",
            created: Vec::new(),
            updated: None,
            expected: Vec::new(),
        },
    ]
}

/// RULE: the attachment list a profile write establishes is what every later read of
/// that profile reports.
#[tokio::test]
async fn a_profile_write_establishes_what_the_profile_read_reports() {
    let emulator = Emulator::new();

    for (row, case) in cases().into_iter().enumerate() {
        // Alternate the profile type across the rows: the attachment rule is the same
        // for a local and a partner profile.
        let profile_type = PROFILE_TYPES[row % PROFILE_TYPES.len()];
        let profile_id = profile_with(&emulator, profile_type, &case.created).await;

        let answer = update(
            &emulator,
            &profile_id,
            case.updated.as_deref(),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {:?}: expected UpdateProfile 200, got {} with body {}",
            case.name, answer.status, answer.body
        );
        let returned = answer.get("ProfileId").as_str().unwrap_or_default();
        assert_eq!(
            returned, profile_id,
            "case {:?}: expected UpdateProfile to return ProfileId {profile_id:?}, got {returned:?}",
            case.name
        );

        let reported = attachments_of(&emulator, case.name, &profile_id).await;
        assert_eq!(
            reported, case.expected,
            "case {:?}: expected CertificateIds {:?}, got {reported:?}",
            case.name, case.expected
        );

        // A read may not change what a later caller sees: the same describe answers the
        // same attachments.
        let again = attachments_of(&emulator, case.name, &profile_id).await;
        assert_eq!(
            again, reported,
            "case {:?}: expected a repeated read to report {reported:?}, got {again:?}",
            case.name
        );

        // The write touches only the member it names: the rest of the profile is the
        // record the create left.
        let described = emulator
            .call("DescribeProfile", json!({"ProfileId": &profile_id}))
            .await;
        let profile = described.get("Profile");
        let as2_id = member_of(profile, "As2Id").as_str().unwrap_or_default();
        assert_eq!(
            as2_id, "attachment-party",
            "case {:?}: expected the stored As2Id to survive the update, got {as2_id:?}",
            case.name
        );
        let reported_type = member_of(profile, "ProfileType").as_str().unwrap_or_default();
        assert_eq!(
            reported_type, profile_type,
            "case {:?}: expected the stored ProfileType to survive the update, got {reported_type:?}",
            case.name
        );
    }
}

/// RULE (successive writes): each write replaces the attachments the one before it
/// established, so a read reports the LAST write's list and not the union of them.
#[tokio::test]
async fn successive_writes_leave_the_last_ones_attachments() {
    let emulator = Emulator::new();
    let case = "three successive attachment writes";

    let profile_id = profile_with(&emulator, PROFILE_TYPES[0], &[CERTIFICATE_IDS[0]]).await;

    // Each step names what the profile must report after it: an attach, a replacement,
    // an omitted member (which changes nothing) and a detach.
    let steps: [(Option<Vec<&str>>, Vec<&str>); 4] = [
        (
            Some(vec![CERTIFICATE_IDS[0], MORE_CERTIFICATE_IDS[0]]),
            vec![CERTIFICATE_IDS[0], MORE_CERTIFICATE_IDS[0]],
        ),
        (
            Some(vec![MORE_CERTIFICATE_IDS[2]]),
            vec![MORE_CERTIFICATE_IDS[2]],
        ),
        (None, vec![MORE_CERTIFICATE_IDS[2]]),
        (Some(Vec::new()), Vec::new()),
    ];

    for (step, (written, expected)) in steps.iter().enumerate() {
        let answer = update(
            &emulator,
            &profile_id,
            written.as_deref(),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?} step {step}: expected UpdateProfile 200, got {} with body {}",
            answer.status, answer.body
        );
        let reported = attachments_of(&emulator, case, &profile_id).await;
        assert_eq!(
            &reported, expected,
            "case {case:?} step {step}: expected CertificateIds {expected:?}, got {reported:?}"
        );
    }
}

/// RULE applied to the ids this service MINTS: a certificate imported through
/// `ImportCertificate` and then attached to a profile is reported by that profile's
/// read, under the id the import returned.
#[tokio::test]
async fn an_imported_certificate_attached_to_a_profile_is_reported_by_the_profile_read() {
    let emulator = Emulator::new();
    let case = "an imported certificate attached to a profile";

    // Two real certificates, generated here rather than pasted, so the ids under test
    // are the ones the service minted for material it actually read.
    let mut minted: Vec<String> = Vec::new();
    for usage in ["SIGNING", "ENCRYPTION"] {
        let key = rcgen::KeyPair::generate().expect("a key pair");
        let mut params =
            rcgen::CertificateParams::new(vec!["as2.example.com".to_string()]).expect("parameters");
        params.not_before = rcgen::date_time_ymd(2020, 1, 1);
        params.not_after = rcgen::date_time_ymd(2100, 1, 1);
        let certificate = params
            .self_signed(&key)
            .expect("a self-signed certificate")
            .pem();
        let answer = emulator
            .call(
                "ImportCertificate",
                json!({"Usage": usage, "Certificate": certificate}),
            )
            .await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected ImportCertificate 200 for {usage}, got {} with body {}",
            answer.status, answer.body
        );
        let certificate_id = answer
            .get("CertificateId")
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: ImportCertificate returned no CertificateId: {}", answer.body)
            })
            .to_string();
        assert!(
            is_modelled_certificate_id(&certificate_id),
            "case {case:?}: expected a modelled CertificateId, got {certificate_id:?}"
        );
        minted.push(certificate_id);
    }

    let profile_id = profile_with(&emulator, PROFILE_TYPES[1], &[]).await;
    let first: Vec<&str> = vec![minted[0].as_str()];
    let answer = update(&emulator, &profile_id, Some(&first)).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected UpdateProfile 200, got {} with body {}",
        answer.status, answer.body
    );
    let reported = attachments_of(&emulator, case, &profile_id).await;
    assert_eq!(
        reported,
        vec![minted[0].clone()],
        "case {case:?}: expected the imported certificate id to be attached, got {reported:?}"
    );

    // Attaching the second one beside the first is the same rule: the list the write
    // names is what the read reports.
    let both: Vec<&str> = minted.iter().map(String::as_str).collect();
    let answer = update(&emulator, &profile_id, Some(&both)).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected the second UpdateProfile 200, got {} with body {}",
        answer.status, answer.body
    );
    let reported = attachments_of(&emulator, case, &profile_id).await;
    assert_eq!(
        reported, minted,
        "case {case:?}: expected both imported certificate ids attached, got {reported:?}"
    );
}

/// RULE (write scope): one write addresses ONE profile, so the attachments of every
/// other stored profile are untouched.
#[tokio::test]
async fn a_write_changes_only_the_addressed_profile() {
    let emulator = Emulator::new();
    let case = "one write beside two untouched profiles";

    let written = profile_with(&emulator, PROFILE_TYPES[0], &[CERTIFICATE_IDS[0]]).await;
    let bystanders = [
        profile_with(&emulator, PROFILE_TYPES[0], &[]).await,
        profile_with(&emulator, PROFILE_TYPES[1], &CERTIFICATE_IDS).await,
    ];
    let bystander_attachments = [Vec::new(), CERTIFICATE_IDS.to_vec()];

    let answer = update(&emulator, &written, Some(&MORE_CERTIFICATE_IDS)).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected UpdateProfile 200, got {} with body {}",
        answer.status, answer.body
    );

    let reported = attachments_of(&emulator, case, &written).await;
    assert_eq!(
        reported,
        MORE_CERTIFICATE_IDS.to_vec(),
        "case {case:?}: expected the written profile to report {MORE_CERTIFICATE_IDS:?}, got {reported:?}"
    );

    for (profile_id, expected) in bystanders.iter().zip(bystander_attachments.iter()) {
        let reported = attachments_of(&emulator, case, profile_id).await;
        assert_eq!(
            &reported, expected,
            "case {case:?}: expected bystander {profile_id} to still report {expected:?}, got {reported:?}"
        );
    }
}

/// RULE (not found): a well-formed `ProfileId` the store does not hold is refused with
/// the service's not-found error, and nothing is stored for it — a later read of that id
/// is still a refusal.
#[tokio::test]
async fn writing_a_profile_the_store_does_not_hold_stores_nothing() {
    let emulator = Emulator::new();

    // Syntactically valid `ProfileId`s (`^p-([0-9a-f]{17})$`) that no create minted: the
    // low end of the id space, a mid value and the high end.
    const NEVER_MINTED: [&str; 3] = [
        "p-00000000000000000",
        "p-0000000000000dead",
        "p-fffffffffffffffff",
    ];

    // One profile exists throughout, so the refusal is about the addressed id and not
    // about an empty store.
    let existing = profile_with(&emulator, PROFILE_TYPES[0], &[CERTIFICATE_IDS[1]]).await;

    for absent in NEVER_MINTED {
        for attached in [None, Some(CERTIFICATE_IDS.to_vec())] {
            let case = format!(
                "UpdateProfile for {absent} with CertificateIds {}",
                if attached.is_some() { "named" } else { "absent" }
            );
            let answer = update(
                &emulator,
                absent,
                attached.as_deref(),
            )
            .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400, got {} with body {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException, got {}",
                answer.body
            );
            let resource_type = member_of(&answer.body, "ResourceType")
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                resource_type, "Profile",
                "case {case:?}: expected ResourceType \"Profile\", got {resource_type:?}"
            );
            let resource = member_of(&answer.body, "Resource")
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                resource, absent,
                "case {case:?}: expected the requested id in Resource, got {resource:?}"
            );
            let message = member_of(&answer.body, "Message")
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(
                !message.is_empty(),
                "case {case:?}: Message is modelled required, got body {}",
                answer.body
            );
            assert!(
                !message.contains(absent),
                "case {case:?}: expected no identifier in Message, got {message:?}"
            );

            // The refused write stored nothing: the id is still unknown to the read.
            let described = emulator
                .call("DescribeProfile", json!({"ProfileId": absent}))
                .await;
            assert_eq!(
                described.error_code(),
                "ResourceNotFoundException",
                "case {case:?}: expected the refused id to remain unknown, got {}",
                described.body
            );
        }
    }

    // The profile that does exist kept the attachments its create established.
    let reported = attachments_of(&emulator, "the existing profile", &existing).await;
    assert_eq!(
        reported,
        vec![CERTIFICATE_IDS[1].to_string()],
        "expected the existing profile to still report {:?}, got {reported:?}",
        [CERTIFICATE_IDS[1]]
    );
}

/// The modelled constraints on `UpdateProfile`'s own input are refused before any state
/// is reached: a `ProfileId` outside `len 19..19` / `^p-([0-9a-f]{17})$` is a bad
/// request, not a not-found.
#[tokio::test]
async fn a_malformed_profile_id_is_refused_as_a_bad_request() {
    let emulator = Emulator::new();

    // Rows: too short, too long, the wrong prefix, and upper-case hex the pattern
    // excludes.
    for malformed in [
        "p-0000000000000000",
        "p-000000000000000000",
        "s-00000000000000000",
        "p-0000000000000DEAD",
    ] {
        let answer = update(&emulator, malformed, Some(&[CERTIFICATE_IDS[0]])).await;
        assert_eq!(
            answer.status, 400,
            "case {malformed:?}: expected HTTP 400, got {} with body {}",
            answer.status, answer.body
        );
        let code = answer.error_code();
        assert_ne!(
            code, "ResourceNotFoundException",
            "case {malformed:?}: expected a validation refusal rather than not-found, got {}",
            answer.body
        );
    }
}

/// `UpdateProfile` is a write, so what it reports is what the store holds: the response
/// carries the addressed id and nothing derived from the request's list.
#[tokio::test]
async fn the_write_answers_the_addressed_profile_id_only() {
    let emulator = Emulator::new();

    for attached in [
        None,
        Some(Vec::new()),
        Some(vec![CERTIFICATE_IDS[0]]),
        Some(MORE_CERTIFICATE_IDS.to_vec()),
    ] {
        let profile_id = profile_with(&emulator, PROFILE_TYPES[0], &[]).await;
        let answer = update(
            &emulator,
            &profile_id,
            attached.as_deref(),
        )
        .await;
        let case = format!("UpdateProfile carrying {attached:?}");
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {}",
            answer.status, answer.body
        );
        let body = answer
            .body
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected an object body, got {}", answer.body));
        let members: Vec<&String> = body.keys().collect();
        assert_eq!(
            members,
            vec![&"ProfileId".to_string()],
            "case {case:?}: expected ProfileId as the only output member, got {members:?}"
        );
        let returned = member_of(&answer.body, "ProfileId").as_str().unwrap_or_default();
        assert_eq!(
            returned, profile_id,
            "case {case:?}: expected ProfileId {profile_id:?}, got {returned:?}"
        );
    }
}
