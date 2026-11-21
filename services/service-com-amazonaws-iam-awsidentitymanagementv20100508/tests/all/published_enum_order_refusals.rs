// SPDX-License-Identifier: CC0-1.0

//! The ORDER a refusal states an admissible value set in, end to end over the awsQuery wire,
//! and what such a refusal leaves behind.
//!
//! The rules under test:
//!
//! 1. a request naming a value outside the set its member's enum declares is refused HTTP
//!    400 `ValidationError` whose `Message` is `1 validation error detected: Value at
//!    '<member>' failed to satisfy constraint: Member must satisfy enum value set:
//!    [<the admissible values>]` — the member in its wire (lower-camel) form, the set the one
//!    the targeted ENUM declares written in the order this service publishes for that enum,
//!    and nothing of the value the request carried. The order belongs to the enum, so every
//!    member targeting it, in any operation, is stated the same way, and a set this service
//!    publishes no order of its own for keeps the one the model declares.
//! 2. that refusal is reached before anything is written, so a resource the account holds is
//!    left exactly as it was held — and every value the set DOES declare is taken and takes
//!    effect, so the value-free clause is not a blanket refusal of the member.
//!
//! The rows differ along what the rules generalise over: which enum the member targets
//! (`statusType`, whose published order is not the model's declaration order, and
//! `encodingType`, whose published order is), which operation and resource the member is
//! reached through (an SSH public key's status, an access key's status, a key read's
//! encoding), how many values the set declares, the FORM of the rejected value (a name no
//! enum of this service declares, a plausible name AWS does not, an admissible value in the
//! wrong case, an admissible value of a different enum, an admissible value with trailing
//! space, an empty value), and the value the resource carried when the refusal was made.
//! No single request is the specification.
//!
//! `tests/all/enum_refusal_wording.rs` states the same clause over the breadth of this
//! service's enum-bearing members, every one of them refused before dispatch; this suite is
//! about a refusal reached on a resource the account HOLDS, and what the held resource looks
//! like afterwards.
//!
//! The clause and the published order of each set are that module's too, imported rather
//! than restated, so one statement of each exists in this crate; the set a refusal states is
//! read back OFF THE WIRE here and compared, as a set, with the values the model declares
//! (`ops::types::STATUS_TYPE_VALUES`, `ENCODING_TYPE_VALUES`), so an order can neither drop a
//! value the enum declares nor name one it does not.
//!
//! The material fixtures, the upload and the user fixture are the ones
//! `tests/all/ssh_public_keys.rs` owns, the read `tests/all/ssh_public_key_reads.rs`'s, and
//! the awsQuery harness `tests/all/user_lifecycle.rs`'s.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::{
    ENCODING_TYPE_VALUES, STATUS_TYPE_VALUES,
};

// The two admissible sets under test and the clause they are stated in are the ones
// `tests/all/enum_refusal_wording.rs` owns, so one statement of each published order and one
// statement of the clause exist in this crate: `CREDENTIAL_STATUS` is `iam#statusType` in the
// order AWS states it (not the order the model declares), `KEY_ENCODING` is `iam#encodingType`,
// whose published order IS the model's.
use crate::enum_refusal_wording::{expected_message, CREDENTIAL_STATUS, KEY_ENCODING};
use crate::ssh_public_key_reads::get;
use crate::ssh_public_keys::{create_user, modulus, openssh_line, upload, PUBLISHED_MINIMUM_BITS};
use crate::user_lifecycle::{call, element, elements, error_code, router};

/// The status a credential carries from the moment it is minted: `CreateAccessKey` documents
/// "the default status for new keys is Active", and an uploaded SSH public key is `Active`.
const ACTIVE: &str = "Active";

/// An SSH public key id the member's own constraints admit (`publicKeyIdType`, len 20..128,
/// pattern `^[\w]+$`), so the enum is the only thing the request below breaks.
const ADMISSIBLE_KEY_ID: &str = "APKAEIBAERJR2EXAMPLE";

/// `values` as an owned list, in the order given, so a statement of one set read off the
/// wire and one written in a fixture can be compared.
fn owned<Value: AsRef<str>>(values: &[Value]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.as_ref().to_string())
        .collect()
}

/// [`owned`], sorted, so two statements of one set can be compared as SETS rather than as
/// sequences.
fn as_a_set<Value: AsRef<str>>(values: &[Value]) -> Vec<String> {
    let mut sorted = owned(values);
    sorted.sort();
    sorted
}

/// The values the bracketed set of `message` states, in the order it states them.
fn stated_set(case: &str, message: &str) -> Vec<String> {
    let (_, bracketed) = message
        .split_once("Member must satisfy enum value set: [")
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a stated enum value set, got {message:?}")
        });
    let listed = bracketed.strip_suffix(']').unwrap_or_else(|| {
        panic!("case {case:?}: expected the stated set to be bracketed, got {message:?}")
    });
    listed.split(", ").map(str::to_string).collect()
}

/// RULE 1: the set a refusal states ON THE WIRE is the set the model declares — the same
/// values, so a clause can neither drop a value the enum declares nor name one it does not —
/// written in the order this service publishes for that enum.
///
/// The rows are the two enums this suite goes on to drive against held resources, each
/// reached through a request the walk refuses before dispatch: the order is a property of the
/// enum, so it is stated whether or not the named resource exists.
#[tokio::test]
async fn each_stated_set_is_the_modelled_set_in_the_published_order() {
    let router = router();

    for (enum_name, action, member, value, published, modelled) in [
        (
            "statusType",
            "UpdateSSHPublicKey",
            "Status",
            "BogusStatus",
            CREDENTIAL_STATUS,
            STATUS_TYPE_VALUES,
        ),
        (
            "encodingType",
            "GetSSHPublicKey",
            "Encoding",
            "BogusEncoding",
            KEY_ENCODING,
            ENCODING_TYPE_VALUES,
        ),
    ] {
        let (status, refused) = call(
            &router,
            action,
            &[
                ("UserName", "a-user"),
                ("SSHPublicKeyId", ADMISSIBLE_KEY_ID),
                (member, value),
            ],
        )
        .await;
        assert_eq!(
            (status, error_code(&refused)),
            (400, "ValidationError".to_string()),
            "case {enum_name:?}: expected the refusal of a value outside the set, got \
             {status}: {refused}"
        );
        let stated = stated_set(enum_name, &element(&refused, "Message"));
        assert_eq!(
            as_a_set(&stated),
            as_a_set(modelled),
            "case {enum_name:?}: expected the stated set to be the modelled values {:?}, got \
             {stated:?}",
            as_a_set(modelled)
        );
        assert_eq!(
            stated,
            owned(published),
            "case {enum_name:?}: expected the published order, got {stated:?}"
        );
    }
}

/// One row of the rejected-value table: what the row is called, and the value the request
/// carries for the enum-bearing member.
struct Rejected {
    case: &'static str,
    value: &'static str,
}

/// One row, spelled out so a row reads as a pair.
fn rejected(case: &'static str, value: &'static str) -> Rejected {
    Rejected { case, value }
}

/// Rows spanning the FORMS a value outside an enumerated set arrives in: a name no enum of
/// this service declares, a plausible status name AWS does not declare, an admissible value
/// in the wrong case, an admissible value of a DIFFERENT enum of this service, and an
/// admissible value with trailing space.
fn rejected_statuses() -> Vec<Rejected> {
    vec![
        rejected("a value no enum of this service declares", "BogusStatus"),
        rejected("a plausible status name AWS does not declare", "Enabled"),
        rejected("an admissible value in the wrong case", "active"),
        rejected("an admissible value of another enum", "SSH"),
        rejected("an admissible value with trailing space", "Active "),
    ]
}

/// One `UpdateSSHPublicKey` call.
async fn update_key_status(
    router: &MultiServiceRouter,
    user_name: &str,
    ssh_public_key_id: &str,
    status: &str,
) -> (u16, String) {
    call(
        router,
        "UpdateSSHPublicKey",
        &[
            ("UserName", user_name),
            ("SSHPublicKeyId", ssh_public_key_id),
            ("Status", status),
        ],
    )
    .await
}

/// Assert `refused` is the refusal RULE 1 states for `member` against `published`, and that
/// it carries nothing of `value`.
fn assert_states_the_published_set(
    case: &str,
    status: u16,
    refused: &str,
    member: &str,
    published: &[&str],
    value: &str,
) {
    assert_eq!(
        (status, error_code(refused)),
        (400, "ValidationError".to_string()),
        "case {case:?}: expected the query protocol's refusal of a value outside an \
         enumerated set, got {status}: {refused}"
    );
    let expected = expected_message(member, published);
    let message = element(refused, "Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
    // And the value is nowhere in the document, however it was spelled. The whole message is
    // compared above, which is what pins the value out of the clause; this check is about the
    // rest of the document. It looks for the QUOTED forms rather than the bare value because
    // a value differing from an admissible one only in case is a substring of the set the
    // clause legitimately states (`active` of `Inactive`).
    assert!(
        !refused.contains(&format!("Value &apos;{value}&apos;"))
            && !refused.contains(&format!("Value '{value}'")),
        "case {case:?}: the clause must not echo {value:?}, got {refused}"
    );
}

/// RULES 1 and 2 over the status of an SSH public key the account HOLDS: every form of a
/// value outside `statusType` is refused with the set in the published order, the held key
/// keeps the status it carried, and every value the set declares is then taken and reported
/// by a later read.
#[tokio::test]
async fn a_status_outside_the_set_states_the_published_order_and_writes_nothing() {
    let router = router();
    let user_name = "keystatusorder";
    create_user(&router, "the published-order rows", user_name).await;
    let (status, uploaded) = upload(
        &router,
        user_name,
        &openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 171), ""),
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture upload must be served, got {status}: {uploaded}"
    );
    let held = element(&uploaded, "SSHPublicKeyId");
    assert_eq!(
        element(&uploaded, "Status"),
        ACTIVE,
        "an uploaded key is Active, got {uploaded}"
    );

    // The refusals are driven against the key while it carries EACH value the set declares,
    // so what a refusal leaves behind is pinned from every one of them — and each admissible
    // value is proven on the way to be taken and reported.
    for carried in CREDENTIAL_STATUS.iter().copied() {
        let (status, answered) = update_key_status(&router, user_name, &held, carried).await;
        assert_eq!(
            status, 200,
            "case {carried:?}: an admissible status must be taken, got {status}: {answered}"
        );
        let (status, read) = get(&router, user_name, &held, "SSH").await;
        assert_eq!(
            (status, element(&read, "Status")),
            (200, carried.to_string()),
            "case {carried:?}: expected the read to report the status just set, got \
             {status}: {read}"
        );

        for Rejected { case, value } in rejected_statuses() {
            let case = format!("{case} while the key is {carried}");
            let (status, refused) = update_key_status(&router, user_name, &held, value).await;
            assert_states_the_published_set(
                &case,
                status,
                &refused,
                "status",
                CREDENTIAL_STATUS,
                value,
            );
            // RULE 2: the key is left exactly as it was held.
            let (status, read) = get(&router, user_name, &held, "SSH").await;
            assert_eq!(
                (status, element(&read, "Status")),
                (200, carried.to_string()),
                "case {case:?}: a refused update must leave the held status alone, got \
                 {status}: {read}"
            );
        }
    }
}

/// RULES 1 and 2 over the SAME enum reached through ANOTHER resource and operation: an
/// access key the account holds. The set is a property of the enum, so the clause is the one
/// the SSH key's status is refused with, and the held key keeps its status.
#[tokio::test]
async fn the_same_published_order_answers_every_member_targeting_the_enum() {
    let router = router();
    let user_name = "accesskeystatusorder";
    let (status, created_user) = call(&router, "CreateUser", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "the fixture CreateUser must be served, got {status}: {created_user}"
    );
    let (status, minted) = call(&router, "CreateAccessKey", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "the fixture CreateAccessKey must be served, got {status}: {minted}"
    );
    let access_key_id = element(&minted, "AccessKeyId");
    assert_eq!(
        element(&minted, "Status"),
        ACTIVE,
        "a minted access key is Active, got {minted}"
    );

    // Each admissible value of the set, with every form of a rejected value driven against
    // the key while it carries that value.
    for carried in CREDENTIAL_STATUS.iter().copied() {
        let (status, answered) = call(
            &router,
            "UpdateAccessKey",
            &[
                ("UserName", user_name),
                ("AccessKeyId", &access_key_id),
                ("Status", carried),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {carried:?}: an admissible status must be taken, got {status}: {answered}"
        );
        assert_eq!(
            listed_status(&router, user_name).await,
            carried,
            "case {carried:?}: expected the listing to report the status just set"
        );

        for Rejected { case, value } in rejected_statuses() {
            let case = format!("{case} while the access key is {carried}");
            let (status, refused) = call(
                &router,
                "UpdateAccessKey",
                &[
                    ("UserName", user_name),
                    ("AccessKeyId", &access_key_id),
                    ("Status", value),
                ],
            )
            .await;
            assert_states_the_published_set(
                &case,
                status,
                &refused,
                "status",
                CREDENTIAL_STATUS,
                value,
            );
            assert_eq!(
                listed_status(&router, user_name).await,
                carried,
                "case {case:?}: a refused update must leave the held status alone"
            );
        }
    }
}

/// The status the listing reports for the one access key `user_name` holds.
async fn listed_status(router: &MultiServiceRouter, user_name: &str) -> String {
    let (status, listed) = call(router, "ListAccessKeys", &[("UserName", user_name)]).await;
    assert_eq!(
        status, 200,
        "the listing must be served, got {status}: {listed}"
    );
    let statuses = elements(&listed, "Status");
    assert_eq!(
        statuses.len(),
        1,
        "expected the user to hold exactly one key, got {listed}"
    );
    statuses.into_iter().next().expect("one status")
}

/// RULES 1 and 2 over an enum this service publishes no order of its OWN for: the set is
/// stated in the order the model declares it, the read reports no key, and each admissible
/// value is served against the same held key.
#[tokio::test]
async fn an_encoding_outside_the_set_states_the_modelled_order() {
    let router = router();
    let user_name = "encodingorder";
    create_user(&router, "the encoding rows", user_name).await;
    let (status, uploaded) = upload(
        &router,
        user_name,
        &openssh_line(&modulus(PUBLISHED_MINIMUM_BITS, 172), ""),
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture upload must be served, got {status}: {uploaded}"
    );
    let held = element(&uploaded, "SSHPublicKeyId");

    // The same forms as the status rows, plus the empty value a query parameter can carry.
    for Rejected { case, value } in [
        rejected("a value no enum of this service declares", "DER"),
        rejected("an admissible value in the wrong case", "ssh"),
        rejected("an admissible value of another enum", "Active"),
        rejected("an admissible value with trailing space", "PEM "),
        rejected("an empty value", ""),
    ] {
        let (status, refused) = get(&router, user_name, &held, value).await;
        assert_states_the_published_set(
            case,
            status,
            &refused,
            "encoding",
            KEY_ENCODING,
            value,
        );
        assert!(
            elements(&refused, "SSHPublicKeyBody").is_empty(),
            "case {case:?}: a refused read must report no key, got {refused}"
        );
    }

    // And every value the set declares is served for the same held key, which is still held
    // as the upload left it.
    for encoding in KEY_ENCODING.iter().copied() {
        let (status, read) = get(&router, user_name, &held, encoding).await;
        assert_eq!(
            (status, element(&read, "Status")),
            (200, ACTIVE.to_string()),
            "case {encoding:?}: an admissible encoding must be served, got {status}: {read}"
        );
        assert!(
            !element(&read, "SSHPublicKeyBody").is_empty(),
            "case {encoding:?}: the read must report the held key, got {read}"
        );
    }
}
