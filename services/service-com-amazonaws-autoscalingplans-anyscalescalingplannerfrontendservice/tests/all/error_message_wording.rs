// SPDX-License-Identifier: CC0-1.0

//! The wording of the refusals this service builds from the request.
//!
//! Two rules are pinned here, each over a TABLE of inputs rather than the one request
//! that exposed them:
//!
//! * a lookup by `ScalingPlanName` + `ScalingPlanVersion` that finds no stored plan is
//!   refused with the request-derived sentence
//!   `No scaling plan found for scaling plan name: <name>, version: <version>`, on every
//!   MUTATING operation that identifies ONE plan by those two members, in the not-found
//!   shape it models — while the reporting operations, which model no not-found error,
//!   report the empty set instead and never carry that sentence;
//! * a `DescribeScalingPlans` request that combines `ApplicationSources` with the
//!   name-based selector (`ScalingPlanNames` and/or `ScalingPlanVersion`) is refused
//!   with the fixed sentence
//!   `Application sources cannot be provided with scaling plan names or version`;
//! * a write whose scaling instruction states a capacity band the service will not
//!   register — a minimum below the floor of the band — is refused with the fixed
//!   sentence `Minimum capacity cannot be less than 1`, in the modelled
//!   `ValidationException` (HTTP 400), on every operation that carries
//!   `ScalingInstructions`, and leaves the store as it was; the sibling relation between
//!   the two capacity members keeps its own sentence.
//!
//! The harness (router, dispatch, error-body readers) is reused from
//! [`crate::scaling_plan_lifecycle`]; nothing here builds a second one.

use serde_json::{json, Value};

use service_com_amazonaws_autoscalingplans_anyscalescalingplannerfrontendservice::ops::get_scaling_plan_resource_forecast_data::{
    FORECAST_SCALABLE_DIMENSION, FORECAST_SERVICE_NAMESPACE,
};

use crate::scaling_plan_lifecycle::{
    error_code, error_message, instruction, now_secs, simple_instructions, stack_source, Emulator,
};

/// The operations that REFUSE a lookup by `ScalingPlanName` + `ScalingPlanVersion` that
/// finds no plan, with the error shape each models that refusal in.
///
/// Source: `smithy_query(query="operation",
/// operation="DeleteScalingPlan,UpdateScalingPlan")` — both write paths bind
/// `ObjectNotFoundException` (client, httpError 400). Both halves of the lookup — the
/// name and the version — are probed by the rows below. The reporting operations are
/// deliberately absent: they bind no not-found shape, so a miss is an empty answer there
/// (pinned by `crate::absent_plan_reads`), not a refusal to word.
const LOOKUP_OPERATIONS: [(&str, &str); 2] = [
    ("DeleteScalingPlan", "ObjectNotFoundException"),
    ("UpdateScalingPlan", "ObjectNotFoundException"),
];

/// The reporting operation that looks one plan up by name and version. It is driven apart
/// from [`LOOKUP_OPERATIONS`] because its request carries five more required members.
const FORECAST_OPERATION: &str = "GetScalingPlanResourceForecastData";

/// The only scaling-plan version the model documents as valid.
const ONLY_VERSION: i64 = 1;

/// AWS's fixed text for the application-source / name-selector conflict.
const APPLICATION_SOURCES_CONFLICT: &str =
    "Application sources cannot be provided with scaling plan names or version";

/// AWS's fixed text for a minimum capacity below the floor of the band an instruction
/// registers. The sentence interpolates nothing: the floor it names is part of the wording.
const MINIMUM_CAPACITY_BELOW_FLOOR: &str = "Minimum capacity cannot be less than 1";

/// AWS's fixed text for the OTHER relation the capacity pair has to satisfy, kept here so
/// the rows below can assert that the two refusals are not interchangeable.
const MAXIMUM_CAPACITY_BELOW_MINIMUM: &str = "Maximum capacity cannot be less than minimum capacity";

/// The least capacity a scaling instruction may set as the floor of its band, as the
/// sentence above words it.
const CAPACITY_FLOOR: i64 = 1;

/// The plan name the refused writes below name — no create that breaks a capacity relation
/// may leave a plan behind under it.
const REFUSED_PLAN: &str = "below-the-floor";

/// Two scaling instructions, each for its own scalable resource, with the one at `position`
/// carrying the band `min..max`.
///
/// The resource triples are the model's own values
/// (`smithy_query(query="shape",shape="ServiceNamespace,ScalableDimension")`); keeping two
/// entries lets a row break the relation in the FIRST or in a LATER instruction, since the
/// rule is about one instruction's band and not about the position of that instruction in
/// the list.
fn instructions_with_band(position: usize, min: i64, max: i64) -> Value {
    let mut built = vec![
        instruction(
            "autoscaling",
            "autoScalingGroup/my-asg",
            "autoscaling:autoScalingGroup:DesiredCapacity",
            CAPACITY_FLOOR,
            10,
            "ASGAverageCPUUtilization",
            40.0,
        ),
        instruction(
            "autoscaling",
            "autoScalingGroup/second-asg",
            "autoscaling:autoScalingGroup:DesiredCapacity",
            CAPACITY_FLOOR,
            10,
            "ASGAverageCPUUtilization",
            40.0,
        ),
    ];
    built[position]["MinCapacity"] = json!(min);
    built[position]["MaxCapacity"] = json!(max);
    Value::Array(built)
}

/// The not-found sentence the rule requires for one requested name and version.
fn expected_not_found(name: &str, version: i64) -> String {
    format!("No scaling plan found for scaling plan name: {name}, version: {version}")
}

/// A `GetScalingPlanResourceForecastData` request for the Auto Scaling group resource the
/// stored plan instructs, with every member the model marks required.
///
/// The window is a valid one (an hour starting now, well inside the documented limits
/// `crate::forecast_data` pins), so nothing but the plan lookup can refuse it.
fn forecast_request(name: &str, version: i64) -> Value {
    let start = now_secs();
    json!({
        "ScalingPlanName": name,
        "ScalingPlanVersion": version,
        "ServiceNamespace": FORECAST_SERVICE_NAMESPACE,
        "ResourceId": FORECAST_RESOURCE_ID,
        "ScalableDimension": FORECAST_SCALABLE_DIMENSION,
        "ForecastDataType": "CapacityForecast",
        "StartTime": start,
        "EndTime": start + 3_600,
    })
}

/// The Auto Scaling group [`simple_instructions`] instructs, which is also the resource
/// the forecast rows below name.
const FORECAST_RESOURCE_ID: &str = "autoScalingGroup/my-asg";

/// A plan the emulator really holds, so a row that names it probes the VERSION half of
/// the lookup rather than the name half.
const STORED_PLAN: &str = "stored plan";

async fn with_one_stored_plan() -> Emulator {
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateScalingPlan",
            json!({
                "ScalingPlanName": STORED_PLAN,
                "ApplicationSource": stack_source(1),
                "ScalingInstructions": simple_instructions(),
            }),
        )
        .await;
    emulator.make_all_due();
    emulator
}

#[tokio::test]
async fn a_lookup_that_finds_no_plan_names_the_requested_name_and_version() {
    let emulator = with_one_stored_plan().await;

    // `ScalingPlanName` is len 1..128, pattern ^[\p{Print}&&[^|:/]]+$ and
    // `ScalingPlanVersion` is a long (Smithy: DeleteScalingPlanRequest members), so the
    // rows differ in name length, name character content and version magnitude — the
    // dimensions the sentence substitutes over.
    let name_at_bound = "a".repeat(128);
    let cases: Vec<(&str, String, i64)> = vec![
        ("absent plain name", "sim-probe-absent".to_string(), 1),
        ("absent one-character name", "x".to_string(), 1),
        (
            "absent name with spaces and punctuation",
            "my plan #2 (staging)".to_string(),
            1,
        ),
        ("absent name at the 128-character bound", name_at_bound, 1),
        (
            "stored name, version past the only one modelled",
            STORED_PLAN.to_string(),
            2,
        ),
        ("stored name, large version", STORED_PLAN.to_string(), 4_294_967_296),
        (
            "absent name, very large version",
            "sim-probe-absent".to_string(),
            9_007_199_254_740_993,
        ),
        ("absent name, version zero", "sim-probe-absent".to_string(), 0),
        ("absent name, negative version", "sim-probe-absent".to_string(), -1),
    ];

    for (label, name, version) in cases {
        let request = json!({ "ScalingPlanName": name, "ScalingPlanVersion": version });
        for (operation, expected_shape) in LOOKUP_OPERATIONS {
            let (status, body) = emulator.call(operation, &request).await;
            assert_eq!(
                status, 400,
                "case {operation}/{label}: expected HTTP 400, got {status} with {body}"
            );
            assert_eq!(
                error_code(&body),
                expected_shape,
                "case {operation}/{label}: expected error code {expected_shape}, got {body}"
            );
            let actual = error_message(&body);
            assert_eq!(
                actual,
                expected_not_found(&name, version),
                "case {operation}/{label}: expected {:?}, got {actual:?}",
                expected_not_found(&name, version)
            );
        }

        // The same miss on the two reporting paths is not a refusal at all — whichever
        // half of the coordinate missed — so the sentence above must not travel in their
        // answers.
        for (operation, request) in [
            (
                "DescribeScalingPlanResources",
                json!({ "ScalingPlanName": name, "ScalingPlanVersion": version }),
            ),
            (FORECAST_OPERATION, forecast_request(&name, version)),
        ] {
            let (status, body) = emulator.call(operation, &request).await;
            assert_eq!(
                status, 200,
                "case {operation}/{label}: expected HTTP 200, got {status} with {body}"
            );
            assert!(
                !body.to_string().contains("No scaling plan found"),
                "case {operation}/{label}: expected no not-found sentence, got {body}"
            );
        }
    }

    // The control: the plan that IS stored, at the version it has, is not refused on any
    // of these paths — the sentence above is reported for a missing plan, not for every
    // lookup.
    emulator
        .ok(
            "DescribeScalingPlanResources",
            json!({ "ScalingPlanName": STORED_PLAN, "ScalingPlanVersion": ONLY_VERSION }),
        )
        .await;
    emulator
        .ok(
            FORECAST_OPERATION,
            forecast_request(STORED_PLAN, ONLY_VERSION),
        )
        .await;
}

#[tokio::test]
async fn application_sources_beside_the_name_selector_are_refused_in_awss_words() {
    let emulator = Emulator::new();
    for (n, name) in [(1usize, "by-stack"), (2, "second")] {
        emulator
            .ok(
                "CreateScalingPlan",
                json!({
                    "ScalingPlanName": name,
                    "ApplicationSource": stack_source(n),
                    "ScalingInstructions": simple_instructions(),
                }),
            )
            .await;
    }
    emulator.make_all_due();

    let eleven: Vec<String> = (0..11).map(|n| format!("name-{n}")).collect();
    // Rows differ in WHICH half of the name selector is present, in how many names and
    // sources are supplied, and in whether another rule is broken at the same time.
    let conflicting: Vec<(&str, Value)> = vec![
        (
            "one source and one name",
            json!({ "ScalingPlanNames": ["by-stack"], "ApplicationSources": [stack_source(1)] }),
        ),
        (
            "one source and a version",
            json!({ "ApplicationSources": [stack_source(1)], "ScalingPlanVersion": 1 }),
        ),
        (
            "one source, a name and a version",
            json!({
                "ScalingPlanNames": ["by-stack"],
                "ScalingPlanVersion": 1,
                "ApplicationSources": [stack_source(1)],
            }),
        ),
        (
            "two sources and two names",
            json!({
                "ScalingPlanNames": ["by-stack", "second"],
                "ApplicationSources": [stack_source(1), stack_source(2)],
            }),
        ),
        (
            "eleven names beside a source",
            json!({ "ScalingPlanNames": eleven, "ApplicationSources": [stack_source(1)] }),
        ),
    ];

    for (label, request) in conflicting {
        let (status, body) = emulator.call("DescribeScalingPlans", &request).await;
        assert_eq!(
            status, 400,
            "case {label}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {label}: expected a ValidationException, got {body}"
        );
        let actual = error_message(&body);
        assert_eq!(
            actual, APPLICATION_SOURCES_CONFLICT,
            "case {label}: expected {APPLICATION_SOURCES_CONFLICT:?}, got {actual:?}"
        );
    }

    // Either selector ALONE is accepted, so the rule refuses the combination and not
    // the members themselves.
    let accepted: Vec<(&str, Value)> = vec![
        (
            "sources alone",
            json!({ "ApplicationSources": [stack_source(1)] }),
        ),
        ("names alone", json!({ "ScalingPlanNames": ["by-stack"] })),
        (
            "names and a version",
            json!({ "ScalingPlanNames": ["by-stack"], "ScalingPlanVersion": 1 }),
        ),
        ("no selector at all", json!({})),
    ];
    for (label, request) in accepted {
        let (status, body) = emulator.call("DescribeScalingPlans", &request).await;
        assert_eq!(
            status, 200,
            "case {label}: expected HTTP 200, got {status} with {body}"
        );
    }

    // A version with neither a name nor a source breaks a DIFFERENT rule, so it must not
    // borrow the conflict sentence.
    let (status, body) = emulator
        .call("DescribeScalingPlans", &json!({ "ScalingPlanVersion": 1 }))
        .await;
    let actual = error_message(&body);
    assert_eq!(
        (status, error_code(&body)),
        (400, "ValidationException"),
        "case version without a name or a source: expected a 400 ValidationException, got {body}"
    );
    assert_ne!(
        actual, APPLICATION_SOURCES_CONFLICT,
        "case version without a name or a source: the application-source conflict text must \
         not be reported for it, got {actual:?}"
    );
}

/// A minimum capacity below the floor of the band is refused, on every write that carries
/// scaling instructions, in AWS's own words.
///
/// `ScalingInstruction$MinCapacity` and `$MaxCapacity` both target `ResourceCapacity`, an
/// integer the model gives no `@range`
/// (`smithy_query(query="shape",shape="ResourceCapacity,ScalingInstruction")`), so the
/// service states the band's rules itself: the floor of the band is one unit, and the
/// maximum does not sit below the minimum. The rows differ along what the rule generalises
/// over — how far below the floor the minimum is (one unit, one negative unit, far down,
/// the smallest value the member's type carries), whether the maximum is above the floor,
/// below it or below the minimum as well, WHICH instruction in the list carries the band,
/// and which operation carries the list — and every one of them is answered HTTP 400,
/// `ValidationException`, with the fixed sentence for that error class.
#[tokio::test]
async fn a_minimum_capacity_below_the_floor_is_refused_in_awss_words() {
    let emulator = with_one_stored_plan().await;

    let below_floor: Vec<(&str, i64, i64)> = vec![
        ("one unit below the floor", CAPACITY_FLOOR - 1, 10),
        ("a band of no width below the floor", 0, 0),
        ("a negative minimum", -1, 10),
        ("a far negative minimum", -1_000, 1_000),
        // `ResourceCapacity` is a Smithy `integer`, so this is the smallest value the
        // member can carry at all.
        ("the smallest minimum the member carries", i64::from(i32::MIN), 10),
        ("a whole band below the floor", -5, -1),
        ("below the floor and inverted as well", 0, -1),
    ];

    for (label, min, max) in below_floor {
        for position in [0usize, 1] {
            let instructions = instructions_with_band(position, min, max);
            let writes: Vec<(&str, Value)> = vec![
                (
                    "CreateScalingPlan",
                    json!({
                        "ScalingPlanName": REFUSED_PLAN,
                        "ApplicationSource": stack_source(9),
                        "ScalingInstructions": instructions,
                    }),
                ),
                (
                    "UpdateScalingPlan",
                    json!({
                        "ScalingPlanName": STORED_PLAN,
                        "ScalingPlanVersion": ONLY_VERSION,
                        "ScalingInstructions": instructions,
                    }),
                ),
            ];
            for (operation, request) in writes {
                let (status, body) = emulator.call(operation, &request).await;
                assert_eq!(
                    status, 400,
                    "case {operation}/{label} ({min}..{max}) at instruction {position}: \
                     expected HTTP 400, got {status} with {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "ValidationException",
                    "case {operation}/{label} ({min}..{max}) at instruction {position}: \
                     expected a ValidationException, got {body}"
                );
                let actual = error_message(&body);
                assert_eq!(
                    actual, MINIMUM_CAPACITY_BELOW_FLOOR,
                    "case {operation}/{label} ({min}..{max}) at instruction {position}: \
                     expected {MINIMUM_CAPACITY_BELOW_FLOOR:?}, got {actual:?}"
                );
            }
        }
    }

    // A refused write leaves the store as it was: no plan under the name the refused
    // creates named, and the band the stored plan was created with still in force.
    let missing = emulator
        .ok(
            "DescribeScalingPlans",
            json!({ "ScalingPlanNames": [REFUSED_PLAN] }),
        )
        .await;
    assert_eq!(
        missing["ScalingPlans"],
        json!([]),
        "case refused creates: no plan may be recorded under {REFUSED_PLAN:?}, got {missing}"
    );
    let described = emulator
        .ok(
            "DescribeScalingPlans",
            json!({ "ScalingPlanNames": [STORED_PLAN] }),
        )
        .await;
    let stored_band = &described["ScalingPlans"][0]["ScalingInstructions"][0];
    assert_eq!(
        (
            stored_band.get("MinCapacity"),
            stored_band.get("MaxCapacity")
        ),
        (Some(&json!(1)), Some(&json!(10))),
        "case refused updates: the stored band must be the one the create set, got {described}"
    );

    // The OTHER relation between the two members keeps its own sentence, so the floor
    // refusal is about the minimum alone and not about every capacity pair the service
    // turns down.
    for (label, min, max) in [
        ("inverted by one unit at the floor", CAPACITY_FLOOR, 0),
        ("inverted by one unit above the floor", 5, 4),
        ("inverted far above the floor", 10, 2),
    ] {
        let (status, body) = emulator
            .call(
                "CreateScalingPlan",
                &json!({
                    "ScalingPlanName": REFUSED_PLAN,
                    "ApplicationSource": stack_source(9),
                    "ScalingInstructions": instructions_with_band(0, min, max),
                }),
            )
            .await;
        let actual = error_message(&body);
        assert_eq!(
            (status, error_code(&body)),
            (400, "ValidationException"),
            "case {label} ({min}..{max}): expected a 400 ValidationException, got {body}"
        );
        assert_eq!(
            actual, MAXIMUM_CAPACITY_BELOW_MINIMUM,
            "case {label} ({min}..{max}): expected {MAXIMUM_CAPACITY_BELOW_MINIMUM:?}, got \
             {actual:?}"
        );
    }

    // The control: a minimum AT the floor, and above it, is accepted \u2014 the rule refuses the
    // values below the floor, not the member.
    for (n, (label, min, max)) in [
        ("a band at the floor", CAPACITY_FLOOR, CAPACITY_FLOOR),
        ("a wide band from the floor", CAPACITY_FLOOR, 10_000),
        ("a minimum far above the floor", 1_000, 2_000),
    ]
    .into_iter()
    .enumerate()
    {
        let accepted = Emulator::new();
        let name = format!("accepted-{n}");
        let created = accepted
            .ok(
                "CreateScalingPlan",
                json!({
                    "ScalingPlanName": name,
                    "ApplicationSource": stack_source(20 + n),
                    "ScalingInstructions": instructions_with_band(0, min, max),
                }),
            )
            .await;
        assert_eq!(
            created.get("ScalingPlanVersion"),
            Some(&json!(1)),
            "case {label} ({min}..{max}): expected the plan to be created at version 1, got \
             {created}"
        );
        accepted.make_all_due();
        let described = accepted
            .ok("DescribeScalingPlans", json!({ "ScalingPlanNames": [name] }))
            .await;
        let band = &described["ScalingPlans"][0]["ScalingInstructions"][0];
        assert_eq!(
            (band.get("MinCapacity"), band.get("MaxCapacity")),
            (Some(&json!(min)), Some(&json!(max))),
            "case {label} ({min}..{max}): the accepted band did not round-trip, got {described}"
        );
    }
}
