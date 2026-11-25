// SPDX-License-Identifier: CC0-1.0

//! How this service ALLOCATES the identifier an instance id carries, driven over the wire it
//! speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML;
//! README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: EC2 allocates a resource identifier out of the whole space its
//! published form has room for — an instance id is `i-` and 17 hexadecimal digits of a
//! 64-bit identifier, so its leading digit is `0` — and it allocates one UNPREDICTABLY. Two
//! things follow, and both are asserted here:
//!
//! * an id a caller writes down for itself names no instance of the account, so the describes
//!   that decode an id before they look anything up answer it as a value the service never
//!   issued rather than resolving it to a live instance; and
//! * the ids a launch hands back are the account's own: in the published form, distinct, in
//!   the launch order the describes page over, and each resolvable to the instance it names.
//!
//! What the tables span: the ids a client reaches for when it wants one that names nothing
//! (the first ids a sequence counted from one would hand out, a round number further along,
//! the null id and a value high in the space), how many instances one launch mints, and
//! several launches against one account — plus two accounts, which are allocated different
//! identifiers because each is drawn rather than counted.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, elements, emulator, error_code, error_message, indexed};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The image every launch names; which image an instance was launched from has no bearing on
/// the identifier it is allocated.
const AMI: &str = "ami-0123456789abcdef0";

/// How many hexadecimal digits the long id form carries — "the form `i-xxxxxxxxxxxxxxxxx`" of
/// AWS's `InvalidInstanceID.Malformed` error-code entry.
const LONG_FORM_DIGITS: usize = 17;

/// The lowest identifier an allocated instance id may carry here.
///
/// The identifier is 64 bits wide and drawn across that space rather than counted up, so
/// every identifier a caller can reach by enumerating from one is far below this floor.
/// Stated as a bound rather than as a value, because WHICH identifier an instance is
/// allocated is the service's own to choose.
const ENUMERABLE_IDENTIFIER_CEILING: u128 = 1 << 48;

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Launch `count` instances in ONE reservation and answer the ids they were allocated.
async fn launch(case: &str, router: &MultiServiceRouter, count: u32) -> Vec<String> {
    let body = ok(
        case,
        router,
        "RunInstances",
        &[
            param("ImageId", AMI),
            param("MinCount", &count.to_string()),
            param("MaxCount", &count.to_string()),
        ],
    )
    .await;
    let ids = elements(&body, "instanceId");
    assert_eq!(
        ids.len(),
        count as usize,
        "case {case:?}: expected {count} instance id(s) from RunInstances, got {ids:?}"
    );
    ids
}

/// The identifier an id in the long form carries, or `None` for a value in any other form.
fn identifier(instance_id: &str) -> Option<u128> {
    let digits = instance_id.strip_prefix("i-")?;
    if digits.len() != LONG_FORM_DIGITS {
        return None;
    }
    u128::from_str_radix(digits, 16).ok()
}

/// Assert that `instance_id` is in the form EC2 issues an instance id in, and answer the
/// identifier it carries.
fn assert_published_form(case: &str, instance_id: &str) -> u128 {
    let value = identifier(instance_id).unwrap_or_else(|| {
        panic!(
            "case {case:?}: an id must be `i-` and {LONG_FORM_DIGITS} hexadecimal digits, got \
             {instance_id:?}"
        )
    });
    assert!(
        instance_id
            .strip_prefix("i-")
            .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()
                || (b'a'..=b'f').contains(&byte))),
        "case {case:?}: the digits of an id are lowercase hexadecimal, got {instance_id:?}"
    );
    assert!(
        instance_id.starts_with("i-0"),
        "case {case:?}: a 64-bit identifier never fills the seventeenth digit, so the id must \
         read `i-0…`, got {instance_id:?}"
    );
    assert!(
        value > ENUMERABLE_IDENTIFIER_CEILING,
        "case {case:?}: an allocated identifier is drawn across the space, so it must be above \
         {ENUMERABLE_IDENTIFIER_CEILING}, got {value} from {instance_id:?}"
    );
    value
}

/// Every id a caller reaches for when it wants one that names nothing: the first ids a
/// sequence counted from one would hand out, a round number further along, the null id and a
/// value high in the space.
const FABRICATED_IDS: &[(&str, &str)] = &[
    ("the first id a sequence from one would hand out", "i-00000000000000001"),
    ("the second such id", "i-00000000000000002"),
    ("the third such id", "i-00000000000000003"),
    ("a round number further along a sequence", "i-00000000000000064"),
    ("the null id", "i-00000000000000000"),
    ("a value high in the space", "i-0fffffffffffffffe"),
];

/// An id a caller made up names no instance of the account, whatever the account has
/// launched: the describe that decodes an id before it looks anything up answers it as a
/// value this service never issued.
#[tokio::test]
async fn an_instance_id_a_caller_fabricates_names_no_instance() {
    let (router, _engine) = emulator();
    // Several launches, so the account holds more instances than the table's low ids would
    // name if the identifiers were positions in a sequence.
    let mut launched = launch("one instance", &router, 1).await;
    launched.extend(launch("three instances in one reservation", &router, 3).await);
    launched.extend(launch("one more instance", &router, 1).await);

    for (case, fabricated) in FABRICATED_IDS {
        assert!(
            !launched.contains(&(*fabricated).to_string()),
            "case {case:?}: the ids the account was allocated {launched:?} must not hold the id \
             a caller made up, got {fabricated}"
        );
        let (status, body) = call(
            &router,
            REGION,
            "DescribeInstances",
            &indexed("InstanceId", &[fabricated]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {fabricated}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInstanceID.Malformed",
            "case {case:?}: expected the refusal for an id this service never issued, got {}",
            error_message(&body)
        );
    }

    // And every id the account really was allocated resolves to its own instance.
    let body = ok(
        "the launched ids",
        &router,
        "DescribeInstances",
        &indexed(
            "InstanceId",
            &launched.iter().map(String::as_str).collect::<Vec<&str>>(),
        ),
    )
    .await;
    let reported = elements(&body, "instanceId");
    let mut expected = launched.clone();
    expected.sort();
    let mut actual = reported.clone();
    actual.sort();
    assert_eq!(
        actual, expected,
        "the describe must report every instance the account was allocated, got {reported:?}"
    );
}

/// The ids one account is allocated are in the published form, distinct, and in launch order
/// — the order the describes page over.
#[tokio::test]
async fn allocated_instance_ids_are_distinct_and_in_launch_order() {
    let (router, _engine) = emulator();
    let mut launched: Vec<String> = Vec::new();
    for (case, count) in [("one instance", 1_u32), ("four instances", 4), ("two more", 2)] {
        launched.extend(launch(case, &router, count).await);
    }

    let identifiers: Vec<u128> = launched
        .iter()
        .map(|instance_id| assert_published_form("an allocated id", instance_id))
        .collect();
    for window in identifiers.windows(2) {
        assert!(
            window[1] > window[0],
            "each id is allocated above every one before it, so launch order is id order; got \
             {:?} after {:?} in {launched:?}",
            window[1],
            window[0]
        );
    }
    let distinct: std::collections::BTreeSet<&String> = launched.iter().collect();
    assert_eq!(
        distinct.len(),
        launched.len(),
        "every instance holds an id of its own, got {launched:?}"
    );
}

/// Two accounts are not allocated the same identifier: each is DRAWN, so neither account's
/// ids can be read off the other's.
#[tokio::test]
async fn two_accounts_are_not_allocated_the_same_instance_id() {
    let (first_router, _first_engine) = emulator();
    let (second_router, _second_engine) = emulator();
    let first = launch("the first account", &first_router, 1).await;
    let second = launch("the second account", &second_router, 1).await;

    assert_published_form("the first account's id", &first[0]);
    assert_published_form("the second account's id", &second[0]);
    assert_ne!(
        first[0], second[0],
        "an identifier is drawn rather than counted, so two accounts cannot be handed the same \
         first id, got {first:?} and {second:?}"
    );
}
