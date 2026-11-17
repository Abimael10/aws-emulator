// SPDX-License-Identifier: CC0-1.0

//! What a write of one patch baseline persists and a read of it reports, end to end:
//! `CreatePatchBaseline` mints a baseline, `UpdatePatchBaseline` writes it,
//! `GetPatchBaseline` reports what the last write left behind, and
//! `DescribeEffectivePatchesForPatchBaseline` reports the patches that baseline makes
//! effective, each with the approval state the baseline gives it.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call_in_region`], [`error_code`],
//! [`error_message`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a response
//! that is not computed from persisted state cannot pass: every read follows a write.
//!
//! The rule under test is that a read of a baseline reports, for the `BaselineId` the
//! request names, exactly the members the write of that baseline persisted — with the
//! documented default of every member the write left out — and refuses an id that names no
//! baseline reachable through the endpoint. An update is such a write: it applies exactly
//! the members the request supplies and leaves every other member as the last write of the
//! baseline persisted, except under `Replace`, where a member the request leaves out is
//! cleared to the value a create that named it nothing would carry. The suites are tables
//! whose rows differ along what the rule generalises over: which optional members a write
//! supplies, how many entries each list carries, the operating system, the Region the
//! endpoint is, which spelling names the baseline (its own id or its full ARN), whether the
//! baseline is the account's own or one Systems Manager predefined, whether an update
//! replaces or merges, and the way an id misses.
//!
//! The effective-patch suites at the end pin the second half of that rule: which of the
//! patches Patch Manager publishes for the baseline's operating system the baseline makes
//! effective, and the approval state — "whether the patch is currently approved, due to be
//! approved by a rule, explicitly approved, or explicitly rejected and the date the patch
//! was or will be approved" (`EffectivePatch`) — that the baseline's own persisted
//! filters, rules and patch lists give each of them. Their rows differ along the window a
//! rule states, the criteria it selects on, which list names a patch, the compliance level
//! the baseline assigns, the page size a request asks for and the way a read has nothing
//! to report.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, ACCOUNT, REGION};

/// The `OperatingSystem` values the model's enum declares
/// (`smithy_query(query="shape", shape="OperatingSystem")`).
const OPERATING_SYSTEMS: &[&str] = &[
    "ALMA_LINUX",
    "AMAZON_LINUX",
    "AMAZON_LINUX_2",
    "AMAZON_LINUX_2022",
    "AMAZON_LINUX_2023",
    "CENTOS",
    "DEBIAN",
    "MACOS",
    "ORACLE_LINUX",
    "RASPBIAN",
    "REDHAT_ENTERPRISE_LINUX",
    "ROCKY_LINUX",
    "SUSE",
    "UBUNTU",
    "WINDOWS",
];

/// The operating system a baseline whose create named none applies to: "Defines the
/// operating system the patch baseline applies to. The default value is WINDOWS"
/// (`smithy_query(query="doc", shape="CreatePatchBaselineRequest",
/// member="OperatingSystem")`).
const DEFAULT_OPERATING_SYSTEM: &str = "WINDOWS";

/// The compliance level of a baseline whose create named none: "The default value is
/// UNSPECIFIED" (`CreatePatchBaselineRequest$ApprovedPatchesComplianceLevel`).
const DEFAULT_COMPLIANCE_LEVEL: &str = "UNSPECIFIED";

/// What Patch Manager does with a rejected patch when the create named no action: "This is
/// the default action if no option is specified"
/// (`CreatePatchBaselineRequest$RejectedPatchesAction`).
const DEFAULT_REJECTED_PATCHES_ACTION: &str = "ALLOW_AS_DEPENDENCY";

/// The published ceiling of `ApprovedPatches` and `RejectedPatches`
/// (`smithy_query(query="shape", shape="PatchIdList")`: `length 0..50`).
const MAX_PATCH_IDS: usize = 50;

/// The published ceiling of a rule group's own list (`smithy_query(query="shape",
/// shape="PatchRuleGroup")`: `PatchRules`, `length 0..10`).
const MAX_PATCH_RULES: usize = 10;

/// The published ceiling of `Sources` (`smithy_query(query="shape",
/// shape="PatchSourceList")`: `length 0..20`).
const MAX_SOURCES: usize = 20;

/// The sentence `DoesNotExistException` carries for a `BaselineId` that names no patch
/// baseline reachable through the endpoint: fixed text for this error class, naming the
/// resource class and interpolating nothing from the request. Spelled out here rather than
/// read from the crate, so the wording itself is what the assertions check.
const NOT_FOUND_MESSAGE: &str = "Patch Baseline to be retrieved does not exist.";

/// `CreatePatchBaseline` at `region`'s endpoint with `body`.
async fn create(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(router, region, "CreatePatchBaseline", body).await
}

/// The same write, asserting it succeeded, and returning the `BaselineId` it minted.
async fn create_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: &Value,
) -> String {
    let (status, response) = create(router, region, body).await;
    assert_eq!(
        status, 200,
        "case {case}: CreatePatchBaseline {body} must succeed, got {status} {response}"
    );
    response["BaselineId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a create reports a BaselineId, got {response}"))
        .to_string()
}

/// `GetPatchBaseline` at `region`'s endpoint for `baseline_id`.
async fn get(router: &MultiServiceRouter, region: &str, baseline_id: &str) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "GetPatchBaseline",
        &json!({ "BaselineId": baseline_id }),
    )
    .await
}

/// The same read, asserting it succeeded.
async fn get_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    baseline_id: &str,
) -> Value {
    let (status, response) = get(router, region, baseline_id).await;
    assert_eq!(
        status, 200,
        "case {case}: GetPatchBaseline {baseline_id:?} must succeed, got {status} {response}"
    );
    response
}

/// `GetDefaultPatchBaseline` at `region`'s endpoint for `operating_system`, returning the
/// `BaselineId` it reports — the full ARN of the baseline Systems Manager predefines while
/// nothing has registered another.
async fn predefined_baseline_id(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operating_system: &str,
) -> String {
    let (status, response) = call_in_region(
        router,
        region,
        "GetDefaultPatchBaseline",
        &json!({ "OperatingSystem": operating_system }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: GetDefaultPatchBaseline must succeed, got {status} {response}"
    );
    response["BaselineId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a read reports a BaselineId, got {response}"))
        .to_string()
}

/// The ARN of the patch baseline `baseline_id` names in `region`, the form the account's
/// own ARNs carry.
fn baseline_arn(region: &str, baseline_id: &str) -> String {
    format!("arn:aws:ssm:{region}:{ACCOUNT}:patchbaseline/{baseline_id}")
}

/// One approval rule selecting patches of `classification`, approving them after a day —
/// the shape `PatchRule` requires: a filter group, plus "a value for either
/// `ApproveAfterDays` or `ApproveUntilDate`".
fn patch_rule(classification: &str) -> Value {
    json!({
        "PatchFilterGroup": {
            "PatchFilters": [{ "Key": "CLASSIFICATION", "Values": [classification] }],
        },
        "ApproveAfterDays": 1,
        "ComplianceLevel": "HIGH",
        "EnableNonSecurity": true,
    })
}

/// One approval rule carrying only what `PatchRule` requires plus the one member "your
/// request must include a value for" — so a read of it reports the default of every member
/// it left out.
fn bare_patch_rule(classification: &str) -> Value {
    json!({
        "PatchFilterGroup": {
            "PatchFilters": [{ "Key": "CLASSIFICATION", "Values": [classification] }],
        },
        "ApproveAfterDays": 7,
    })
}

/// A rule group of `count` rules, each selecting its own classification.
fn rule_group(count: usize) -> Value {
    // The classifications a Windows patch filter accepts, from `PatchFilter`'s own doc
    // (`smithy_query(query="doc", shape="PatchFilter")`).
    const CLASSIFICATIONS: &[&str] = &[
        "CriticalUpdates",
        "SecurityUpdates",
        "DefinitionUpdates",
        "Drivers",
        "FeaturePacks",
        "ServicePacks",
        "Tools",
        "UpdateRollups",
        "Updates",
        "Upgrades",
    ];
    json!({
        "PatchRules": (0..count)
            .map(|index| patch_rule(CLASSIFICATIONS[index % CLASSIFICATIONS.len()]))
            .collect::<Vec<Value>>(),
    })
}

/// `count` patch ids starting at `first`, in the `KB…` form a Windows patch id takes.
fn patch_ids(first: usize, count: usize) -> Value {
    json!((0..count)
        .map(|index| format!("KB{:07}", first + index))
        .collect::<Vec<String>>())
}

/// `count` patch sources, each with the three members `PatchSource` requires.
fn sources(count: usize) -> Value {
    json!((0..count)
        .map(|index| json!({
            "Name": format!("Repo-{index}"),
            "Products": [format!("Ubuntu22.{index:02}")],
            "Configuration": format!("deb http://repo-{index}.example.invalid/ubuntu jammy main"),
        }))
        .collect::<Vec<Value>>())
}

/// What a read of the baseline a create with `body` minted must report, member by member:
/// the members the create supplied, the documented default of each member it left out, and
/// `Null` — reported by nothing at all — for a member with neither.
fn expected_members(body: &Value) -> Map<String, Value> {
    let supplied = |member: &str| body.get(member).cloned().unwrap_or(Value::Null);
    let defaulted = |member: &str, default: Value| match body.get(member) {
        Some(value) => value.clone(),
        None => default,
    };
    let mut expected = Map::new();
    expected.insert("Name".to_string(), supplied("Name"));
    // A set of filters and a set of rules the write named none of are reported as the
    // EMPTY set, and every member of a rule the write left out at its own default.
    expected.insert(
        "GlobalFilters".to_string(),
        defaulted("GlobalFilters", json!({ "PatchFilters": [] })),
    );
    expected.insert(
        "ApprovalRules".to_string(),
        expected_approval_rules(body.get("ApprovalRules")),
    );
    expected.insert(
        "OperatingSystem".to_string(),
        defaulted("OperatingSystem", json!(DEFAULT_OPERATING_SYSTEM)),
    );
    expected.insert(
        "ApprovedPatches".to_string(),
        defaulted("ApprovedPatches", json!([])),
    );
    expected.insert(
        "ApprovedPatchesComplianceLevel".to_string(),
        defaulted(
            "ApprovedPatchesComplianceLevel",
            json!(DEFAULT_COMPLIANCE_LEVEL),
        ),
    );
    expected.insert(
        "ApprovedPatchesEnableNonSecurity".to_string(),
        defaulted("ApprovedPatchesEnableNonSecurity", json!(false)),
    );
    expected.insert(
        "RejectedPatches".to_string(),
        defaulted("RejectedPatches", json!([])),
    );
    expected.insert(
        "RejectedPatchesAction".to_string(),
        defaulted("RejectedPatchesAction", json!(DEFAULT_REJECTED_PATCHES_ACTION)),
    );
    expected.insert("Sources".to_string(), defaulted("Sources", json!([])));
    // A patch group is included in a baseline by registering it there, and this create
    // registered none.
    expected.insert("PatchGroups".to_string(), json!([]));
    // The members with no documented default: reported exactly when the create supplied
    // one, and reported by nothing when it did not.
    for member in ["Description", "AvailableSecurityUpdatesComplianceStatus"] {
        expected.insert(member.to_string(), supplied(member));
    }
    expected
}

/// The rule group a read reports for a create that supplied `supplied`: every rule as it
/// was written, with `ComplianceLevel` at the unspecified level and `EnableNonSecurity` at
/// false for a rule that named neither (`PatchRule$EnableNonSecurity`: "the default value
/// is false"), and the empty group when the create supplied none.
fn expected_approval_rules(supplied: Option<&Value>) -> Value {
    let rules = supplied
        .and_then(|rules| rules.get("PatchRules"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "PatchRules": rules
            .into_iter()
            .map(|rule| {
                let mut rule = rule;
                if rule.get("ComplianceLevel").is_none() {
                    rule["ComplianceLevel"] = json!(DEFAULT_COMPLIANCE_LEVEL);
                }
                if rule.get("EnableNonSecurity").is_none() {
                    rule["EnableNonSecurity"] = json!(false);
                }
                rule
            })
            .collect::<Vec<Value>>(),
    })
}

/// One row of the round-trip table: the body one create carries, at one Region.
struct RoundTripCase<'a> {
    case: &'a str,
    region: &'a str,
    body: Value,
}

/// A read reports every member the create persisted, and the documented default of every
/// member the create left out — whichever members the create carried, however many entries
/// they hold, for whichever operating system and at whichever Region.
#[tokio::test]
async fn a_read_reports_every_member_the_create_persisted() {
    let cases = [
        RoundTripCase {
            case: "only the members a create must carry",
            region: REGION,
            body: json!({
                "Name": "Minimal.Baseline",
                "ApprovedPatches": patch_ids(2_124_261, 1),
            }),
        },
        RoundTripCase {
            case: "every member a create may carry",
            region: REGION,
            body: json!({
                "Name": "Fully.Specified.Baseline",
                "OperatingSystem": "AMAZON_LINUX_2023",
                "Description": "a baseline whose every member is supplied",
                "GlobalFilters": {
                    "PatchFilters": [{ "Key": "PRODUCT", "Values": ["AmazonLinux2023"] }],
                },
                "ApprovalRules": rule_group(2),
                "ApprovedPatches": patch_ids(1_000_000, 3),
                "ApprovedPatchesComplianceLevel": "CRITICAL",
                "ApprovedPatchesEnableNonSecurity": true,
                "RejectedPatches": patch_ids(2_000_000, 2),
                "RejectedPatchesAction": "BLOCK",
                "Sources": sources(1),
                "AvailableSecurityUpdatesComplianceStatus": "NON_COMPLIANT",
                "Tags": [{ "Key": "Owner", "Value": "SysAdmin" }],
            }),
        },
        RoundTripCase {
            case: "approval by rule alone, at another Region",
            region: "eu-central-1",
            body: json!({
                "Name": "Rules.Only.Baseline",
                "OperatingSystem": "UBUNTU",
                "ApprovalRules": rule_group(1),
            }),
        },
        RoundTripCase {
            case: "every list filled to its published ceiling",
            region: "ap-southeast-1",
            body: json!({
                "Name": "Full.Lists.Baseline",
                "OperatingSystem": "MACOS",
                "ApprovedPatches": patch_ids(3_000_000, MAX_PATCH_IDS),
                "RejectedPatches": patch_ids(4_000_000, MAX_PATCH_IDS),
                "ApprovalRules": rule_group(MAX_PATCH_RULES),
                "Sources": sources(MAX_SOURCES),
            }),
        },
        RoundTripCase {
            case: "an empty approved-patch list beside one rule",
            region: REGION,
            body: json!({
                "Name": "Empty.List.Baseline",
                "OperatingSystem": "SUSE",
                "ApprovedPatches": [],
                "RejectedPatches": [],
                "Sources": [],
                "ApprovalRules": rule_group(1),
                "AvailableSecurityUpdatesComplianceStatus": "COMPLIANT",
            }),
        },
        RoundTripCase {
            case: "rules that name neither their level nor their non-security flag",
            region: REGION,
            body: json!({
                "Name": "Bare.Rules.Baseline",
                "OperatingSystem": "AMAZON_LINUX_2023",
                "ApprovalRules": {
                    "PatchRules": [
                        bare_patch_rule("Security"),
                        bare_patch_rule("Bugfix"),
                    ],
                },
            }),
        },
        RoundTripCase {
            case: "a description at its published floor",
            region: REGION,
            body: json!({
                "Name": "Short.Description.Baseline",
                "OperatingSystem": "WINDOWS",
                "Description": "a",
                "ApprovedPatches": patch_ids(5_000_000, 1),
            }),
        },
    ];

    for RoundTripCase { case, region, body } in &cases {
        let router = router();
        let baseline_id = create_ok(&router, case, region, body).await;
        let reported = get_ok(&router, case, region, &baseline_id).await;

        assert_eq!(
            reported["BaselineId"].as_str(),
            Some(baseline_id.as_str()),
            "case {case}: the read reports the id the create minted {baseline_id:?}, got \
             {reported}"
        );
        for (member, expected) in expected_members(body) {
            assert_eq!(
                reported[&member], expected,
                "case {case}: member {member} must be {expected}, got {}",
                reported[&member]
            );
        }

        // "The date the patch baseline was created" and "the date the patch baseline was
        // last modified": nothing has written the baseline since its create, so the two
        // are the one instant that create ran at.
        let created = reported["CreatedDate"].as_f64().unwrap_or_else(|| {
            panic!("case {case}: the read reports a CreatedDate, got {reported}")
        });
        let modified = reported["ModifiedDate"].as_f64().unwrap_or_else(|| {
            panic!("case {case}: the read reports a ModifiedDate, got {reported}")
        });
        assert_eq!(
            created, modified,
            "case {case}: an unmodified baseline was last written when it was created \
             ({created}), got {modified}"
        );
        assert!(
            created > 0.0,
            "case {case}: the created date is the instant the create ran, got {created}"
        );

        // A read writes nothing: a second read reports exactly what the first one did.
        let again = get_ok(&router, case, region, &baseline_id).await;
        assert_eq!(
            again, reported,
            "case {case}: a read must not change what a later read of {baseline_id:?} \
             reports, got {again} after {reported}"
        );
    }
}

/// A baseline the account created is read by its own id and by its full ARN alike: both
/// spellings report the same baseline, at whichever Region holds it.
#[tokio::test]
async fn either_spelling_of_a_held_baseline_reads_the_same_baseline() {
    let cases = [
        ("the endpoint's own Region", REGION, "WINDOWS"),
        ("another Region", "eu-central-1", "AMAZON_LINUX_2"),
        ("a third Region", "ap-northeast-1", "DEBIAN"),
    ];

    for (case, region, operating_system) in cases {
        let router = router();
        let body = json!({
            "Name": "Either.Spelling.Baseline",
            "OperatingSystem": operating_system,
            "ApprovedPatches": patch_ids(2_124_261, 2),
        });
        let baseline_id = create_ok(&router, case, region, &body).await;

        let by_id = get_ok(&router, case, region, &baseline_id).await;
        let by_arn = get_ok(&router, case, region, &baseline_arn(region, &baseline_id)).await;
        assert_eq!(
            by_arn, by_id,
            "case {case}: the ARN of {baseline_id:?} reads the same baseline its id does, \
             got {by_arn} and {by_id}"
        );
        assert_eq!(
            by_id["OperatingSystem"].as_str(),
            Some(operating_system),
            "case {case}: the read reports the operating system the create named, got {by_id}"
        );
    }
}

/// Every operating system's predefined baseline is readable by the full ARN a read of the
/// default reports — "to retrieve information about an Amazon Web Services managed patch
/// baseline, specify the full Amazon Resource Name (ARN) of the baseline" — and reports the
/// name Patch Manager gives it and the operating system it applies to.
#[tokio::test]
async fn a_predefined_baseline_is_read_by_the_arn_that_names_it() {
    let router = router();
    let mut reported_names: Vec<(&str, String)> = Vec::new();
    for operating_system in OPERATING_SYSTEMS {
        let case = *operating_system;
        let arn = predefined_baseline_id(&router, case, REGION, operating_system).await;
        let reported = get_ok(&router, case, REGION, &arn).await;
        assert_eq!(
            reported["BaselineId"].as_str(),
            Some(arn.as_str()),
            "case {case}: a predefined baseline is named by its full ARN {arn:?}, got \
             {reported}"
        );
        assert_eq!(
            reported["OperatingSystem"].as_str(),
            Some(*operating_system),
            "case {case}: the read reports the operating system the baseline applies to, \
             got {reported}"
        );
        let name = reported["Name"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the read reports a Name, got {reported}"))
            .to_string();
        assert!(
            name.starts_with("AWS-") && name.ends_with("DefaultPatchBaseline"),
            "case {case}: a predefined baseline carries the name Patch Manager gives it \
             (AWS-…DefaultPatchBaseline), got {name:?}"
        );
        reported_names.push((operating_system, name));
    }

    for (case, name) in &reported_names {
        let collisions = reported_names
            .iter()
            .filter(|(_, held)| held == name)
            .count();
        assert_eq!(
            collisions, 1,
            "case {case}: each operating system has its own predefined baseline, but \
             {name:?} is reported for {collisions} of them"
        );
    }
}

/// A `BaselineId` that is well formed but names no baseline reachable through the endpoint
/// is refused as absent, with the status, the error class and the sentence Patch Manager
/// words — whichever way the id misses.
#[tokio::test]
async fn a_read_of_an_unheld_baseline_is_refused_as_absent() {
    let router = router();
    let held = create_ok(
        &router,
        "the held baseline",
        REGION,
        &json!({
            "Name": "Held.Baseline",
            "OperatingSystem": "SUSE",
            "ApprovedPatches": patch_ids(2_124_261, 1),
        }),
    )
    .await;
    // A baseline of ANOTHER Region's endpoint: a baseline is reached through the endpoint
    // of the one Region it was created in.
    let elsewhere = create_ok(
        &router,
        "the baseline of another Region",
        "eu-central-1",
        &json!({
            "Name": "Elsewhere.Baseline",
            "OperatingSystem": "SUSE",
            "ApprovedPatches": patch_ids(2_124_261, 1),
        }),
    )
    .await;

    // One hex digit of the held id changed, so the value is as well formed as the held one
    // and names nothing.
    let mut near_miss: Vec<char> = held.chars().collect();
    let last = near_miss.len() - 1;
    near_miss[last] = if near_miss[last] == 'f' { '0' } else { 'f' };
    let near_miss: String = near_miss.into_iter().collect();

    let cases: Vec<(&str, String)> = vec![
        ("an id no create minted", near_miss),
        ("a baseline of another Region", elsewhere.clone()),
        (
            "the ARN of a baseline of another Region, at this endpoint",
            baseline_arn(REGION, &elsewhere),
        ),
        (
            "an id at the published floor of the member",
            "pb-00000000000000000".to_string(),
        ),
        (
            "a name-like value no baseline carries",
            "AWS-DefaultPatchBaselineXXX".to_string(),
        ),
    ];
    for (case, baseline_id) in cases {
        let (status, response) = get(&router, REGION, &baseline_id).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {case}: expected 400 DoesNotExistException for {baseline_id:?}, got \
             {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            NOT_FOUND_MESSAGE,
            "case {case}: expected the sentence Patch Manager words, got {response}"
        );

        // The refused read changed nothing: the baseline this endpoint holds still reads.
        let reported = get_ok(&router, case, REGION, &held).await;
        assert_eq!(
            reported["Name"].as_str(),
            Some("Held.Baseline"),
            "case {case}: the held baseline still reads, got {reported}"
        );
    }
}

/// A `BaselineId` that could never name a baseline held at this endpoint — an ARN of
/// another service, of another account, of another Region, or of another resource type — is
/// refused as an invalid resource id rather than resolved.
#[tokio::test]
async fn a_read_of_an_id_this_endpoint_could_never_hold_is_refused_as_invalid() {
    let router = router();
    let held = create_ok(
        &router,
        "the held baseline",
        REGION,
        &json!({
            "Name": "Invalid.Id.Baseline",
            "OperatingSystem": "WINDOWS",
            "ApprovedPatches": patch_ids(2_124_261, 1),
        }),
    )
    .await;

    let cases: Vec<(&str, String)> = vec![
        (
            "an ARN of another account",
            format!("arn:aws:ssm:{REGION}:210987654321:patchbaseline/{held}"),
        ),
        (
            "an ARN of another Region",
            format!("arn:aws:ssm:eu-central-1:{ACCOUNT}:patchbaseline/{held}"),
        ),
        (
            "an ARN of another service",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:patchbaseline/{held}"),
        ),
        (
            "an ARN of another resource type",
            format!("arn:aws:ssm:{REGION}:{ACCOUNT}:document/{held}"),
        ),
    ];
    for (case, baseline_id) in cases {
        let (status, response) = get(&router, REGION, &baseline_id).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "InvalidResourceId"),
            "case {case}: expected 400 InvalidResourceId for {baseline_id:?}, got {status} \
             {response}"
        );

        let reported = get_ok(&router, case, REGION, &held).await;
        assert_eq!(
            reported["BaselineId"].as_str(),
            Some(held.as_str()),
            "case {case}: the held baseline still reads, got {reported}"
        );
    }
}

/// A `BaselineId` outside the published bounds of the member is refused by the boundary
/// walk before any lookup: below its floor, above its ceiling, and built from characters
/// its pattern does not admit.
#[tokio::test]
async fn a_baseline_id_outside_the_modelled_member_is_refused() {
    // The published bounds of `BaselineId` (`smithy_query(query="shape",
    // shape="BaselineId")`: `length 20..128`, pattern `^[a-zA-Z0-9_\-:/]{20,128}$`).
    const MIN: usize = 20;
    const MAX: usize = 128;

    let router = router();
    let cases: Vec<(&str, String)> = vec![
        ("one short of the floor", format!("pb-{}", "0".repeat(MIN - 4))),
        ("one past the ceiling", format!("pb-{}", "0".repeat(MAX - 2))),
        ("a character the pattern excludes", format!("pb-{}", "a.".repeat(MIN))),
    ];
    for (case, baseline_id) in cases {
        let (status, response) = get(&router, REGION, &baseline_id).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case}: expected 400 ValidationException for {baseline_id:?}, got \
             {status} {response}"
        );
    }

    // Both published bounds themselves are values the member allows, so they reach the
    // lookup and are answered as absent rather than as malformed.
    let at_bounds = [
        ("at the floor", "0".repeat(MIN - 3)),
        ("at the ceiling", "0".repeat(MAX - 3)),
    ];
    for (case, digits) in at_bounds {
        let baseline_id = format!("pb-{digits}");
        let (status, response) = get(&router, REGION, &baseline_id).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {case}: a {} character id the pattern admits reaches the lookup, got \
             {status} {response}",
            baseline_id.len()
        );
    }
}

// ---------------------------------------------------------------------------
// What an update of one baseline writes
// ---------------------------------------------------------------------------

/// The sentence a write that would leave a baseline approving no patch at all is refused
/// with: fixed text naming the two attributes, and nothing of the request.
const APPROVAL_COVERAGE_MESSAGE: &str = "Patch Baseline attribute: Approved Patches and \
     Approval Rule, at least one must be non empty.";

/// The sentence an update that asks to REPLACE a baseline without naming one is refused
/// with: `Replace` makes every member `CreatePatchBaseline` requires required here too, and
/// the one such member is `Name`, so the refusal is the clause this service states over a
/// member that must not be null, in the envelope its refusals carry.
const REPLACE_WITHOUT_NAME_MESSAGE: &str = "1 validation error detected: Value null at \
     'name' failed to satisfy constraint: Member must not be null";

/// Every member of a patch baseline an update may write, in the order the model lists them
/// (`smithy_query(query="operation", operation="UpdatePatchBaseline")`). `Name` is the one
/// `CreatePatchBaseline` also requires; the rest are the optional members `Replace` clears.
const UPDATABLE_MEMBERS: &[&str] = &[
    "Name",
    "GlobalFilters",
    "ApprovalRules",
    "ApprovedPatches",
    "ApprovedPatchesComplianceLevel",
    "ApprovedPatchesEnableNonSecurity",
    "RejectedPatches",
    "RejectedPatchesAction",
    "Description",
    "Sources",
    "AvailableSecurityUpdatesComplianceStatus",
];

/// The member a read reports that an update's own answer does not: a patch group is
/// included in a baseline by registering it there, which is no part of this write.
const READ_ONLY_MEMBER: &str = "PatchGroups";

/// `UpdatePatchBaseline` at `region`'s endpoint for `baseline_id` with `body`'s members.
async fn update(
    router: &MultiServiceRouter,
    region: &str,
    baseline_id: &str,
    body: &Value,
) -> (u16, Value) {
    let mut request = body.clone();
    request["BaselineId"] = json!(baseline_id);
    call_in_region(router, region, "UpdatePatchBaseline", &request).await
}

/// The write a successful update leaves behind: the members `update` supplied, and for every
/// member it left out either what `created` persisted or — under `Replace` — nothing at all.
///
/// This is the rule stated in the test's own terms: "fields not specified in the request are
/// left unchanged", and with `Replace` true "optional fields that aren't specified are set to
/// null". The operating system is not an update member, so it stays whatever the create
/// carried, and what the merged write leaves out is what [`expected_members`] reports at its
/// documented default.
fn merged_body(created: &Value, update: &Value) -> Value {
    let replace = update["Replace"].as_bool().unwrap_or(false);
    let mut merged = Map::new();
    if let Some(operating_system) = created.get("OperatingSystem") {
        merged.insert("OperatingSystem".to_string(), operating_system.clone());
    }
    for member in UPDATABLE_MEMBERS {
        let supplied = match update.get(*member) {
            Some(value) => Some(value),
            None if replace => None,
            None => created.get(*member),
        };
        if let Some(value) = supplied {
            merged.insert((*member).to_string(), value.clone());
        }
    }
    Value::Object(merged)
}

/// The epoch seconds a reported instant carries.
fn instant(case: &str, reported: &Value, member: &str) -> f64 {
    reported[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case}: the answer reports a {member}, got {reported}"))
}

/// One row of the update table: what one create carried, what one update of it carries, at
/// one Region.
struct UpdateCase<'a> {
    case: &'a str,
    region: &'a str,
    created: Value,
    update: Value,
}

/// An update reports — and persists — the members it supplied, every other member as the
/// last write of the baseline persisted it, and under `Replace` the documented default of
/// every member it left out: whichever members the update carries, however many entries
/// they hold, for whichever operating system and at whichever Region.
#[tokio::test]
async fn an_update_writes_the_members_it_supplies_and_leaves_the_rest() {
    let cases = [
        UpdateCase {
            case: "nothing but the baseline's id",
            region: REGION,
            created: json!({
                "Name": "Untouched.Baseline",
                "ApprovedPatches": patch_ids(2_124_261, 1),
            }),
            update: json!({}),
        },
        UpdateCase {
            case: "the name alone",
            region: REGION,
            created: json!({
                "Name": "Before.Rename",
                "Description": "a baseline whose description the update leaves alone",
                "ApprovedPatches": patch_ids(1_000_000, 2),
                "ApprovedPatchesComplianceLevel": "CRITICAL",
            }),
            update: json!({ "Name": "After.Rename" }),
        },
        UpdateCase {
            case: "a description where the create carried none",
            region: REGION,
            created: json!({
                "Name": "Describe.Me",
                "OperatingSystem": "AMAZON_LINUX_2023",
                "ApprovalRules": rule_group(1),
            }),
            update: json!({ "Description": "a description the update added" }),
        },
        UpdateCase {
            case: "a longer approved-patch list and another compliance level",
            region: REGION,
            created: json!({
                "Name": "Approvals.Grow",
                "ApprovedPatches": patch_ids(2_000_000, 1),
            }),
            update: json!({
                "ApprovedPatches": patch_ids(2_100_000, 3),
                "ApprovedPatchesComplianceLevel": "MEDIUM",
            }),
        },
        UpdateCase {
            case: "a shorter rejected-patch list and another action",
            region: REGION,
            created: json!({
                "Name": "Rejections.Shrink",
                "ApprovedPatches": patch_ids(3_000_000, 1),
                "RejectedPatches": patch_ids(3_100_000, 4),
                "RejectedPatchesAction": "BLOCK",
            }),
            update: json!({
                "RejectedPatches": patch_ids(3_200_000, 1),
                "RejectedPatchesAction": "ALLOW_AS_DEPENDENCY",
            }),
        },
        UpdateCase {
            case: "a rule group where the create approved by name, clearing the name list",
            region: REGION,
            created: json!({
                "Name": "Rules.Replace.Names",
                "ApprovedPatches": patch_ids(4_000_000, 2),
            }),
            update: json!({
                "ApprovalRules": rule_group(2),
                "ApprovedPatches": [],
            }),
        },
        UpdateCase {
            case: "the global filters and the sources, on a Linux baseline",
            region: REGION,
            created: json!({
                "Name": "Filters.And.Sources",
                "OperatingSystem": "UBUNTU",
                "ApprovalRules": rule_group(1),
            }),
            update: json!({
                "GlobalFilters": {
                    "PatchFilters": [{ "Key": "PRODUCT", "Values": ["Ubuntu22.04"] }],
                },
                "Sources": sources(2),
            }),
        },
        UpdateCase {
            case: "the two flags a Windows baseline carries",
            region: REGION,
            created: json!({
                "Name": "Flags.Move",
                "ApprovedPatches": patch_ids(5_000_000, 1),
                "AvailableSecurityUpdatesComplianceStatus": "NON_COMPLIANT",
            }),
            update: json!({
                "ApprovedPatchesEnableNonSecurity": true,
                "AvailableSecurityUpdatesComplianceStatus": "COMPLIANT",
            }),
        },
        UpdateCase {
            case: "every member an update may carry, at another Region",
            region: "eu-central-1",
            created: json!({
                "Name": "Everything.Before",
                "OperatingSystem": "MACOS",
                "Description": "the create's description",
                "GlobalFilters": {
                    "PatchFilters": [{ "Key": "PRODUCT", "Values": ["MacOS13.1"] }],
                },
                "ApprovalRules": rule_group(1),
                "ApprovedPatches": patch_ids(6_000_000, 1),
                "ApprovedPatchesComplianceLevel": "LOW",
                "ApprovedPatchesEnableNonSecurity": false,
                "RejectedPatches": patch_ids(6_100_000, 1),
                "RejectedPatchesAction": "ALLOW_AS_DEPENDENCY",
                "Sources": sources(1),
                "AvailableSecurityUpdatesComplianceStatus": "COMPLIANT",
            }),
            update: json!({
                "Name": "Everything.After",
                "Description": "the update's description",
                "GlobalFilters": {
                    "PatchFilters": [{ "Key": "PRODUCT", "Values": ["MacOS14.0"] }],
                },
                "ApprovalRules": rule_group(3),
                "ApprovedPatches": patch_ids(6_200_000, 2),
                "ApprovedPatchesComplianceLevel": "INFORMATIONAL",
                "ApprovedPatchesEnableNonSecurity": true,
                "RejectedPatches": patch_ids(6_300_000, 3),
                "RejectedPatchesAction": "BLOCK",
                "Sources": sources(3),
                "AvailableSecurityUpdatesComplianceStatus": "NON_COMPLIANT",
            }),
        },
        UpdateCase {
            case: "every list filled to its published ceiling",
            region: "ap-southeast-1",
            created: json!({
                "Name": "Ceilings.Before",
                "OperatingSystem": "SUSE",
                "ApprovedPatches": patch_ids(7_000_000, 1),
            }),
            update: json!({
                "ApprovedPatches": patch_ids(7_100_000, MAX_PATCH_IDS),
                "RejectedPatches": patch_ids(7_200_000, MAX_PATCH_IDS),
                "ApprovalRules": rule_group(MAX_PATCH_RULES),
                "Sources": sources(MAX_SOURCES),
            }),
        },
        UpdateCase {
            case: "a merge asked for explicitly",
            region: REGION,
            created: json!({
                "Name": "Explicit.Merge",
                "Description": "a description an explicit merge keeps",
                "ApprovedPatches": patch_ids(8_000_000, 2),
            }),
            update: json!({ "Name": "Explicit.Merge.Renamed", "Replace": false }),
        },
        UpdateCase {
            case: "a replacing update carrying only the members a create must carry",
            region: REGION,
            created: json!({
                "Name": "Replace.Before",
                "OperatingSystem": "DEBIAN",
                "Description": "a description the replacement clears",
                "GlobalFilters": {
                    "PatchFilters": [{ "Key": "PRODUCT", "Values": ["Debian12"] }],
                },
                "ApprovalRules": rule_group(2),
                "ApprovedPatches": patch_ids(9_000_000, 2),
                "ApprovedPatchesComplianceLevel": "CRITICAL",
                "ApprovedPatchesEnableNonSecurity": true,
                "RejectedPatches": patch_ids(9_100_000, 2),
                "RejectedPatchesAction": "BLOCK",
                "Sources": sources(2),
                "AvailableSecurityUpdatesComplianceStatus": "NON_COMPLIANT",
            }),
            update: json!({
                "Name": "Replace.After",
                "ApprovedPatches": patch_ids(9_200_000, 1),
                "Replace": true,
            }),
        },
        UpdateCase {
            case: "a replacing update approving by rule alone",
            region: REGION,
            created: json!({
                "Name": "Replace.Rules.Before",
                "Description": "a description the replacement clears",
                "ApprovedPatches": patch_ids(10_000_000, 3),
                "Sources": sources(1),
            }),
            update: json!({
                "Name": "Replace.Rules.After",
                "ApprovalRules": rule_group(1),
                "Replace": true,
            }),
        },
        UpdateCase {
            case: "a replacing update carrying every member an update may carry",
            region: REGION,
            created: json!({
                "Name": "Replace.All.Before",
                "OperatingSystem": "REDHAT_ENTERPRISE_LINUX",
                "ApprovedPatches": patch_ids(11_000_000, 1),
            }),
            update: json!({
                "Name": "Replace.All.After",
                "Description": "a description the replacement sets",
                "GlobalFilters": {
                    "PatchFilters": [{ "Key": "PRODUCT", "Values": ["RedhatEnterpriseLinux9"] }],
                },
                "ApprovalRules": rule_group(2),
                "ApprovedPatches": patch_ids(11_100_000, 2),
                "ApprovedPatchesComplianceLevel": "HIGH",
                "ApprovedPatchesEnableNonSecurity": true,
                "RejectedPatches": patch_ids(11_200_000, 1),
                "RejectedPatchesAction": "BLOCK",
                "Sources": sources(1),
                "AvailableSecurityUpdatesComplianceStatus": "COMPLIANT",
                "Replace": true,
            }),
        },
    ];

    for UpdateCase {
        case,
        region,
        created,
        update: requested,
    } in &cases
    {
        let router = router();
        let baseline_id = create_ok(&router, case, region, created).await;
        let before = get_ok(&router, case, region, &baseline_id).await;

        let (status, answered) = update(&router, region, &baseline_id, requested).await;
        assert_eq!(
            status, 200,
            "case {case}: UpdatePatchBaseline {requested} must succeed, got {status} {answered}"
        );

        // What the update answered and what a later read reports are the one baseline the
        // update left behind.
        let reread = get_ok(&router, case, region, &baseline_id).await;
        for (member, expected) in expected_members(&merged_body(created, requested)) {
            if member != READ_ONLY_MEMBER {
                assert_eq!(
                    answered[&member], expected,
                    "case {case}: the update reports {member} as {expected}, got {}",
                    answered[&member]
                );
            }
            assert_eq!(
                reread[&member], expected,
                "case {case}: a read after the update reports {member} as {expected}, got {}",
                reread[&member]
            );
        }

        // The baseline's identity is not something an update writes: no member of the
        // request carries the id or the operating system, and the create is what the created
        // date records.
        assert_eq!(
            answered["BaselineId"].as_str(),
            Some(baseline_id.as_str()),
            "case {case}: the update reports the id it was given {baseline_id:?}, got {answered}"
        );
        assert_eq!(
            reread["BaselineId"], answered["BaselineId"],
            "case {case}: a read after the update reports the same id, got {reread}"
        );
        let created_at = instant(case, &before, "CreatedDate");
        assert_eq!(
            instant(case, &answered, "CreatedDate"),
            created_at,
            "case {case}: the update leaves the created date at {created_at}, got {answered}"
        );
        assert_eq!(
            instant(case, &reread, "CreatedDate"),
            created_at,
            "case {case}: a read after the update reports the created date {created_at}, got \
             {reread}"
        );

        // "The date when the patch baseline was last modified": the update wrote the
        // baseline, so it was last written no earlier than it was created, and both reports
        // of that instant are the one the update recorded.
        let modified_at = instant(case, &answered, "ModifiedDate");
        assert!(
            modified_at >= created_at,
            "case {case}: the baseline was last written at or after its create {created_at}, \
             got {modified_at}"
        );
        assert_eq!(
            instant(case, &reread, "ModifiedDate"),
            modified_at,
            "case {case}: a read after the update reports the instant the update recorded \
             ({modified_at}), got {reread}"
        );
    }
}

/// An update of a `BaselineId` that names no baseline this endpoint can write reports the
/// absent baseline and changes nothing — however the id misses.
#[tokio::test]
async fn an_update_of_a_baseline_this_endpoint_cannot_write_is_refused_as_absent() {
    // An account that is not the one this endpoint serves.
    const OTHER_ACCOUNT: &str = "210987654321";

    let router = router();
    let held = create_ok(
        &router,
        "the held baseline",
        REGION,
        &json!({
            "Name": "Update.Held.Baseline",
            "ApprovedPatches": patch_ids(12_000_000, 1),
        }),
    )
    .await;
    // A baseline Systems Manager predefines exists in every account and is Patch Manager's
    // own rather than the account's: this endpoint holds no record of it to write.
    let predefined =
        predefined_baseline_id(&router, "the predefined baseline", REGION, "WINDOWS").await;

    let cases: Vec<(&str, String)> = vec![
        (
            "a well-formed id at the published floor of the member",
            "pb-00000000000000000".to_string(),
        ),
        (
            "the ARN of the held baseline at another Region",
            format!("arn:aws:ssm:eu-west-1:{ACCOUNT}:patchbaseline/{held}"),
        ),
        (
            "the ARN of the held baseline in another account",
            format!("arn:aws:ssm:{REGION}:{OTHER_ACCOUNT}:patchbaseline/{held}"),
        ),
        (
            "an ARN whose resource is not a patch baseline",
            format!("arn:aws:ssm:{REGION}:{ACCOUNT}:document/AWS-RunShellScript"),
        ),
        (
            "an ARN of another service",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:instance/i-0123456789abcdef0"),
        ),
        (
            "the ARN of the baseline Systems Manager predefines",
            predefined.clone(),
        ),
    ];

    for (case, baseline_id) in cases {
        let before = get_ok(&router, case, REGION, &held).await;
        let (status, response) = update(
            &router,
            REGION,
            &baseline_id,
            &json!({ "Name": "Never.Written" }),
        )
        .await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {case}: expected 400 DoesNotExistException for {baseline_id:?}, got {status} \
             {response}"
        );
        assert_eq!(
            error_message(&response),
            NOT_FOUND_MESSAGE,
            "case {case}: expected the sentence Patch Manager words, got {response}"
        );

        // The refused update wrote nothing: the baseline this endpoint does hold reads
        // exactly as it did before the call.
        let after = get_ok(&router, case, REGION, &held).await;
        assert_eq!(
            after, before,
            "case {case}: a refused update leaves the held baseline as it was ({before}), got \
             {after}"
        );
    }
}

/// One row of the invalid-request table: an update refused on the request itself rather
/// than on the baseline it names.
struct RefusedUpdateCase<'a> {
    case: &'a str,
    created: Value,
    update: Value,
    message: &'a str,
}

/// An update whose members cannot hold together is refused as invalid and writes nothing: a
/// replacement that names no baseline name, and any write that would leave the baseline
/// approving no patch at all.
#[tokio::test]
async fn an_update_whose_members_cannot_hold_together_is_refused() {
    let cases = [
        RefusedUpdateCase {
            case: "a replacement naming no baseline name",
            created: json!({
                "Name": "Replace.Needs.Name",
                "ApprovedPatches": patch_ids(13_000_000, 1),
            }),
            update: json!({ "ApprovedPatches": patch_ids(13_100_000, 1), "Replace": true }),
            message: REPLACE_WITHOUT_NAME_MESSAGE,
        },
        RefusedUpdateCase {
            case: "clearing the only approved-patch list the baseline had",
            created: json!({
                "Name": "Approvals.By.Name",
                "ApprovedPatches": patch_ids(14_000_000, 2),
            }),
            update: json!({ "ApprovedPatches": [] }),
            message: APPROVAL_COVERAGE_MESSAGE,
        },
        RefusedUpdateCase {
            case: "emptying the only rule group the baseline had",
            created: json!({
                "Name": "Approvals.By.Rule",
                "ApprovalRules": rule_group(2),
            }),
            update: json!({ "ApprovalRules": { "PatchRules": [] } }),
            message: APPROVAL_COVERAGE_MESSAGE,
        },
        RefusedUpdateCase {
            case: "a replacement that approves nothing at all",
            created: json!({
                "Name": "Replace.Approves.Nothing",
                "ApprovedPatches": patch_ids(15_000_000, 1),
                "ApprovalRules": rule_group(1),
            }),
            update: json!({ "Name": "Replace.Approves.Nothing", "Replace": true }),
            message: APPROVAL_COVERAGE_MESSAGE,
        },
    ];

    for RefusedUpdateCase {
        case,
        created,
        update: requested,
        message,
    } in &cases
    {
        let router = router();
        let baseline_id = create_ok(&router, case, REGION, created).await;
        let before = get_ok(&router, case, REGION, &baseline_id).await;

        let (status, response) = update(&router, REGION, &baseline_id, requested).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case}: expected 400 ValidationException for {requested}, got {status} \
             {response}"
        );
        assert_eq!(
            error_message(&response),
            *message,
            "case {case}: expected {message:?}, got {response}"
        );

        let after = get_ok(&router, case, REGION, &baseline_id).await;
        assert_eq!(
            after, before,
            "case {case}: a refused update leaves the baseline as it was ({before}), got {after}"
        );
    }
}

// ---------------------------------------------------------------------------------------
// The EFFECTIVE patches of one baseline: the other read of a single patch baseline, which
// reports the patches Patch Manager publishes for the baseline's operating system together
// with the approval state the baseline gives each of them.
// ---------------------------------------------------------------------------------------

/// The `PatchDeploymentStatus` values the model's enum declares
/// (`smithy_query(query="shape", shape="PatchDeploymentStatus")`).
const DEPLOYMENT_STATUSES: &[&str] = &[
    "APPROVED",
    "PENDING_APPROVAL",
    "EXPLICIT_APPROVED",
    "EXPLICIT_REJECTED",
];

/// The published ceiling of `ApproveAfterDays` (`smithy_query(query="shape",
/// shape="PatchRule")`: `range 0..360`), whose floor is `0`.
const MAX_APPROVE_AFTER_DAYS: i64 = 360;

/// The published ceiling of an effective-patch page (`smithy_query(query="operation",
/// operation="DescribeEffectivePatchesForPatchBaseline")`: `MaxResults`, `range 1..100`),
/// whose floor is `1`.
const MAX_EFFECTIVE_PAGE: usize = 100;

/// Seconds in one day, which is the unit `ApproveAfterDays` counts in: "a value of 7 means
/// that patches are approved seven days after they are released"
/// (`smithy_query(query="doc", shape="PatchRule", member="ApproveAfterDays")`).
const DAY_SECONDS: i64 = 24 * 60 * 60;

/// How far an approval date must be from the clock reading before a case insists on which
/// side of it the deployment status falls.
///
/// Whether a selected patch "is currently approved" or is "due to be approved by a rule"
/// (`EffectivePatch`) turns on whether its approval date has passed, so a date within this
/// band of the reading is left to either answer rather than raced against the clock. Every
/// row below states a window a whole month or more away from it.
const CLOCK_BAND_SECONDS: i64 = DAY_SECONDS;

/// One approval rule: the filters that select the patches it is about, how it approves
/// them, and the compliance level it assigns them.
fn selecting_rule(
    filters: &[(&str, &[&str])],
    after_days: Option<i64>,
    until_date: Option<&str>,
    compliance_level: Option<&str>,
) -> Value {
    let mut rule = Map::new();
    rule.insert(
        "PatchFilterGroup".to_string(),
        json!({
            "PatchFilters": filters
                .iter()
                .map(|(key, values)| json!({ "Key": key, "Values": values }))
                .collect::<Vec<Value>>(),
        }),
    );
    if let Some(days) = after_days {
        rule.insert("ApproveAfterDays".to_string(), json!(days));
    }
    if let Some(date) = until_date {
        rule.insert("ApproveUntilDate".to_string(), json!(date));
    }
    if let Some(level) = compliance_level {
        rule.insert("ComplianceLevel".to_string(), json!(level));
    }
    Value::Object(rule)
}

/// A rule group of the rules it is handed.
fn rules(rules: &[Value]) -> Value {
    json!({ "PatchRules": rules })
}

/// The one rule that selects every published patch: "The filter values can be exact values
/// for the patch property given as a key, or a wildcard (*), which matches all values"
/// (`smithy_query(query="shape", shape="PatchFilter")`), approving each of them on the day
/// it was released.
fn every_patch_rule() -> Value {
    selecting_rule(&[("PRODUCT", &["*"])], Some(0), None, None)
}

/// `DescribeEffectivePatchesForPatchBaseline` at `region`'s endpoint for `baseline_id`,
/// with `extra` request members beside it.
async fn effective(
    router: &MultiServiceRouter,
    region: &str,
    baseline_id: &str,
    extra: &[(&str, Value)],
) -> (u16, Value) {
    let mut body = Map::new();
    body.insert("BaselineId".to_string(), json!(baseline_id));
    for (key, value) in extra {
        body.insert((*key).to_string(), value.clone());
    }
    call_in_region(
        router,
        region,
        "DescribeEffectivePatchesForPatchBaseline",
        &Value::Object(body),
    )
    .await
}

/// The same read, asserting it succeeded.
async fn effective_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    baseline_id: &str,
    extra: &[(&str, Value)],
) -> Value {
    let (status, response) = effective(router, region, baseline_id, extra).await;
    assert_eq!(
        status, 200,
        "case {case}: an effective-patch read of {baseline_id:?} must succeed, got {status} \
         {response}"
    );
    response
}

/// The `EffectivePatches` of one page.
fn reported_patches(case: &str, response: &Value) -> Vec<Value> {
    response["EffectivePatches"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case}: a read reports an EffectivePatches array, got {response}")
        })
        .clone()
}

/// Every effective patch of `baseline_id`, walking the pages the reads hand out.
async fn effective_all(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    baseline_id: &str,
) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    // The sequence is finite: each page is a suffix of one result and hands out the id of
    // the patch the next page starts at, or no token at all. The bound is a backstop
    // against a page that never advances, not a wait.
    for _ in 0..64 {
        let extra: Vec<(&str, Value)> = match token.as_deref() {
            Some(next) => vec![("NextToken", json!(next))],
            None => Vec::new(),
        };
        let response = effective_ok(router, case, region, baseline_id, &extra).await;
        collected.extend(reported_patches(case, &response));
        match response["NextToken"].as_str() {
            Some(next) => token = Some(next.to_string()),
            None => return collected,
        }
    }
    panic!("case {case}: the page sequence of {baseline_id:?} did not end");
}

/// The value one reported member of an effective patch's `Patch` carries.
fn patch_member<'a>(case: &str, entry: &'a Value, member: &str) -> &'a Value {
    let value = &entry["Patch"][member];
    assert!(
        !value.is_null(),
        "case {case}: a published Windows patch reports {member}, got {entry}"
    );
    value
}

/// The Microsoft Knowledge Base id of one reported patch.
fn kb_number(case: &str, entry: &Value) -> String {
    patch_member(case, entry, "KbNumber")
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: KbNumber is a string, got {entry}"))
        .to_string()
}

/// The deployment status of one reported patch.
fn deployment_status(case: &str, entry: &Value) -> String {
    let status = entry["PatchStatus"]["DeploymentStatus"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: an effective patch reports a status, got {entry}"))
        .to_string();
    assert!(
        DEPLOYMENT_STATUSES.contains(&status.as_str()),
        "case {case}: {status:?} is not one of the modelled deployment statuses, got {entry}"
    );
    status
}

/// The epoch second one reported timestamp names.
fn epoch_second(case: &str, value: &Value) -> i64 {
    value
        .as_f64()
        .map(|seconds| seconds as i64)
        .unwrap_or_else(|| panic!("case {case}: {value} is not a reported instant"))
}

/// The current instant, as the epoch second the reported instants are compared against.
fn clock_reading() -> i64 {
    let since = ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .expect("the clock reads after the epoch");
    since.as_secs() as i64
}

/// A read reports the patches Patch Manager publishes for the baseline's operating system,
/// each with the metadata that catalogue holds for it, and reports the same catalogue to a
/// later caller.
#[tokio::test]
async fn an_effective_patch_read_reports_the_published_catalogue() {
    let router = router();
    let baseline = create_ok(
        &router,
        "a baseline selecting every published patch",
        REGION,
        &json!({
            "Name": "Effective.Catalogue",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": rules(&[every_patch_rule()]),
        }),
    )
    .await;

    let case = "every published patch";
    let catalogue = effective_all(&router, case, REGION, &baseline).await;
    assert!(
        catalogue.len() > MAX_EFFECTIVE_PAGE,
        "case {case}: the published catalogue spans more than one page, got {} patches",
        catalogue.len()
    );

    let mut ids: Vec<String> = Vec::new();
    let mut kbs: Vec<String> = Vec::new();
    for entry in &catalogue {
        // The members the published metadata of a Windows patch carries values for
        // (`smithy_query(query="shape", shape="Patch")`), each reported for every patch of
        // the catalogue.
        for member in [
            "Id",
            "Title",
            "ReleaseDate",
            "Vendor",
            "ProductFamily",
            "Product",
            "Classification",
            "MsrcSeverity",
            "KbNumber",
            "Language",
        ] {
            patch_member(case, entry, member);
        }
        assert_eq!(
            patch_member(case, entry, "Vendor").as_str(),
            Some("Microsoft"),
            "case {case}: a Windows patch names the vendor publishing it, got {entry}"
        );
        // "The number of days after the release date of each patch matched by the rule
        // that the patch is marked as approved": a rule approving on release day leaves
        // every released patch approved, on its own release date.
        assert_eq!(
            deployment_status(case, entry),
            "APPROVED",
            "case {case}: a patch approved on its release day is approved, got {entry}"
        );
        assert_eq!(
            entry["PatchStatus"]["ApprovalDate"],
            entry["Patch"]["ReleaseDate"],
            "case {case}: that patch was approved on the day it was released, got {entry}"
        );
        // The rule named no compliance level, so the patches it approves carry the
        // documented default.
        assert_eq!(
            entry["PatchStatus"]["ComplianceLevel"].as_str(),
            Some(DEFAULT_COMPLIANCE_LEVEL),
            "case {case}: a rule naming no compliance level assigns the default, got {entry}"
        );
        assert!(
            epoch_second(case, patch_member(case, entry, "ReleaseDate")) <= clock_reading(),
            "case {case}: a published patch was released by now, got {entry}"
        );
        ids.push(
            patch_member(case, entry, "Id")
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
        kbs.push(kb_number(case, entry));
    }

    // One patch is reported once: the id and the Knowledge Base id each name one patch of
    // the catalogue, which is what makes the id a page token can resume at.
    let mut unique_ids = ids.clone();
    unique_ids.sort();
    unique_ids.dedup();
    assert_eq!(
        unique_ids.len(),
        ids.len(),
        "case {case}: every patch carries its own id, got {} of {}",
        unique_ids.len(),
        ids.len()
    );
    let mut unique_kbs = kbs.clone();
    unique_kbs.sort();
    unique_kbs.dedup();
    assert_eq!(
        unique_kbs.len(),
        kbs.len(),
        "case {case}: every patch carries its own Knowledge Base id, got {} of {}",
        unique_kbs.len(),
        kbs.len()
    );

    // A READ: the second read reports the catalogue the first one did, and the baseline
    // itself still reads.
    let again = effective_all(&router, "a later caller", REGION, &baseline).await;
    assert_eq!(
        again, catalogue,
        "case {case}: a later caller is served the same catalogue"
    );
    let reported = get_ok(&router, case, REGION, &baseline).await;
    assert_eq!(
        reported["BaselineId"].as_str(),
        Some(baseline.as_str()),
        "case {case}: the baseline still reads, got {reported}"
    );
}

/// The approval state a rule gives the patches it selects follows the window the rule
/// states: the date is the patch's own release date moved on by the rule's days, and the
/// patch is approved once that date has passed and due to be approved until then.
#[tokio::test]
async fn a_rule_approves_the_patches_it_selects_on_the_window_it_states() {
    let router = router();

    // Rows differing along the window a rule states — both published bounds of
    // `ApproveAfterDays`, and values inside it — and along the compliance level it assigns.
    let cases: &[(&str, i64, Option<&str>)] = &[
        ("at the published floor of the window", 0, Some("CRITICAL")),
        ("a week after release", 7, Some("HIGH")),
        ("a month after release", 30, None),
        ("a quarter after release", 90, Some("INFORMATIONAL")),
        (
            "at the published ceiling of the window",
            MAX_APPROVE_AFTER_DAYS,
            Some("LOW"),
        ),
    ];
    for (case, days, level) in cases {
        let baseline = create_ok(
            &router,
            case,
            REGION,
            &json!({
                "Name": format!("Effective.Window{days}"),
                "OperatingSystem": "WINDOWS",
                "ApprovalRules": rules(&[selecting_rule(
                    &[("PRODUCT", &["*"])],
                    Some(*days),
                    None,
                    *level,
                )]),
            }),
        )
        .await;
        let reported = effective_all(&router, case, REGION, &baseline).await;
        assert!(
            !reported.is_empty(),
            "case {case}: a rule selecting every patch makes the catalogue effective"
        );
        let now = clock_reading();
        let mut pending = 0_usize;
        let mut approved = 0_usize;
        for entry in &reported {
            let release = epoch_second(case, patch_member(case, entry, "ReleaseDate"));
            let approval = epoch_second(case, &entry["PatchStatus"]["ApprovalDate"]);
            assert_eq!(
                approval - release,
                days * DAY_SECONDS,
                "case {case}: the patch is approved {days} days after its release, got {entry}"
            );
            assert_eq!(
                entry["PatchStatus"]["ComplianceLevel"].as_str(),
                Some(level.unwrap_or(DEFAULT_COMPLIANCE_LEVEL)),
                "case {case}: the rule's own compliance level is reported, got {entry}"
            );
            let status = deployment_status(case, entry);
            if approval + CLOCK_BAND_SECONDS < now {
                assert_eq!(
                    status, "APPROVED",
                    "case {case}: an approval date already passed leaves the patch approved, \
                     got {entry}"
                );
                approved += 1;
            } else if approval > now + CLOCK_BAND_SECONDS {
                assert_eq!(
                    status, "PENDING_APPROVAL",
                    "case {case}: an approval date still to come leaves the patch due to be \
                     approved, got {entry}"
                );
                pending += 1;
            }
        }
        // The window the row states is what decides which answer the catalogue falls on:
        // a window of nothing approves every released patch at once, and the widest window
        // a rule may state leaves the most recent release waiting.
        if *days == 0 {
            assert_eq!(
                pending, 0,
                "case {case}: no patch waits when a rule approves on release day"
            );
            assert!(
                approved > 0,
                "case {case}: the released patches are approved"
            );
        }
        if *days == MAX_APPROVE_AFTER_DAYS {
            assert!(
                pending > 0,
                "case {case}: the widest window a rule may state leaves patches waiting"
            );
        }
    }
}

/// A rule stating a cutoff date approves the patches of its criteria released by that
/// date — on their own release date — and leaves the ones released after it waiting with no
/// approval date at all.
#[tokio::test]
async fn a_rule_stating_a_cutoff_approves_the_patches_released_by_it() {
    let router = router();
    let probe = create_ok(
        &router,
        "the catalogue the cutoffs are taken from",
        REGION,
        &json!({
            "Name": "Effective.Cutoff.Probe",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": rules(&[every_patch_rule()]),
        }),
    )
    .await;
    let catalogue = effective_all(&router, "the published catalogue", REGION, &probe).await;
    let mut releases: Vec<i64> = catalogue
        .iter()
        .map(|entry| epoch_second("the published catalogue", &entry["Patch"]["ReleaseDate"]))
        .collect();
    releases.sort_unstable();
    releases.dedup();
    assert!(
        releases.len() >= 3,
        "the catalogue holds several releases, got {}",
        releases.len()
    );

    // Rows differing along where the cutoff falls in the catalogue's own release
    // sequence: before every release, inside it, and after all of them.
    let cases: &[(&str, usize)] = &[
        ("a cutoff before every release", 0),
        ("a cutoff inside the release sequence", releases.len() / 2),
        ("a cutoff after every release", releases.len() - 1),
    ];
    for (case, index) in cases {
        let cutoff_second = releases[*index];
        let cutoff = ::chrono::DateTime::from_timestamp(cutoff_second, 0)
            .unwrap_or_else(|| panic!("case {case}: {cutoff_second} is a calendar instant"))
            .format("%Y-%m-%d")
            .to_string();
        let baseline = create_ok(
            &router,
            case,
            REGION,
            &json!({
                "Name": format!("Effective.Cutoff{index}"),
                "OperatingSystem": "WINDOWS",
                "ApprovalRules": rules(&[selecting_rule(
                    &[("PRODUCT", &["*"])],
                    None,
                    Some(&cutoff),
                    Some("MEDIUM"),
                )]),
            }),
        )
        .await;
        let reported = effective_all(&router, case, REGION, &baseline).await;
        assert_eq!(
            reported.len(),
            catalogue.len(),
            "case {case}: the rule's criteria still select the whole catalogue"
        );
        // "Patches released between <date>T00:00:00Z and <date>T23:59:59Z will be
        // included in the approval", so a patch released on the cutoff day itself is
        // approved and the band the day covers is what decides the rest.
        let day_end = cutoff_second - cutoff_second.rem_euclid(DAY_SECONDS) + DAY_SECONDS - 1;
        for entry in &reported {
            let release = epoch_second(case, patch_member(case, entry, "ReleaseDate"));
            let status = deployment_status(case, entry);
            if release <= day_end {
                assert_eq!(
                    status, "APPROVED",
                    "case {case}: a patch released by the cutoff {cutoff} is approved, got \
                     {entry}"
                );
                assert_eq!(
                    entry["PatchStatus"]["ApprovalDate"],
                    entry["Patch"]["ReleaseDate"],
                    "case {case}: it was approved on its release date, got {entry}"
                );
            } else {
                assert_eq!(
                    status, "PENDING_APPROVAL",
                    "case {case}: a patch released after the cutoff {cutoff} is not approved, \
                     got {entry}"
                );
                assert!(
                    entry["PatchStatus"]["ApprovalDate"].is_null(),
                    "case {case}: nothing approves it, so no approval date is reported, got \
                     {entry}"
                );
            }
            assert_eq!(
                entry["PatchStatus"]["ComplianceLevel"].as_str(),
                Some("MEDIUM"),
                "case {case}: the rule's own compliance level is reported, got {entry}"
            );
        }
    }
}

/// The patches a baseline names one by one carry the state the baseline NAMED, whichever
/// way its rules and filters fall: an explicitly approved patch carries the level the
/// baseline assigns the patches it approves, an explicitly rejected one is reported
/// rejected even where a rule would approve it, and the patch no list names follows the
/// rules.
#[tokio::test]
async fn the_patches_a_baseline_names_carry_the_state_it_named() {
    let router = router();
    let probe = create_ok(
        &router,
        "the catalogue the named patches are taken from",
        REGION,
        &json!({
            "Name": "Effective.Named.Probe",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": rules(&[every_patch_rule()]),
        }),
    )
    .await;
    let catalogue = effective_all(&router, "the published catalogue", REGION, &probe).await;
    let named: Vec<String> = [0, 1, catalogue.len() / 2, catalogue.len() - 1]
        .into_iter()
        .map(|index| kb_number("the published catalogue", &catalogue[index]))
        .collect();
    let classification = patch_member("the published catalogue", &catalogue[0], "Classification")
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Rows differing along the compliance level the baseline assigns the patches it
    // approves by naming them, including the row that names none at all.
    let cases: &[(&str, Option<&str>)] = &[
        ("a baseline assigning the critical level", Some("CRITICAL")),
        ("a baseline assigning the medium level", Some("MEDIUM")),
        ("a baseline assigning none", None),
    ];
    for (case, level) in cases {
        let approved = &named[0..2];
        let rejected = &named[2..4];
        let mut body = json!({
            "Name": format!("Effective.Named{}", level.unwrap_or("Default")),
            "OperatingSystem": "WINDOWS",
            // The rule selects the patches of ONE classification, so the two named lists
            // reach patches the rule covers and patches it does not.
            "ApprovalRules": rules(&[selecting_rule(
                &[("CLASSIFICATION", &[classification.as_str()])],
                Some(0),
                None,
                Some("HIGH"),
            )]),
            "ApprovedPatches": approved,
            "RejectedPatches": rejected,
        });
        if let Some(level) = level {
            body["ApprovedPatchesComplianceLevel"] = json!(level);
        }
        let baseline = create_ok(&router, case, REGION, &body).await;
        let reported = effective_all(&router, case, REGION, &baseline).await;

        for kb in approved {
            let entry = reported
                .iter()
                .find(|entry| &kb_number(case, entry) == kb)
                .unwrap_or_else(|| panic!("case {case}: {kb} is effective in the baseline"));
            assert_eq!(
                deployment_status(case, entry),
                "EXPLICIT_APPROVED",
                "case {case}: a patch the baseline approves by name is explicitly approved, \
                 got {entry}"
            );
            assert_eq!(
                entry["PatchStatus"]["ComplianceLevel"].as_str(),
                Some(level.unwrap_or(DEFAULT_COMPLIANCE_LEVEL)),
                "case {case}: it carries the level the baseline assigns approved patches, got \
                 {entry}"
            );
            assert!(
                !entry["PatchStatus"]["ApprovalDate"].is_null(),
                "case {case}: the date it was approved on is reported, got {entry}"
            );
        }
        for kb in rejected {
            let entry = reported
                .iter()
                .find(|entry| &kb_number(case, entry) == kb)
                .unwrap_or_else(|| panic!("case {case}: {kb} is effective in the baseline"));
            assert_eq!(
                deployment_status(case, entry),
                "EXPLICIT_REJECTED",
                "case {case}: a patch the baseline rejects by name is explicitly rejected, \
                 got {entry}"
            );
            assert!(
                entry["PatchStatus"]["ApprovalDate"].is_null(),
                "case {case}: a rejected patch was never approved, got {entry}"
            );
            assert!(
                entry["PatchStatus"]["ComplianceLevel"].is_null(),
                "case {case}: a rejected patch carries no approval compliance level, got \
                 {entry}"
            );
        }

        // Every other effective patch is one the rule selected, so it carries the rule's
        // own level and the rule's own approval, and no patch outside the rule's criteria
        // and outside both lists is effective at all.
        for entry in &reported {
            let kb = kb_number(case, entry);
            if named.contains(&kb) {
                continue;
            }
            assert_eq!(
                patch_member(case, entry, "Classification")
                    .as_str()
                    .unwrap_or_default(),
                classification,
                "case {case}: only the rule's own classification is effective, got {entry}"
            );
            assert_eq!(
                deployment_status(case, entry),
                "APPROVED",
                "case {case}: the rule approves its patches on release day, got {entry}"
            );
            assert_eq!(
                entry["PatchStatus"]["ComplianceLevel"].as_str(),
                Some("HIGH"),
                "case {case}: the rule's own compliance level is reported, got {entry}"
            );
        }
    }
}

/// Which patches a baseline makes effective follows the criteria it states: a rule's
/// filters select on the patch property their key names, and the baseline's global filters
/// narrow what its rules are applied to at all.
#[tokio::test]
async fn the_criteria_a_baseline_states_select_the_patches_it_makes_effective() {
    let router = router();
    let probe = create_ok(
        &router,
        "the catalogue the criteria are taken from",
        REGION,
        &json!({
            "Name": "Effective.Criteria.Probe",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": rules(&[every_patch_rule()]),
        }),
    )
    .await;
    let catalogue = effective_all(&router, "the published catalogue", REGION, &probe).await;

    /// The patch property a filter key selects on, for the keys a Windows patch filter
    /// accepts: "the available filter keys for `WINDOWS` are `PATCH_SET`, `PRODUCT`,
    /// `PRODUCT_FAMILY`, `CLASSIFICATION`, and `MSRC_SEVERITY`"
    /// (`smithy_query(query="shape", shape="PatchFilter")`), beside the `PATCH_ID` whose
    /// sample values are Knowledge Base ids (`DescribeAvailablePatches$Filters`).
    const SELECTORS: &[(&str, &str)] = &[
        ("PRODUCT", "Product"),
        ("PRODUCT_FAMILY", "ProductFamily"),
        ("CLASSIFICATION", "Classification"),
        ("MSRC_SEVERITY", "MsrcSeverity"),
        ("PATCH_ID", "KbNumber"),
    ];

    for (key, member) in SELECTORS {
        // The value is taken from the catalogue itself rather than written here, so the
        // row holds for whichever products and classifications the catalogue publishes.
        let value = patch_member("the published catalogue", &catalogue[0], member)
            .as_str()
            .unwrap_or_default()
            .to_string();
        let case = &format!("a rule selecting on {key}");
        let baseline = create_ok(
            &router,
            case,
            REGION,
            &json!({
                "Name": format!("Effective.Rule.{key}"),
                "OperatingSystem": "WINDOWS",
                "ApprovalRules": rules(&[selecting_rule(
                    &[(key, &[value.as_str()])],
                    Some(0),
                    None,
                    None,
                )]),
            }),
        )
        .await;
        let reported = effective_all(&router, case, REGION, &baseline).await;
        assert!(
            !reported.is_empty(),
            "case {case}: the value {value:?} the catalogue publishes selects patches"
        );
        let expected = catalogue
            .iter()
            .filter(|entry| {
                patch_member("the published catalogue", entry, member).as_str() == Some(&value)
            })
            .count();
        assert_eq!(
            reported.len(),
            expected,
            "case {case}: exactly the patches whose {member} is {value:?} are effective"
        );
        for entry in &reported {
            assert_eq!(
                patch_member(case, entry, member).as_str(),
                Some(value.as_str()),
                "case {case}: every effective patch meets the rule's criteria, got {entry}"
            );
        }
    }

    // A global filter narrows what the rules are applied to: the rule still selects every
    // patch, so what is effective is what the global filter admits.
    let product = patch_member("the published catalogue", &catalogue[0], "Product")
        .as_str()
        .unwrap_or_default()
        .to_string();
    let case = "a global filter narrowing the baseline";
    let baseline = create_ok(
        &router,
        case,
        REGION,
        &json!({
            "Name": "Effective.Global.Filter",
            "OperatingSystem": "WINDOWS",
            "GlobalFilters": {
                "PatchFilters": [{ "Key": "PRODUCT", "Values": [product.as_str()] }],
            },
            "ApprovalRules": rules(&[every_patch_rule()]),
        }),
    )
    .await;
    let reported = effective_all(&router, case, REGION, &baseline).await;
    assert!(
        !reported.is_empty() && reported.len() < catalogue.len(),
        "case {case}: the filter admits some of the catalogue and not all of it, got {} of {}",
        reported.len(),
        catalogue.len()
    );
    for entry in &reported {
        assert_eq!(
            patch_member(case, entry, "Product").as_str(),
            Some(product.as_str()),
            "case {case}: only the products the global filter admits are effective, got {entry}"
        );
    }
}

/// A page of an effective-patch read is a prefix of the one result, and the token it hands
/// out resumes it at the patch the page stopped at — whichever page size the request asks
/// for.
#[tokio::test]
async fn an_effective_patch_read_pages_the_one_result() {
    let router = router();
    let baseline = create_ok(
        &router,
        "a baseline selecting every published patch",
        REGION,
        &json!({
            "Name": "Effective.Pages",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": rules(&[every_patch_rule()]),
        }),
    )
    .await;
    let whole = effective_all(&router, "the unpaged sequence", REGION, &baseline).await;

    // Rows at both published bounds of `MaxResults` and inside it.
    for size in [1_usize, 7, MAX_EFFECTIVE_PAGE] {
        let case = &format!("a page of {size}");
        let mut collected: Vec<Value> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut extra: Vec<(&str, Value)> = vec![("MaxResults", json!(size))];
            if let Some(next) = token.as_deref() {
                extra.push(("NextToken", json!(next)));
            }
            let response = effective_ok(&router, case, REGION, &baseline, &extra).await;
            let page = reported_patches(case, &response);
            assert!(
                page.len() <= size,
                "case {case}: a page holds at most what the request asked for, got {}",
                page.len()
            );
            collected.extend(page);
            match response["NextToken"].as_str() {
                Some(next) => {
                    assert_eq!(
                        collected.len() % size,
                        0,
                        "case {case}: a full page precedes a token, got {} patches",
                        collected.len()
                    );
                    token = Some(next.to_string());
                }
                None => break,
            }
            assert!(
                collected.len() <= whole.len(),
                "case {case}: the pages do not run past the one result, got {} of {}",
                collected.len(),
                whole.len()
            );
        }
        assert_eq!(
            collected, whole,
            "case {case}: the pages walk the same sequence the unpaged read reports"
        );
    }

    // "The token for the next set of items to return. (You received this token from a
    // previous call.)": a token naming no patch of this result is not a page of it.
    for (case, token) in [
        ("a token naming nothing", "not-a-patch-of-this-result"),
        ("a token naming a patch of another result", "KB5000000"),
    ] {
        let (status, response) =
            effective(&router, REGION, &baseline, &[("NextToken", json!(token))]).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case}: expected 400 ValidationException for {token:?}, got {status} \
             {response}"
        );
    }
}

/// The read is refused where it has nothing to report: a baseline of an operating system it
/// does not apply to, an id that names no baseline whose approval configuration is held,
/// and a value that could never name one.
#[tokio::test]
async fn an_effective_patch_read_refuses_what_it_cannot_report() {
    let router = router();

    // "Applies to patch baselines for Windows only": a row per other operating system the
    // model declares, each a baseline this read does not apply to.
    for operating_system in OPERATING_SYSTEMS
        .iter()
        .filter(|name| **name != DEFAULT_OPERATING_SYSTEM)
    {
        let case = &format!("a {operating_system} baseline");
        let baseline = create_ok(
            &router,
            case,
            REGION,
            &json!({
                "Name": "Effective.Other.Os",
                "OperatingSystem": operating_system,
                "ApprovedPatches": patch_ids(2_124_261, 1),
            }),
        )
        .await;
        let (status, response) = effective(&router, REGION, &baseline, &[]).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "UnsupportedOperatingSystem"),
            "case {case}: expected 400 UnsupportedOperatingSystem, got {status} {response}"
        );
        assert!(
            error_message(&response).contains(operating_system),
            "case {case}: the refusal names the operating system it is about, got {response}"
        );
    }

    let held = create_ok(
        &router,
        "the held Windows baseline",
        REGION,
        &json!({
            "Name": "Effective.Held",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": rules(&[every_patch_rule()]),
        }),
    )
    .await;
    let elsewhere = create_ok(
        &router,
        "a baseline of another Region",
        "eu-central-1",
        &json!({
            "Name": "Effective.Elsewhere",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": rules(&[every_patch_rule()]),
        }),
    )
    .await;
    let predefined =
        predefined_baseline_id(&router, "the predefined default", REGION, "WINDOWS").await;

    let absent: Vec<(&str, String)> = vec![
        (
            "an id at the published floor of the member",
            "pb-00000000000000000".to_string(),
        ),
        ("a baseline of another Region", elsewhere.clone()),
        (
            "the ARN of a baseline of another Region, at this endpoint",
            baseline_arn(REGION, &elsewhere),
        ),
    ];
    for (case, baseline_id) in absent {
        let (status, response) = effective(&router, REGION, &baseline_id, &[]).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "DoesNotExistException"),
            "case {case}: expected 400 DoesNotExistException for {baseline_id:?}, got {status} \
             {response}"
        );
        assert_eq!(
            error_message(&response),
            NOT_FOUND_MESSAGE,
            "case {case}: expected the sentence Patch Manager words, got {response}"
        );
    }

    // A baseline Systems Manager predefines is reachable through this endpoint and carries
    // the approval configuration published with it, so an effective-patch read of one is
    // SERVED from that configuration rather than refused as absent.
    let case = "the predefined default baseline";
    let (status, served) = effective(&router, REGION, &predefined, &[]).await;
    assert_eq!(
        status, 200,
        "case {case}: expected the read to be served for {predefined:?}, got {status} {served}"
    );
    assert!(
        served["EffectivePatches"]
            .as_array()
            .is_some_and(|patches| !patches.is_empty()),
        "case {case}: expected the patches the published configuration makes effective, got \
         {served}"
    );

    let invalid: Vec<(&str, String)> = vec![
        (
            "an ARN of another account",
            format!("arn:aws:ssm:{REGION}:210987654321:patchbaseline/{held}"),
        ),
        (
            "an ARN of another Region",
            format!("arn:aws:ssm:eu-central-1:{ACCOUNT}:patchbaseline/{held}"),
        ),
        (
            "an ARN of another service",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:patchbaseline/{held}"),
        ),
        (
            "an ARN of another resource type",
            format!("arn:aws:ssm:{REGION}:{ACCOUNT}:document/{held}"),
        ),
    ];
    for (case, baseline_id) in invalid {
        let (status, response) = effective(&router, REGION, &baseline_id, &[]).await;
        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "InvalidResourceId"),
            "case {case}: expected 400 InvalidResourceId for {baseline_id:?}, got {status} \
             {response}"
        );
    }

    // Every refusal changed nothing: the held baseline still reports its effective patches.
    let reported = effective_all(&router, "after the refusals", REGION, &held).await;
    assert!(
        !reported.is_empty(),
        "the held baseline still reports its effective patches"
    );
}
