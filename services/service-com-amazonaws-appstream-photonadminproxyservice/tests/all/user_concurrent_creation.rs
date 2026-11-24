// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what `CreateUser` answers every caller that issues the SAME user
//! pool create at the same time.
//!
//! RULE: an `(AuthenticationType, UserName)` identity is allocated AT MOST ONCE, and
//! allocating it takes a moment — the create that claims the username holds the claim while
//! the record is written. A create that arrives while the allocation of exactly the user it
//! asks for is still running therefore conflicts with nothing: it joins that allocation and
//! is answered the same empty result, so creates of one user issued together are ALL
//! answered HTTP 200 and the pool afterwards holds exactly one enabled user for the
//! identity, built from the record they allocated. A create that asks for a DIFFERENT user
//! under that identity does conflict: it is refused `ResourceAlreadyExistsException` (400)
//! with AWS's sentence, as is every create issued once the allocation has settled — which
//! the sibling suites [`crate::user_allocation_window`] and [`crate::user_duplicate_create`]
//! span over a settled pool.
//!
//! The rows differ along what the rule generalises over: the form and the length of the
//! user name (an ASCII email address, a tagged address, non-ASCII letters, a bare name, a
//! name carrying a symbol, the longest name the member admits), how many callers issue the
//! create together (one, a pair, several, a dozen), what those creates carry beside the
//! identity (nothing, a suppressed welcome email, one name, both names), whether they ask
//! for the same user or for different ones, and how many identities are raced side by side.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk. Nothing here waits for a span:
//! each case asserts on the answers the racing calls returned and on what the pool reports,
//! never on how fast the host ran them.

use std::sync::Arc;

use serde_json::{json, Value};

use service_com_amazonaws_appstream_photonadminproxyservice::state;

use crate::stack_fleet_lifecycle::{error_code, Emulator};

/// AWS's sentence for a username that is taken, read from the crate so this suite tracks
/// what the service states rather than a copy of it.
const ALREADY_EXISTS: &str = state::USER_ALREADY_EXISTS_MESSAGE;

/// The wire error code a create of a taken username answers, one of `CreateUser`'s
/// modelled errors (`smithy_query(query="operation", operation="CreateUser")`).
const ALREADY_EXISTS_CODE: &str = "ResourceAlreadyExistsException";

/// The only authentication type the user pool operations manage, which
/// `CreateUser.AuthenticationType` documents as "You must specify USERPOOL".
const USER_POOL: &str = "USERPOOL";

/// The Region these cases sign in, so the ARN a read reports can be stated rather than
/// copied from the answer.
const REGION: &str = "us-west-2";

/// The status of a user that has been created but has not set a password, read from the
/// crate so the assertion tracks what the service states.
const UNCONFIRMED: &str = state::USER_STATUS_UNCONFIRMED;

/// The `CreateUser` request for `user_name` carrying `extra` beside the identity.
fn create_request(user_name: &str, extra: &Value) -> Value {
    let mut request = json!({ "UserName": user_name, "AuthenticationType": USER_POOL });
    for (member, value) in extra.as_object().expect("the row carries members") {
        request[member] = value.clone();
    }
    request
}

/// Issue every request at once and answer what each of them was answered, in the order the
/// requests were given.
async fn issued_together(emulator: &Arc<Emulator>, requests: Vec<Value>) -> Vec<(u16, Value)> {
    let mut issued = Vec::with_capacity(requests.len());
    for request in requests {
        let emulator = Arc::clone(emulator);
        issued.push(tokio::spawn(async move {
            emulator.call("CreateUser", request).await
        }));
    }
    let mut answers = Vec::with_capacity(issued.len());
    for call in issued {
        answers.push(call.await.expect("the racing call completes"));
    }
    answers
}

/// Every user the pool reports.
async fn user_pool(emulator: &Emulator, case: &str) -> Vec<Value> {
    let payload = emulator
        .ok(
            case,
            "DescribeUsers",
            json!({ "AuthenticationType": USER_POOL }),
        )
        .await;
    payload
        .get("Users")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Every user the pool reports under `user_name` — one for an identity allocated once.
async fn reported(emulator: &Emulator, case: &str, user_name: &str) -> Vec<Value> {
    user_pool(emulator, case)
        .await
        .into_iter()
        .filter(|user| user.get("UserName").and_then(Value::as_str) == Some(user_name))
        .collect()
}

/// Assert one answer is the empty success.
fn assert_empty_success(case: &str, status: u16, payload: &Value) {
    assert_eq!(
        status, 200,
        "case {case:?}: the create must be answered the empty success, got {status} {payload}"
    );
    assert_eq!(
        payload,
        &json!({}),
        "case {case:?}: the modelled result carries no members, got {payload}"
    );
}

/// Assert one answer is the refusal of a username that is taken.
fn assert_refused_as_taken(case: &str, status: u16, payload: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: a create is either answered 200 or refused 400, got {status} {payload}"
    );
    assert_eq!(
        error_code(payload),
        ALREADY_EXISTS_CODE,
        "case {case:?}: a create that asks for another user under a taken username must be \
         refused as a name that is taken, got {payload}"
    );
    assert_eq!(
        payload.get("Message").and_then(Value::as_str),
        Some(ALREADY_EXISTS),
        "case {case:?}: expected AWS's sentence {ALREADY_EXISTS:?}, got {payload}"
    );
}

/// One row of the race table: how the identity is named, how many callers issue the create
/// together, and what each of those identical creates carries beside the identity.
struct Race {
    case: &'static str,
    user_name: String,
    racers: usize,
    extra: Value,
}

/// The longest user name a create admits: `CreateUser.UserName` targets `Username`, whose
/// constraints are @length 1..128 and @pattern `^[\p{L}\p{M}\p{S}\p{N}\p{P}]+$`
/// (`smithy_query(query="operation", operation="CreateUser")`), so 128 characters of
/// letters and punctuation.
fn longest_user_name() -> String {
    let domain = "@example.com";
    let name = format!("{}{domain}", "u".repeat(128 - domain.chars().count()));
    assert_eq!(
        name.chars().count(),
        128,
        "the longest admitted name is 128 characters, built {name:?}"
    );
    name
}

/// The race table.
fn races() -> Vec<Race> {
    vec![
        Race {
            case: "a lone create",
            user_name: "alone@example.com".to_string(),
            racers: 1,
            extra: json!({}),
        },
        Race {
            case: "a pair issued together",
            user_name: "pair@example.com".to_string(),
            racers: 2,
            extra: json!({ "MessageAction": "SUPPRESS" }),
        },
        Race {
            case: "six issued together",
            user_name: "six.racers@example.com".to_string(),
            racers: 6,
            extra: json!({ "MessageAction": "SUPPRESS" }),
        },
        Race {
            case: "a dozen issued together, both names carried",
            user_name: "tagged+label@example.com".to_string(),
            racers: 12,
            extra: json!({ "FirstName": "Ada", "LastName": "Lovelace" }),
        },
        Race {
            case: "a name in non-ASCII letters",
            user_name: "ユーザー@example.com".to_string(),
            racers: 5,
            extra: json!({ "FirstName": "Grace" }),
        },
        Race {
            case: "a bare name, no domain",
            user_name: "bare.name".to_string(),
            racers: 4,
            extra: json!({ "LastName": "Hopper" }),
        },
        Race {
            case: "a name carrying a symbol",
            user_name: "user=name@example.com".to_string(),
            racers: 3,
            extra: json!({}),
        },
        Race {
            case: "the longest name the member admits",
            user_name: longest_user_name(),
            racers: 3,
            extra: json!({ "MessageAction": "SUPPRESS" }),
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_creates_issued_together_are_all_answered_the_empty_success() {
    for row in races() {
        let case = row.case;
        let emulator = Arc::new(Emulator::in_region(REGION));
        let request = create_request(&row.user_name, &row.extra);
        let answers = issued_together(&emulator, vec![request; row.racers]).await;

        assert_eq!(
            answers.len(),
            row.racers,
            "case {case:?}: every caller must be answered, got {answers:?}"
        );
        for (status, payload) in &answers {
            assert_empty_success(case, *status, payload);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creates_issued_together_allocate_one_enabled_user_for_the_identity() {
    for row in races() {
        let case = row.case;
        let emulator = Arc::new(Emulator::in_region(REGION));
        let request = create_request(&row.user_name, &row.extra);
        issued_together(&emulator, vec![request; row.racers]).await;

        let stored = reported(&emulator, case, &row.user_name).await;
        assert_eq!(
            stored.len(),
            1,
            "case {case:?}: {} creates must have allocated the identity exactly once, got \
             {stored:?}",
            row.racers
        );
        let user = &stored[0];
        assert_eq!(
            user.get("Enabled"),
            Some(&json!(true)),
            "case {case:?}: the user they created may sign in, got {user:?}"
        );
        assert_eq!(
            user.get("AuthenticationType"),
            Some(&json!(USER_POOL)),
            "case {case:?}: the identity keeps the authentication type it was created \
             under, got {user:?}"
        );
        assert_eq!(
            user.get("Status"),
            Some(&json!(UNCONFIRMED)),
            "case {case:?}: a created user has not confirmed a password yet, got {user:?}"
        );
        let arn = state::user_arn(REGION, USER_POOL, &row.user_name);
        assert_eq!(
            user.get("Arn"),
            Some(&json!(arn)),
            "case {case:?}: expected the ARN {arn:?}, got {user:?}"
        );
        assert!(
            user.get("CreatedTime").is_some(),
            "case {case:?}: the allocated record carries when it was created, got {user:?}"
        );
        // The one record carries exactly the attributes those identical creates asked
        // for, and nothing they did not name.
        for member in ["FirstName", "LastName"] {
            assert_eq!(
                user.get(member),
                row.extra.get(member),
                "case {case:?}: expected {member} {:?}, got {user:?}",
                row.extra.get(member)
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_issued_once_the_burst_has_been_answered_is_refused_as_a_taken_username() {
    // However many creates allocated the identity together, the allocation has settled by
    // the time they were all answered: a create issued afterwards claims a username that is
    // taken, and the pool is left exactly as the burst allocated it.
    for row in races() {
        let case = row.case;
        let emulator = Arc::new(Emulator::in_region(REGION));
        let request = create_request(&row.user_name, &row.extra);
        issued_together(&emulator, vec![request.clone(); row.racers]).await;
        let allocated = reported(&emulator, case, &row.user_name).await;

        let (status, payload) = emulator.call("CreateUser", request).await;
        assert_refused_as_taken(case, status, &payload);

        let after = reported(&emulator, case, &row.user_name).await;
        assert_eq!(
            after, allocated,
            "case {case:?}: the refused create must leave the stored user exactly as the \
             burst allocated it, got {after:?}"
        );
    }
}

/// One row of the conflicting-burst table: what each create issued together carries beside
/// the one identity they all name. They do not all ask for the same user, so they do not
/// all join one allocation.
struct Conflicting {
    case: &'static str,
    user_name: &'static str,
    /// What each caller carries beside the identity, one entry per caller.
    carried: Vec<Value>,
}

/// The conflicting-burst table. The rows differ along what the rule generalises over: how
/// many callers disagree, which attribute they disagree on, whether one of them names an
/// attribute the others leave out, and how many of them agree with each other.
fn conflicting_bursts() -> Vec<Conflicting> {
    vec![
        Conflicting {
            case: "two callers, different first names",
            user_name: "disagreeing.pair@example.com",
            carried: vec![json!({ "FirstName": "Ada" }), json!({ "FirstName": "Augusta" })],
        },
        Conflicting {
            case: "three callers, each a different last name",
            user_name: "disagreeing.trio@example.com",
            carried: vec![
                json!({ "LastName": "Lovelace" }),
                json!({ "LastName": "Byron" }),
                json!({ "LastName": "Hopper" }),
            ],
        },
        Conflicting {
            case: "one caller names an attribute the others leave out",
            user_name: "ユーザー@example.com",
            carried: vec![
                json!({}),
                json!({}),
                json!({ "FirstName": "Grace" }),
                json!({}),
            ],
        },
        Conflicting {
            case: "two groups of callers, each agreeing with itself",
            user_name: "two.groups",
            carried: vec![
                json!({ "FirstName": "Ada", "LastName": "Lovelace" }),
                json!({ "FirstName": "Grace", "LastName": "Hopper" }),
                json!({ "FirstName": "Ada", "LastName": "Lovelace" }),
                json!({ "FirstName": "Grace", "LastName": "Hopper" }),
            ],
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn creates_issued_together_for_different_users_allocate_the_identity_once() {
    for row in conflicting_bursts() {
        let case = row.case;
        let emulator = Arc::new(Emulator::in_region(REGION));
        let requests: Vec<Value> = row
            .carried
            .iter()
            .map(|extra| create_request(row.user_name, extra))
            .collect();
        let answers = issued_together(&emulator, requests).await;
        assert_eq!(
            answers.len(),
            row.carried.len(),
            "case {case:?}: every caller must be answered, got {answers:?}"
        );

        // The identity is allocated once: the pool holds one user for the name.
        let stored = reported(&emulator, case, row.user_name).await;
        assert_eq!(
            stored.len(),
            1,
            "case {case:?}: the identity must be allocated exactly once, got {stored:?}"
        );
        let user = &stored[0];

        // Every caller is answered the empty success or the taken-username refusal, and
        // every caller answered success asked for exactly the user the pool holds — a
        // success is never reported for a user that was not allocated.
        let mut allocated = 0_usize;
        for (extra, (status, payload)) in row.carried.iter().zip(&answers) {
            let case = format!("{case}, caller carrying {extra}");
            if *status == 200 {
                allocated += 1;
                assert_empty_success(&case, *status, payload);
                for member in ["FirstName", "LastName"] {
                    assert_eq!(
                        user.get(member),
                        extra.get(member),
                        "case {case:?}: a caller answered success must have asked for the \
                         user the pool holds, expected {member} {:?}, got {user:?}",
                        extra.get(member)
                    );
                }
                continue;
            }
            assert_refused_as_taken(&case, *status, payload);
        }
        assert!(
            allocated >= 1,
            "case {case:?}: the first of them must allocate the identity, got {answers:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identities_raced_side_by_side_are_each_allocated_exactly_once() {
    // Rows: how many identities are created at once, and how many callers race each of
    // them. The allocation is per identity: the callers of one name are all answered the
    // empty success, and the pool holds one enabled user per name — never one user for the
    // whole batch.
    for (identities, racers) in [(2_usize, 2_usize), (4, 3), (8, 2)] {
        let case = format!("{identities} identities raced by {racers} caller(s) each");
        let emulator = Arc::new(Emulator::in_region(REGION));
        let names: Vec<String> = (0..identities)
            .map(|index| format!("side.by.side.{index}@example.com"))
            .collect();

        let mut requests = Vec::new();
        for name in &names {
            let request = create_request(name, &json!({ "MessageAction": "SUPPRESS" }));
            requests.extend(std::iter::repeat_n(request, racers));
        }
        let answers = issued_together(&emulator, requests).await;
        for (status, payload) in &answers {
            assert_empty_success(&case, *status, payload);
        }

        for name in &names {
            let stored = reported(&emulator, &case, name).await;
            assert_eq!(
                stored.len(),
                1,
                "case {case:?}: {name:?} must be allocated exactly once, got {stored:?}"
            );
            assert_eq!(
                stored[0].get("Enabled"),
                Some(&json!(true)),
                "case {case:?}: the user created under {name:?} may sign in, got {:?}",
                stored[0]
            );
        }
        let all = user_pool(&emulator, &case).await;
        assert_eq!(
            all.len(),
            identities,
            "case {case:?}: the pool must hold one user per raced identity, got {all:?}"
        );
    }
}
