// SPDX-License-Identifier: CC0-1.0

//! What a web app customization write that LOSES the race is told, end to end over HTTP.
//!
//! RULE under test: `UpdateWebAppCustomization` and `DeleteWebAppCustomization` both write the
//! single customization object a web app holds, and the service applies one such write at a
//! time, so a write that overlaps another is refused with the modelled `ConflictException`
//! (HTTP 409 for each,
//! `smithy_query(query="operation",operation="UpdateWebAppCustomization,DeleteWebAppCustomization")`).
//! That refusal carries the service's own FIXED sentence for the write it could not carry
//! out — "Unable to update WebApp customization" for an update, "Unable to delete WebApp
//! customization" for a delete — so the sentence is decided by the operation that lost and by
//! nothing else: no web app id, no title, no logo, nothing from the request or from the store
//! reaches it, and two web apps' losers read the same.
//!
//! (The guard itself — exactly one winner, the surviving customization being the winner's,
//! non-overlapping writes all applied — is [`crate::web_app_customization_write_guard`]'s
//! rule; this suite is about the WORDING every refusal it produces carries.)
//!
//! The rows differ along what the rule generalises over: which operation loses, which pair of
//! operations race (two updates, two deletes, an update against a delete in both orders, and
//! the two kinds mixed), how many writers race at once, what the losing request carried
//! (a title, a logo, nothing at all), whether the web app held a customization already, and
//! which of two web apps the race is on. The last suite races the two kinds TOGETHER at
//! several counts of each, where both kinds lose in one race, and also holds the race to what
//! it leaves behind: the winner's whole outcome, its members or no customization at all.
//!
//! Overlap is produced the way the guard's own suite produces it, with the helpers it owns.

use serde_json::{json, Value};

use crate::harness::{member_of, Answer, Emulator};
use crate::web_app_customization_write_guard::{create_web_app, futures_of};

/// The sentence a losing `UpdateWebAppCustomization` is refused with, spelled out as a
/// literal rather than read off the crate's producer, so the WORDING is what these rows are
/// held to.
const UPDATE_CONFLICT: &str = "Unable to update WebApp customization";

/// The sentence a losing `DeleteWebAppCustomization` is refused with, for the same reason.
const DELETE_CONFLICT: &str = "Unable to delete WebApp customization";

/// The sentence the service refuses a lost write of `op` with.
fn conflict_sentence(op: &str) -> &'static str {
    match op {
        "UpdateWebAppCustomization" => UPDATE_CONFLICT,
        "DeleteWebAppCustomization" => DELETE_CONFLICT,
        other => panic!("{other} does not write a web app's customization"),
    }
}

/// Assert `answer` is the modelled conflict for a lost write of `op`, carrying that
/// operation's own sentence and nothing the request submitted.
fn assert_conflict_wording(case: &str, op: &str, answer: &Answer, submitted: &[&str]) {
    assert_eq!(
        answer.status, 409,
        "case {case:?} ({op} lost): expected HTTP 409, got {} with body {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        "ConflictException",
        "case {case:?} ({op} lost): expected ConflictException, got body {}",
        answer.body
    );
    let message = member_of(&answer.body, "Message")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let expected = conflict_sentence(op);
    assert_eq!(
        message, expected,
        "case {case:?} ({op} lost): expected {expected:?}, got {message:?}"
    );
    for value in submitted {
        assert!(
            !message.contains(value),
            "case {case:?} ({op} lost): the refusal must carry nothing the request \
             submitted, got {message:?} for {value:?}"
        );
    }
}

/// One racing writer: the operation and the request members it carries.
type Call = (&'static str, Value);

/// RULE: of writes of one web app's customization that overlap, every loser is refused with
/// the sentence for the operation IT could not carry out.
#[tokio::test]
async fn every_losing_customization_write_carries_its_own_operation_sentence() {
    // (case, whether a customization is seeded first, the writers that race)
    let rows: Vec<(&str, bool, Vec<Call>)> = vec![
        (
            "two updates with different titles",
            true,
            vec![
                ("UpdateWebAppCustomization", json!({"Title": "first"})),
                ("UpdateWebAppCustomization", json!({"Title": "second"})),
            ],
        ),
        (
            "two deletes",
            true,
            vec![
                ("DeleteWebAppCustomization", json!({})),
                ("DeleteWebAppCustomization", json!({})),
            ],
        ),
        (
            "an update against a delete",
            true,
            vec![
                ("UpdateWebAppCustomization", json!({"Title": "assigned"})),
                ("DeleteWebAppCustomization", json!({})),
            ],
        ),
        (
            "a delete against an update",
            true,
            vec![
                ("DeleteWebAppCustomization", json!({})),
                ("UpdateWebAppCustomization", json!({"Title": "assigned"})),
            ],
        ),
        (
            "a logo update against a delete, on a web app with no customization yet",
            false,
            vec![
                (
                    "UpdateWebAppCustomization",
                    json!({"LogoFile": aws_smithy_types::base64::encode(b"logo")}),
                ),
                ("DeleteWebAppCustomization", json!({})),
            ],
        ),
        (
            "five overlapping updates",
            true,
            vec![
                ("UpdateWebAppCustomization", json!({"Title": "one"})),
                ("UpdateWebAppCustomization", json!({"Title": "two"})),
                ("UpdateWebAppCustomization", json!({"Title": "three"})),
                ("UpdateWebAppCustomization", json!({"Title": "four"})),
                ("UpdateWebAppCustomization", json!({"Title": "five"})),
            ],
        ),
        (
            "three overlapping deletes",
            true,
            vec![
                ("DeleteWebAppCustomization", json!({})),
                ("DeleteWebAppCustomization", json!({})),
                ("DeleteWebAppCustomization", json!({})),
            ],
        ),
        (
            "two updates and two deletes at once",
            true,
            vec![
                ("UpdateWebAppCustomization", json!({"Title": "mixed-one"})),
                ("DeleteWebAppCustomization", json!({})),
                ("UpdateWebAppCustomization", json!({"Title": "mixed-two"})),
                ("DeleteWebAppCustomization", json!({})),
            ],
        ),
    ];

    for (case, seed, writers) in rows {
        let emulator = Emulator::new();
        // A SECOND web app, created first, so the id under test is not the only one the
        // store holds: a refusal that named the web app it was about would differ between
        // the two, and these rows would catch it.
        let other = create_web_app(&emulator).await;
        let web_app_id = create_web_app(&emulator).await;
        assert_ne!(
            other, web_app_id,
            "case {case:?}: the two web apps must have different ids"
        );
        if seed {
            let seeded = emulator
                .call(
                    "UpdateWebAppCustomization",
                    json!({"WebAppId": &web_app_id, "Title": "seeded"}),
                )
                .await;
            assert_eq!(
                seeded.status, 200,
                "case {case:?}: expected the seeding update to be served, got {} with body {}",
                seeded.status, seeded.body
            );
        }

        let calls = writers.iter().map(|(op, members)| {
            let mut request = members.clone();
            request["WebAppId"] = json!(&web_app_id);
            emulator.call(op, request)
        });
        let answers = futures_of(calls).await;

        let served = answers
            .iter()
            .filter(|answer| answer.status == 200)
            .count();
        assert_eq!(
            served,
            1,
            "case {case:?}: expected exactly one winning write, got the statuses {:?}",
            answers.iter().map(|answer| answer.status).collect::<Vec<_>>()
        );

        // Every value the losing requests submitted, which no refusal may carry.
        let mut submitted: Vec<&str> = vec![web_app_id.as_str()];
        for (_, members) in &writers {
            if let Some(title) = member_of(members, "Title").as_str() {
                submitted.push(title);
            }
        }
        let losers = answers
            .iter()
            .zip(&writers)
            .filter(|(answer, _)| answer.status != 200);
        let mut refused = 0;
        for (answer, (op, _)) in losers {
            assert_conflict_wording(case, op, answer, &submitted);
            refused += 1;
        }
        assert_eq!(
            refused,
            writers.len() - 1,
            "case {case:?}: every writer but the winner must be refused"
        );
    }
}

/// Assert what `web_app_id` holds once a race is over: the winning update's `Title`, or no
/// customization at all when the delete won — which `DescribeWebAppCustomization` reports
/// as the modelled `ResourceNotFoundException` (HTTP 404) for a web app that holds none.
async fn assert_surviving_customization(
    emulator: &Emulator,
    case: &str,
    web_app_id: &str,
    expected_title: Option<&str>,
) {
    let answer = emulator
        .call("DescribeWebAppCustomization", json!({"WebAppId": web_app_id}))
        .await;
    match expected_title {
        Some(title) => {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the winning update's customization to be readable, \
                 got {} with body {}",
                answer.status, answer.body
            );
            let stored = member_of(member_of(&answer.body, "WebAppCustomization"), "Title")
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                stored, title,
                "case {case:?}: expected the stored title to be the winner's {title:?}, got \
                 {stored:?}"
            );
        }
        None => {
            assert_eq!(
                answer.status, 404,
                "case {case:?}: the winning delete must leave no customization, got {} with \
                 body {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException for a web app that holds no \
                 customization, got body {}",
                answer.body
            );
        }
    }
}

/// RULE, at the scale a client racing from a thread pool produces, with BOTH operations
/// losing in the same race: of writes of one web app's customization that overlap, every
/// loser is refused with the sentence for the operation IT could not carry out — so a
/// losing delete reads "Unable to delete WebApp customization" even while updates of the
/// same object are in flight — and what survives is the winner's WHOLE outcome, its
/// members or no customization, never a mixture.
///
/// The rows differ along how many writers of each kind race at once and hence along which
/// kind is more likely to win, each row interleaving the two kinds.
#[tokio::test]
async fn mixed_customization_races_refuse_each_loser_with_its_own_sentence() {
    // (updates, deletes) racing at once.
    for (updates, deletes) in [(4usize, 2usize), (2, 4), (1, 1), (3, 3), (5, 1)] {
        let case = format!("{updates} updates and {deletes} deletes at once");
        let emulator = Emulator::new();
        let web_app_id = create_web_app(&emulator).await;
        // A customization is in place first, so a losing delete cannot be mistaken for
        // "there was nothing to remove".
        let seeded = emulator
            .call(
                "UpdateWebAppCustomization",
                json!({"WebAppId": &web_app_id, "Title": "seeded"}),
            )
            .await;
        assert_eq!(
            seeded.status, 200,
            "case {case:?}: expected the seeding update to be served, got {} with body {}",
            seeded.status, seeded.body
        );

        // The two kinds interleaved, so neither kind is always the one that arrives first.
        let mut writers: Vec<Call> = Vec::new();
        let mut made_updates = 0usize;
        let mut made_deletes = 0usize;
        while made_updates < updates || made_deletes < deletes {
            if made_updates < updates {
                writers.push((
                    "UpdateWebAppCustomization",
                    json!({"Title": format!("racer-{made_updates}")}),
                ));
                made_updates += 1;
            }
            if made_deletes < deletes {
                writers.push(("DeleteWebAppCustomization", json!({})));
                made_deletes += 1;
            }
        }

        let calls = writers.iter().map(|(op, members)| {
            let mut request = members.clone();
            request["WebAppId"] = json!(&web_app_id);
            emulator.call(op, request)
        });
        let answers = futures_of(calls).await;

        let winners: Vec<usize> = answers
            .iter()
            .enumerate()
            .filter(|(_, answer)| answer.status == 200)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "case {case:?}: expected exactly one winning write, got the statuses {:?}",
            answers.iter().map(|answer| answer.status).collect::<Vec<_>>()
        );

        // Every value a racing request submitted, which no refusal may carry.
        let mut submitted: Vec<&str> = vec![web_app_id.as_str(), "seeded"];
        for (_, members) in &writers {
            if let Some(title) = member_of(members, "Title").as_str() {
                submitted.push(title);
            }
        }
        for (index, (answer, (op, _))) in answers.iter().zip(&writers).enumerate() {
            if index == winners[0] {
                continue;
            }
            assert_conflict_wording(
                &format!("{case} (writer {index})"),
                op,
                answer,
                &submitted,
            );
        }

        let (winning_op, winning_members) = &writers[winners[0]];
        let expected_title = if *winning_op == "DeleteWebAppCustomization" {
            None
        } else {
            member_of(winning_members, "Title").as_str()
        };
        assert_surviving_customization(&emulator, &case, &web_app_id, expected_title).await;
    }
}
