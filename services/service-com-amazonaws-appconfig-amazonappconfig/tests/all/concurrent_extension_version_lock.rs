// SPDX-License-Identifier: CC0-1.0

//! `CreateExtension`'s `LatestVersionNumber` is a CONDITIONAL WRITE on the extension a name
//! addresses, and the refusal it answers a lock that lost with is AppConfig's own sentence.
//!
//! RULE: `LatestVersionNumber` is "the most recent current version number"
//! (`smithy_query(query="doc",shape="CreateExtensionRequest",member="LatestVersionNumber")`),
//! so a `CreateExtension` naming a version that is no longer the extension's current one is
//! refused with the modelled `ConflictException` (HTTP 409,
//! `smithy_query(query="operation",operation="CreateExtension")`) whose `Message` is one of
//! AppConfig's two conflict sentences — `LatestVersionNumber supplied in request <supplied>
//! does not match latest version <actual>.` or `Extension with id <extensionId> and
//! versionNumber <version> already exists for account <accountId>.` — every variable part
//! taken from the request, from the extension's own record or from the caller's account, and
//! no other property of the extension (its name least of all) named at all. Because the
//! comparison and the write it guards are ONE conditional write, N callers naming the same
//! current version leave exactly one winner, which lands the next version and nothing more:
//! the loser's refusal is the only place that conditional write explains itself, so a retry
//! loop keyed on AppConfig's wording has to match it.
//!
//! The rows span what the rule generalises over: how many versions the extension already
//! holds — so the two numbers the sentence reports differ row by row rather than being one
//! recorded pair — and how many callers race for the next one. A race on a name NOBODY has
//! created is the same conditional write at version 0, and the second suite covers it: there
//! the lock is ABSENT, which is the other way the comparison can fail, and AppConfig answers
//! that in the other class.
//!
//! Nothing here waits for a span: the racers are awaited, which is the condition the race's
//! outcome is settled by.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appconfig_workflow::{account, router, Answer};
use crate::extensions::{actions, create_extension, get_extension};

/// One race: how many versions the extension holds before it starts, and how many callers
/// then ask for the next one at once.
struct Race {
    /// What a failing assertion calls this row.
    case: &'static str,
    /// The versions seeded before the race, which is the version every racer locks.
    seeded: i32,
    /// How many callers race for the next version.
    racers: usize,
}

/// The races: an extension at its first version and at later ones, raced for by two callers
/// (the smallest race there is) and by more.
const RACES: [Race; 4] = [
    Race {
        case: "two callers racing for the second version",
        seeded: 1,
        racers: 2,
    },
    Race {
        case: "five callers racing for the second version",
        seeded: 1,
        racers: 5,
    },
    Race {
        case: "three callers racing for the third version",
        seeded: 2,
        racers: 3,
    },
    Race {
        case: "two callers racing for the fifth version",
        seeded: 4,
        racers: 2,
    },
];

/// How many callers race for one name in the suite that races to CREATE it.
const RACERS_FOR_A_NEW_NAME: usize = 4;

/// The `CreateExtension` body a racer sends: the same definition every racer sends, with its
/// own `Description` so the winner is identifiable.
fn racer_body(name: &str, racer: usize) -> Value {
    json!({
        "Name": name,
        "Actions": actions(&["ON_DEPLOYMENT_COMPLETE"]),
        "Description": format!("racer-{racer}"),
    })
}

/// The two sentences AppConfig refuses a lost conditional write with, as the rule states
/// them: the first reports the supplied lock against the extension's current version, the
/// second reports the version row the winner had already written. Both are id-and-number
/// only — neither names the extension.
fn conflict_sentences(supplied: i32, current: i32, extension_id: &str) -> [String; 2] {
    [
        format!(
            "LatestVersionNumber supplied in request {supplied} does not match latest version \
             {current}."
        ),
        format!(
            "Extension with id {extension_id} and versionNumber {current} already exists for \
             account {}.",
            account()
        ),
    ]
}

/// Seed `versions` versions of `name` one after another, answering the extension's minted id.
///
/// Each create locks the version before it, which is the same conditional write the race
/// then loses: seeding through the front door rather than writing state means the race runs
/// against an extension AppConfig itself minted.
async fn seed(router: &MultiServiceRouter, case: &str, name: &str, versions: i32) -> String {
    let mut id = String::new();
    for version in 0..versions {
        let answer = create_extension(router, &racer_body(name, 0), Some(version)).await;
        assert_eq!(
            answer.status,
            201,
            "case {case:?}: seeding version {} must answer 201, got {} / {}",
            version + 1,
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let body = answer.json();
        assert_eq!(
            body.get("VersionNumber").and_then(Value::as_i64),
            Some(i64::from(version + 1)),
            "case {case:?}: a create locking version {version} lands the next one, got {body}"
        );
        id = body
            .get("Id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: CreateExtension answered no Id, got {body}"))
            .to_string();
    }
    id
}

/// Fire `racers` concurrent `CreateExtension` calls for `name`, each locking `lock`, and
/// answer what each one was told, in racer order.
async fn race(
    router: &Arc<MultiServiceRouter>,
    name: &str,
    racers: usize,
    lock: Option<i32>,
) -> Vec<(usize, Answer)> {
    let mut tasks = Vec::with_capacity(racers);
    for racer in 0..racers {
        let router = Arc::clone(router);
        let name = name.to_string();
        tasks.push(tokio::spawn(async move {
            (
                racer,
                create_extension(&router, &racer_body(&name, racer), lock).await,
            )
        }));
    }
    let mut answers = Vec::with_capacity(racers);
    for task in tasks {
        answers.push(task.await.expect("the racing create task ran"));
    }
    answers
}

/// The `VersionNumber` `GetExtension` reports for `identifier`, which must be readable.
async fn version_read_back(router: &MultiServiceRouter, case: &str, identifier: &str) -> i64 {
    let answer = get_extension(router, identifier, None).await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: GetExtension({identifier:?}) expected 200, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    let body = answer.json();
    body.get("VersionNumber")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: GetExtension reported no VersionNumber: {body}"))
}

/// RULE: concurrent creates naming the extension's current version leave exactly one winner
/// at the next version, and every loser is refused HTTP 409 `ConflictException` with one of
/// AppConfig's two conflict sentences.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lock_that_loses_the_race_is_refused_in_appconfigs_own_words() {
    let router = Arc::new(router());

    for (index, row) in RACES.iter().enumerate() {
        let Race {
            case,
            seeded,
            racers,
        } = *row;
        let name = format!("raced-extension-{index}");
        let id = seed(&router, case, &name, seeded).await;

        let answers = race(&router, &name, racers, Some(seeded)).await;
        let landed = seeded + 1;
        let accepted: Vec<String> = conflict_sentences(seeded, landed, &id).to_vec();

        let mut winners: Vec<(usize, Value)> = Vec::new();
        for (racer, answer) in &answers {
            if answer.status == 201 {
                winners.push((*racer, answer.json()));
                continue;
            }
            assert_eq!(
                answer.status,
                409,
                "case {case:?}: racer {racer} must either land or be refused with the modelled \
                 ConflictException's HTTP 409, got {} / {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer
                    .error_code()
                    .split('#')
                    .next_back()
                    .unwrap_or_default(),
                "ConflictException",
                "case {case:?}: racer {racer} expected ConflictException, got {}",
                answer.error_code()
            );
            let message = answer.string("Message").unwrap_or_default();
            assert!(
                accepted.contains(&message),
                "case {case:?}: racer {racer} expected one of {accepted:?}, got {message:?}"
            );
            // The sentence is id-and-number only: the extension's NAME is not part of either
            // form, so a refusal that leaked it would be the wrong sentence however much of
            // the rest matched.
            assert!(
                !message.contains(&name),
                "case {case:?}: racer {racer}'s refusal must name no extension name, got \
                 {message:?}"
            );
        }

        assert_eq!(
            winners.len(),
            1,
            "case {case:?}: one conditional write wins and the rest lose, got {} winners out \
             of {racers} racers",
            winners.len()
        );
        let (winner, won) = winners.remove(0);
        assert_eq!(
            won.get("VersionNumber").and_then(Value::as_i64),
            Some(i64::from(landed)),
            "case {case:?}: racer {winner} won, so it landed version {landed}, got {won}"
        );
        assert_eq!(
            won.get("Id").and_then(Value::as_str),
            Some(id.as_str()),
            "case {case:?}: a further version keeps the extension's own id, got {won}"
        );
        assert_eq!(
            won.get("Description").and_then(Value::as_str),
            Some(format!("racer-{winner}").as_str()),
            "case {case:?}: the version that landed is the winner's own definition, got {won}"
        );

        // Exactly one new version came out of the race, whichever racer won it: the reader
        // reports the version that landed and the one past it does not exist.
        assert_eq!(
            version_read_back(&router, case, &name).await,
            i64::from(landed),
            "case {case:?}: the extension must read back at version {landed}"
        );
        assert_eq!(
            version_read_back(&router, case, &id).await,
            i64::from(landed),
            "case {case:?}: the extension must read back at version {landed} by id too"
        );
        let forked = get_extension(&router, &id, Some(landed + 1)).await;
        assert_eq!(
            forked.status,
            404,
            "case {case:?}: the race may mint no version past {landed}, got {} / {}",
            forked.status,
            String::from_utf8_lossy(&forked.body)
        );
    }
}

/// RULE at version 0: a name nobody has created is at version 0, so concurrent creates that
/// supply NO lock mint the name exactly once — and every caller that arrives after that is
/// told the lock it did not supply is what it needs, the other class AppConfig answers a
/// failed comparison in (HTTP 400 `BadRequestException`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_of_one_new_name_mint_it_exactly_once() {
    let router = Arc::new(router());
    let case = "four callers racing to create one name";
    let name = "raced-new-extension";

    let answers = race(&router, name, RACERS_FOR_A_NEW_NAME, None).await;

    let mut minted: BTreeSet<String> = BTreeSet::new();
    let expected_refusal = format!(
        "LatestVersionNumber not populated and an extension with name {name} already exists."
    );
    for (racer, answer) in &answers {
        if answer.status == 201 {
            let body = answer.json();
            assert_eq!(
                body.get("VersionNumber").and_then(Value::as_i64),
                Some(1),
                "case {case:?}: racer {racer} created the name, so it opened its history at \
                 version 1, got {body}"
            );
            minted.insert(
                body.get("Id")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: CreateExtension answered no Id, got {body}")
                    })
                    .to_string(),
            );
            continue;
        }
        assert_eq!(
            answer.status,
            400,
            "case {case:?}: racer {racer} arrived at a name that already exists with no lock, \
             which is the modelled BadRequestException's HTTP 400, got {} / {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer
                .error_code()
                .split('#')
                .next_back()
                .unwrap_or_default(),
            "BadRequestException",
            "case {case:?}: racer {racer} expected BadRequestException, got {}",
            answer.error_code()
        );
        assert_eq!(
            answer.string("Message").as_deref(),
            Some(expected_refusal.as_str()),
            "case {case:?}: racer {racer} expected the refusal {expected_refusal:?}, got {:?}",
            answer.string("Message")
        );
    }

    assert_eq!(
        minted.len(),
        1,
        "case {case:?}: one racer mints the name and the rest find it taken, got the ids \
         {minted:?}"
    );
    assert_eq!(
        version_read_back(&router, case, name).await,
        1,
        "case {case:?}: the name that was raced for holds exactly its first version"
    );
}
