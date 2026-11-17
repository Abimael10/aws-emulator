// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for AppStream's app block builder/app block associations over the
//! wire.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness, so what is exercised is
//! the dispatch → deserialize → handle → serialize walk.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * an app block builder may be associated only with an app block whose application
//!   package a builder produces: an app block of any other packaging is refused
//!   `InvalidParameterValueException` (400) carrying AWS's sentence and records NO
//!   association, while a builder-packaged one associates with HTTP 200 and is then
//!   reported from either endpoint;
//! * an app block builder holds AT MOST ONE app block: while one is associated, a request
//!   naming a DIFFERENT app block is refused `OperationNotPermittedException` (400)
//!   carrying AWS's sentence and records nothing, the same pair again is answered as the
//!   association it already is, and disassociating frees the slot so the refused request
//!   then succeeds — while one app block may fill the slot of any number of builders;
//! * a disassociation removes exactly the edge it names, however often it is repeated, and
//!   either endpoint this account does not hold is refused `ResourceNotFoundException`
//!   with nothing removed;
//! * either endpoint this account does not hold is refused `ResourceNotFoundException`
//!   (400) with AWS's sentence for that resource kind, and records nothing;
//! * `MaxResults` sizes a page of the read so that following `NextToken` reports every
//!   association exactly once;
//! * an association holds the app block, so the app block cannot be deleted while a
//!   builder is attached to it;
//! * the packaging gate turns on the app block's STORED packaging and on nothing else —
//!   not on the shape of either name, not on the app block's optional metadata — and the
//!   sentence it refuses with is fixed text that echoes no name and no ARN;
//! * a refusal is repeatable and leaves the associations the builder already holds
//!   exactly as they were.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::refusal_messages::{not_found_sentence, refusal};
use crate::stack_fleet_lifecycle::{assert_error, Emulator};

/// The account every emulator ARN names.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The Region every case below signs for.
const REGION: &str = "us-east-1";

/// The only `AppBlockBuilderPlatformType` the model declares, and the only value
/// `CreateAppBlockBuilder.Platform` documents.
const PLATFORM: &str = "WINDOWS_SERVER_2019";

/// The word AWS's refusal sentences use for an app block builder.
const BUILDER_KIND: &str = "appblockbuilder";

/// The `PackagingType` values the model admits (`PACKAGING_TYPE_VALUES` in
/// `src/ops/types/shared_types.rs`), and the one whose application package an app block
/// builder produces.
const PACKAGING_TYPES: &[&str] = &["APPSTREAM2", "CUSTOM"];
const BUILDER_PACKAGED: &str = "APPSTREAM2";

/// The sentence the service states for an app block that supports no association, read
/// from the crate so the test tracks the wording rather than a copy of it.
fn not_supported_sentence() -> String {
    service_com_amazonaws_appstream_photonadminproxyservice::state::app_block_association_not_supported_message()
}

/// AWS's `ResourceNotFoundException` sentence for an app block addressed by an ARN this
/// account holds nothing under: the capitalised resource word AWS spells that sentence
/// with, and the ARN the request gave.
fn app_block_not_found_sentence(arn: &str) -> String {
    not_found_sentence("App block", arn)
}

/// The ARN an app block of this name carries in [`REGION`].
fn app_block_arn(name: &str) -> String {
    format!("arn:aws:appstream:{REGION}:{ACCOUNT}:app-block/{name}")
}

/// Create an app block builder of `name`.
async fn create_builder(emulator: &Emulator, case: &str, name: &str) {
    emulator
        .ok(
            case,
            "CreateAppBlockBuilder",
            json!({
                "Name": name,
                "Platform": PLATFORM,
                "InstanceType": "stream.standard.small",
                "VpcConfig": { "SubnetIds": ["subnet-a1", "subnet-b2"] },
            }),
        )
        .await;
}

/// Create an app block of `name` with `packaging_type` (omitted when `None`, which is the
/// packaging a create that names none gets), and answer its ARN.
async fn create_app_block(
    emulator: &Emulator,
    case: &str,
    name: &str,
    packaging_type: Option<&str>,
    s3_key: Option<&str>,
) -> String {
    let mut source = json!({ "S3Bucket": "app-packages" });
    if let Some(key) = s3_key {
        source["S3Key"] = json!(key);
    }
    let mut request = json!({ "Name": name, "SourceS3Location": source });
    if let Some(packaging_type) = packaging_type {
        request["PackagingType"] = json!(packaging_type);
    }
    // `SetupScriptDetails` "must be provided for the CUSTOM PackagingType", which is also
    // the packaging a create that names none gets.
    if packaging_type.is_none_or(|packaging_type| packaging_type != BUILDER_PACKAGED) {
        request["SetupScriptDetails"] = json!({
            "ScriptS3Location": {
                "S3Bucket": "app-packages",
                "S3Key": "scripts/setup.ps1",
            },
            "ExecutablePath": "setup.ps1",
            "TimeoutInSeconds": 60,
        });
    }
    let created = emulator.ok(case, "CreateAppBlock", request).await;
    created
        .get("AppBlock")
        .and_then(|block| block.get("Arn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the app block's ARN, got {created}"))
        .to_string()
}

/// The associations a read of `selector` reports.
async fn associated(emulator: &Emulator, case: &str, selector: Value) -> Vec<Value> {
    let payload = emulator
        .ok(
            case,
            "DescribeAppBlockBuilderAppBlockAssociations",
            selector,
        )
        .await;
    payload
        .get("AppBlockBuilderAppBlockAssociations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected AppBlockBuilderAppBlockAssociations, got {payload}")
        })
}

/// One row of the packaging table: how the app block was created, and whether a builder
/// may be associated with it.
struct Row {
    case: &'static str,
    packaging_type: Option<&'static str>,
    /// Whether the source location names an application package already — what decides
    /// the state a created app block carries, which the association rule is INDEPENDENT
    /// of.
    s3_key: Option<&'static str>,
    associable: bool,
}

/// The packaging table. The rows span the whole `PackagingType` catalog, the create that
/// names no packaging at all, and both source locations an app block can be created with —
/// so what the rule turns on (the packaging) is varied against what it must not turn on
/// (whether the app block already carries a package, and with it the state it reports).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "builder packaging with an application package",
            packaging_type: Some("APPSTREAM2"),
            s3_key: Some("disks/app.vhd"),
            associable: true,
        },
        Row {
            case: "builder packaging with no application package yet",
            packaging_type: Some("APPSTREAM2"),
            s3_key: None,
            associable: true,
        },
        Row {
            case: "an app block that brings its own disk",
            packaging_type: Some("CUSTOM"),
            s3_key: Some("disks/custom.vhd"),
            associable: false,
        },
        Row {
            case: "an app block that brings its own disk, with no key",
            packaging_type: Some("CUSTOM"),
            s3_key: None,
            associable: false,
        },
        Row {
            case: "a create that names no packaging at all",
            packaging_type: None,
            s3_key: Some("disks/default.vhd"),
            associable: false,
        },
    ]
}

#[tokio::test]
async fn only_an_app_block_a_builder_packages_may_be_associated() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::in_region(REGION);
        let builder = format!("packaging-builder-{index}");
        let block = format!("packaging-block-{index}");
        create_builder(&emulator, case, &builder).await;
        let arn = create_app_block(&emulator, case, &block, row.packaging_type, row.s3_key).await;

        let request = json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn });
        if row.associable {
            let answer = emulator
                .ok(case, "AssociateAppBlockBuilderAppBlock", request.clone())
                .await;
            assert_eq!(
                answer,
                json!({
                    "AppBlockBuilderAppBlockAssociation": {
                        "AppBlockBuilderName": builder,
                        "AppBlockArn": arn,
                    }
                }),
                "case {case:?}: the result must carry the association that was recorded, \
                 got {answer}"
            );
        } else {
            let message = refusal(
                &emulator,
                case,
                "AssociateAppBlockBuilderAppBlock",
                request.clone(),
                400,
                "InvalidParameterValueException",
            )
            .await;
            assert_eq!(
                message,
                not_supported_sentence(),
                "case {case:?}: the refusal must state that the app block supports no \
                 association, got {message:?}"
            );
        }

        // Either way, the read reports exactly the associations that were recorded — from
        // the builder's side, from the app block's, and with no selector at all.
        let expected: Vec<Value> = if row.associable {
            vec![json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn })]
        } else {
            Vec::new()
        };
        for selector in [
            json!({ "AppBlockBuilderName": builder }),
            json!({ "AppBlockArn": arn }),
            json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn }),
            json!({}),
        ] {
            let reported = associated(&emulator, case, selector.clone()).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: the read of {selector} must report the associations the \
                 store holds, got {reported:?}"
            );
        }

        // A repeat of an admitted association is answered the same way and records the
        // edge once, so associating again is not a second association.
        if row.associable {
            emulator
                .ok(case, "AssociateAppBlockBuilderAppBlock", request)
                .await;
            let reported = associated(&emulator, case, json!({})).await;
            assert_eq!(
                reported, expected,
                "case {case:?}: the pair must be associated exactly once, got {reported:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_packaging_in_the_catalog_is_judged_by_the_builder_rule() {
    // The whole `PackagingType` catalog, one app block each against one builder: the rule
    // holds for every value the model admits rather than only for the one the issue cited.
    let case = "the whole packaging catalog";
    let emulator = Emulator::in_region(REGION);
    create_builder(&emulator, case, "catalog-builder").await;

    let mut expected: Vec<Value> = Vec::new();
    for (index, packaging_type) in PACKAGING_TYPES.iter().enumerate() {
        let block = format!("catalog-block-{index}");
        let arn = create_app_block(
            &emulator,
            case,
            &block,
            Some(packaging_type),
            Some("disks/app.vhd"),
        )
        .await;
        let request = json!({ "AppBlockBuilderName": "catalog-builder", "AppBlockArn": arn });
        if *packaging_type == BUILDER_PACKAGED {
            emulator
                .ok(case, "AssociateAppBlockBuilderAppBlock", request)
                .await;
            expected.push(json!({
                "AppBlockBuilderName": "catalog-builder",
                "AppBlockArn": arn,
            }));
        } else {
            let message = refusal(
                &emulator,
                case,
                "AssociateAppBlockBuilderAppBlock",
                request,
                400,
                "InvalidParameterValueException",
            )
            .await;
            assert_eq!(
                message,
                not_supported_sentence(),
                "case {case:?} ({packaging_type}): unexpected sentence {message:?}"
            );
        }
    }

    let reported = associated(
        &emulator,
        case,
        json!({ "AppBlockBuilderName": "catalog-builder" }),
    )
    .await;
    assert_eq!(
        reported, expected,
        "case {case:?}: only the builder-packaged app block may be associated, got \
         {reported:?}"
    );
}

#[tokio::test]
async fn an_endpoint_this_account_does_not_hold_is_not_found() {
    let case = "an endpoint that does not exist";
    let emulator = Emulator::in_region(REGION);
    create_builder(&emulator, case, "stored-builder").await;
    let stored = create_app_block(
        &emulator,
        case,
        "stored-block",
        Some(BUILDER_PACKAGED),
        Some("disks/app.vhd"),
    )
    .await;

    // Rows: a builder name nothing was created under, spanning length, character classes
    // and case — the sentence names the kind, the name the request asked for and the
    // account that was searched.
    for builder in [
        "absent".to_string(),
        "Mixed.Case_builder-9".to_string(),
        "0123456789".to_string(),
        format!("b{}", "7".repeat(100)),
    ] {
        let message = refusal(
            &emulator,
            case,
            "AssociateAppBlockBuilderAppBlock",
            json!({ "AppBlockBuilderName": builder, "AppBlockArn": stored }),
            400,
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            message,
            not_found_sentence(BUILDER_KIND, &builder),
            "case {case:?} ({builder:?}): unexpected sentence {message:?}"
        );
    }

    // Rows: ARNs wrong in the name, the Region, the account and the resource type, so the
    // lookup matches the stored ARN in full.
    for arn in [
        app_block_arn("absent-block"),
        format!("arn:aws:appstream:eu-west-1:{ACCOUNT}:app-block/stored-block"),
        format!("arn:aws:appstream:{REGION}:210987654321:app-block/stored-block"),
        format!("arn:aws:appstream:{REGION}:{ACCOUNT}:application/stored-block"),
    ] {
        let message = refusal(
            &emulator,
            case,
            "AssociateAppBlockBuilderAppBlock",
            json!({ "AppBlockBuilderName": "stored-builder", "AppBlockArn": arn }),
            400,
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            message,
            app_block_not_found_sentence(&arn),
            "case {case:?} ({arn}): unexpected sentence {message:?}"
        );
    }

    // Nothing was recorded by any of those refusals.
    let reported = associated(&emulator, case, json!({})).await;
    assert!(
        reported.is_empty(),
        "case {case:?}: a refused association records nothing, got {reported:?}"
    );
}

#[tokio::test]
async fn max_results_sizes_the_page_and_the_token_walks_every_association_once() {
    /// The page the read renders at most: the model caps
    /// `AppBlockBuilderAppBlockAssociationsList` at 25 members.
    const PUBLISHED_PAGE: usize = 25;

    // One app block, one builder each: a builder holds at most one app block, so the
    // endpoint with more than a page of edges is the app block, and the read is sized and
    // walked from its side.
    let case = "paging one app block's associations";
    let emulator = Emulator::in_region(REGION);
    let arn = create_app_block(
        &emulator,
        case,
        "paging-block",
        Some(BUILDER_PACKAGED),
        Some("disks/app.vhd"),
    )
    .await;
    let stored: BTreeSet<String> = {
        let mut builders = BTreeSet::new();
        for index in 0..30 {
            let builder = format!("paging-builder-{index:02}");
            create_builder(&emulator, case, &builder).await;
            emulator
                .ok(
                    case,
                    "AssociateAppBlockBuilderAppBlock",
                    json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn }),
                )
                .await;
            builders.insert(builder);
        }
        builders
    };

    // Rows: each published bound of `MaxResults`, one past the upper bound, a size in
    // between, and a read that asks for no size at all.
    for max_results in [Some(1), Some(7), Some(PUBLISHED_PAGE), Some(100), None] {
        let page_case = format!("{case} with MaxResults {max_results:?}");
        let mut walked: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        loop {
            let mut request = json!({ "AppBlockArn": arn });
            if let Some(max_results) = max_results {
                request["MaxResults"] = json!(max_results);
            }
            if let Some(token) = &next_token {
                request["NextToken"] = json!(token);
            }
            let payload = emulator
                .ok(
                    &page_case,
                    "DescribeAppBlockBuilderAppBlockAssociations",
                    request,
                )
                .await;
            let reported = payload
                .get("AppBlockBuilderAppBlockAssociations")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| {
                    panic!("case {page_case:?}: expected a list of associations, got {payload}")
                });
            let limit = max_results.map_or(PUBLISHED_PAGE, |size| size.min(PUBLISHED_PAGE));
            let is_last_page = payload.get("NextToken").is_none();
            if is_last_page {
                assert!(
                    !reported.is_empty() && reported.len() <= limit,
                    "case {page_case:?}: the last page must carry 1..={limit} associations, \
                     got {}",
                    reported.len()
                );
            } else {
                assert_eq!(
                    reported.len(),
                    limit,
                    "case {page_case:?}: a page before the last must carry exactly {limit} \
                     associations, got {}",
                    reported.len()
                );
            }
            walked.extend(reported.iter().map(|entry| {
                entry["AppBlockBuilderName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            }));
            next_token = payload
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if next_token.is_none() {
                break;
            }
        }

        assert_eq!(
            walked.len(),
            stored.len(),
            "case {page_case:?}: following the token must report every association exactly \
             once, got {walked:?}"
        );
        assert_eq!(
            walked.iter().cloned().collect::<BTreeSet<String>>(),
            stored,
            "case {page_case:?}: expected {stored:?}, got {walked:?}"
        );
    }

    // A page of fewer than one record is not a size this read can answer with, and a
    // token this service never issued resumes nothing.
    for (label, request) in [
        ("MaxResults 0", json!({ "MaxResults": 0 })),
        ("MaxResults -1", json!({ "MaxResults": -1 })),
        (
            "a token nobody issued",
            json!({ "NextToken": "not-a-token" }),
        ),
    ] {
        assert_error(
            &emulator,
            label,
            "DescribeAppBlockBuilderAppBlockAssociations",
            request,
            400,
            "ValidationException",
        )
        .await;
    }
}

#[tokio::test]
async fn an_association_holds_the_app_block() {
    // Rows: how many builders are attached to the one app block. Any of them holds it, so
    // the delete is refused whatever the count.
    for builders in 1..=3_usize {
        let case = format!("{builders} builder(s) attached to one app block");
        let emulator = Emulator::in_region(REGION);
        let arn = create_app_block(
            &emulator,
            &case,
            "held-block",
            Some(BUILDER_PACKAGED),
            Some("disks/app.vhd"),
        )
        .await;

        // An app block nothing is attached to is deletable, so what refuses the delete
        // below is the association rather than the app block itself.
        let unheld = create_app_block(
            &emulator,
            &case,
            "unheld-block",
            Some(BUILDER_PACKAGED),
            Some("disks/app.vhd"),
        )
        .await;
        assert!(
            !unheld.is_empty(),
            "case {case:?}: the unheld app block must have an ARN"
        );
        emulator
            .ok(&case, "DeleteAppBlock", json!({ "Name": "unheld-block" }))
            .await;

        for index in 0..builders {
            let builder = format!("holding-builder-{index}");
            create_builder(&emulator, &case, &builder).await;
            emulator
                .ok(
                    &case,
                    "AssociateAppBlockBuilderAppBlock",
                    json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn }),
                )
                .await;
        }

        let message = refusal(
            &emulator,
            &case,
            "DeleteAppBlock",
            json!({ "Name": "held-block" }),
            400,
            "ResourceInUseException",
        )
        .await;
        assert_eq!(
            message,
            service_com_amazonaws_appstream_photonadminproxyservice::state::app_block_in_use_message("held-block"),
            "case {case:?}: the refusal must state that the app block is in use, got \
             {message:?}"
        );
    }
}

/// Name shapes spanning `Name`'s pattern `^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$`
/// (`smithy_query(query="operation", operation="AssociateAppBlockBuilderAppBlock")`): the
/// rows differ along length, character classes and case, the last being the longest name
/// the pattern admits (101 characters). None of them occurs in the refusal sentence, so a
/// row can also assert that the sentence echoes nothing of the request.
fn name_shapes() -> Vec<String> {
    vec![
        "block1".to_string(),
        "Mixed.Case_name-9".to_string(),
        "0123456789".to_string(),
        format!("z{}", "9".repeat(100)),
    ]
}

/// Create an app block of `name` with `packaging_type`, carrying the optional members a
/// create admits (`Description`, `DisplayName`, `Tags` —
/// `smithy_query(query="operation", operation="CreateAppBlock")`) when `described`, and
/// answer its ARN. Those members are what the association rule must NOT turn on.
async fn create_described_app_block(
    emulator: &Emulator,
    case: &str,
    name: &str,
    packaging_type: &str,
    described: bool,
) -> String {
    let mut request = json!({
        "Name": name,
        "PackagingType": packaging_type,
        "SourceS3Location": { "S3Bucket": "app-packages", "S3Key": "disks/app.vhd" },
    });
    // `SetupScriptDetails` "must be provided for the CUSTOM PackagingType".
    if packaging_type != BUILDER_PACKAGED {
        request["SetupScriptDetails"] = json!({
            "ScriptS3Location": { "S3Bucket": "app-packages", "S3Key": "scripts/setup.ps1" },
            "ExecutablePath": "setup.ps1",
            "TimeoutInSeconds": 60,
        });
    }
    if described {
        request["Description"] = json!("an app block with every optional member set");
        request["DisplayName"] = json!("Described block");
        request["Tags"] = json!({ "owner": "platform" });
    }
    let created = emulator.ok(case, "CreateAppBlock", request).await;
    created
        .get("AppBlock")
        .and_then(|block| block.get("Arn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the app block's ARN, got {created}"))
        .to_string()
}

#[tokio::test]
async fn the_gate_turns_on_the_stored_packaging_and_on_nothing_else() {
    // Rows: the whole `PackagingType` catalog crossed with name shapes and with the
    // optional metadata of the app block — so the verdict is varied only by the packaging,
    // never by how the resources are named or described.
    for (index, shape) in name_shapes().into_iter().enumerate() {
        for packaging_type in PACKAGING_TYPES {
            let described = index % 2 == 0;
            let case =
                format!("{packaging_type} app block named {shape:?} (described: {described})");
            let emulator = Emulator::in_region(REGION);
            create_builder(&emulator, &case, &shape).await;
            let arn =
                create_described_app_block(&emulator, &case, &shape, packaging_type, described)
                    .await;

            let request = json!({ "AppBlockBuilderName": shape, "AppBlockArn": arn });
            let selector = json!({ "AppBlockBuilderName": shape });
            if *packaging_type == BUILDER_PACKAGED {
                let answer = emulator
                    .ok(&case, "AssociateAppBlockBuilderAppBlock", request)
                    .await;
                assert_eq!(
                    answer,
                    json!({
                        "AppBlockBuilderAppBlockAssociation": {
                            "AppBlockBuilderName": shape,
                            "AppBlockArn": arn,
                        }
                    }),
                    "case {case:?}: the result must carry the association that was \
                     recorded, got {answer}"
                );
                let reported = associated(&emulator, &case, selector).await;
                assert_eq!(
                    reported,
                    vec![json!({ "AppBlockBuilderName": shape, "AppBlockArn": arn })],
                    "case {case:?}: the read must report the recorded association, got \
                     {reported:?}"
                );
            } else {
                let message = refusal(
                    &emulator,
                    &case,
                    "AssociateAppBlockBuilderAppBlock",
                    request,
                    400,
                    "InvalidParameterValueException",
                )
                .await;
                assert_eq!(
                    message,
                    not_supported_sentence(),
                    "case {case:?}: the refusal must state that the app block supports no \
                     association, got {message:?}"
                );
                // The sentence states the rule about the app block that was selected, so
                // it takes no part from the request or from state.
                assert!(
                    !message.contains(shape.as_str()) && !message.contains(arn.as_str()),
                    "case {case:?}: the refusal must echo neither the names nor the ARN, \
                     got {message:?}"
                );
                let reported = associated(&emulator, &case, selector).await;
                assert!(
                    reported.is_empty(),
                    "case {case:?}: a refused association records nothing, got {reported:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_refused_association_leaves_the_admitted_edges_untouched() {
    // Rows: how many associations the store already holds when the refused request
    // arrives, from none to several — one per builder, because a builder holds at most one
    // app block. The refusal is a no-op whatever the store holds, and repeating it is
    // answered the same way every time.
    for admitted in 0..=3_usize {
        let case = format!("a refusal against {admitted} admitted association(s)");
        let emulator = Emulator::in_region(REGION);
        create_builder(&emulator, &case, "untouched-builder").await;

        let mut expected: Vec<Value> = Vec::new();
        for index in 0..admitted {
            let builder = format!("admitted-builder-{index}");
            create_builder(&emulator, &case, &builder).await;
            let arn = create_app_block(
                &emulator,
                &case,
                &format!("admitted-block-{index}"),
                Some(BUILDER_PACKAGED),
                Some("disks/app.vhd"),
            )
            .await;
            emulator
                .ok(
                    &case,
                    "AssociateAppBlockBuilderAppBlock",
                    json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn }),
                )
                .await;
            expected.push(json!({
                "AppBlockBuilderName": builder,
                "AppBlockArn": arn,
            }));
        }

        let refused = create_app_block(
            &emulator,
            &case,
            "refused-block",
            Some("CUSTOM"),
            Some("disks/custom.vhd"),
        )
        .await;
        for attempt in 1..=3_usize {
            let message = refusal(
                &emulator,
                &case,
                "AssociateAppBlockBuilderAppBlock",
                json!({ "AppBlockBuilderName": "untouched-builder", "AppBlockArn": refused }),
                400,
                "InvalidParameterValueException",
            )
            .await;
            assert_eq!(
                message,
                not_supported_sentence(),
                "case {case:?} (attempt {attempt}): unexpected sentence {message:?}"
            );

            let reported = associated(&emulator, &case, json!({})).await;
            assert_eq!(
                reported, expected,
                "case {case:?} (attempt {attempt}): the refusal must leave the admitted \
                 associations as they were, got {reported:?}"
            );
            let from_the_app_block =
                associated(&emulator, &case, json!({ "AppBlockArn": refused })).await;
            assert!(
                from_the_app_block.is_empty(),
                "case {case:?} (attempt {attempt}): the refused app block must be \
                 associated with nothing, got {from_the_app_block:?}"
            );
        }
    }
}

/// The wire code AWS answers a request the service does not permit with
/// (`OperationNotPermittedException`, `httpError` 400 —
/// `smithy_query(query="operation", operation="AssociateAppBlockBuilderAppBlock")`).
const NOT_PERMITTED_CODE: &str = "OperationNotPermittedException";

/// The sentence the service states for an app block builder whose one app block slot is
/// already taken, read from the crate so the test tracks the wording rather than a copy of
/// it.
fn slot_taken_sentence() -> String {
    service_com_amazonaws_appstream_photonadminproxyservice::state::app_block_builder_app_block_slot_taken_message()
}

/// Associate `builder` with `app_block_arn` and answer the association the result carries.
async fn associate(emulator: &Emulator, case: &str, builder: &str, app_block_arn: &str) -> Value {
    emulator
        .ok(
            case,
            "AssociateAppBlockBuilderAppBlock",
            json!({ "AppBlockBuilderName": builder, "AppBlockArn": app_block_arn }),
        )
        .await
}

/// A builder-packaged app block of `name`, carrying an application package already.
async fn create_associable_app_block(emulator: &Emulator, case: &str, name: &str) -> String {
    create_app_block(
        emulator,
        case,
        name,
        Some(BUILDER_PACKAGED),
        Some("disks/app.vhd"),
    )
    .await
}

#[tokio::test]
async fn a_builder_holds_at_most_one_app_block() {
    // Rows: how many FURTHER app blocks are offered to a builder that already holds one,
    // crossed with the name shapes the `Name` pattern admits — the verdict turns on the
    // slot being taken, never on how many app blocks are offered or how anything is named.
    for (index, shape) in name_shapes().into_iter().enumerate() {
        let further = index + 1;
        let case = format!("{further} further app block(s) offered to builder {shape:?}");
        let emulator = Emulator::in_region(REGION);
        create_builder(&emulator, &case, &shape).await;
        let held = create_associable_app_block(&emulator, &case, "held-block").await;
        associate(&emulator, &case, &shape, &held).await;
        let expected = vec![json!({ "AppBlockBuilderName": shape, "AppBlockArn": held })];

        for offer in 0..further {
            let offered =
                create_associable_app_block(&emulator, &case, &format!("offered-block-{offer}"))
                    .await;
            // Twice: the refusal is repeatable and records nothing either time.
            for attempt in 1..=2_usize {
                let message = refusal(
                    &emulator,
                    &case,
                    "AssociateAppBlockBuilderAppBlock",
                    json!({ "AppBlockBuilderName": shape, "AppBlockArn": offered }),
                    400,
                    NOT_PERMITTED_CODE,
                )
                .await;
                assert_eq!(
                    message,
                    slot_taken_sentence(),
                    "case {case:?} (offer {offer}, attempt {attempt}): the refusal must state \
                     that the builder takes no additional app block, got {message:?}"
                );
                // The sentence states the rule about the app block builder that was
                // selected, so it takes no part from the request or from state.
                assert!(
                    !message.contains(shape.as_str())
                        && !message.contains(held.as_str())
                        && !message.contains(offered.as_str()),
                    "case {case:?} (offer {offer}, attempt {attempt}): the refusal must echo \
                     neither the builder's name nor either ARN, got {message:?}"
                );

                let reported = associated(
                    &emulator,
                    &case,
                    json!({ "AppBlockBuilderName": shape.clone() }),
                )
                .await;
                assert_eq!(
                    reported, expected,
                    "case {case:?} (offer {offer}, attempt {attempt}): the builder must still \
                     hold exactly the app block it held, got {reported:?}"
                );
                let from_the_offered =
                    associated(&emulator, &case, json!({ "AppBlockArn": offered })).await;
                assert!(
                    from_the_offered.is_empty(),
                    "case {case:?} (offer {offer}, attempt {attempt}): the refused app block \
                     must be associated with nothing, got {from_the_offered:?}"
                );
            }
        }

        // The app block the builder already holds is no additional app block: the identical
        // request is answered as the association it already is.
        let answer = associate(&emulator, &case, &shape, &held).await;
        assert_eq!(
            answer,
            json!({
                "AppBlockBuilderAppBlockAssociation": {
                    "AppBlockBuilderName": shape,
                    "AppBlockArn": held,
                }
            }),
            "case {case:?}: re-associating the same pair must answer the association it \
             already is, got {answer}"
        );
        let reported = associated(&emulator, &case, json!({})).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: the pair must be associated exactly once, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn disassociating_frees_the_builders_slot() {
    // Rows: how many times the disassociation is repeated before the freed slot is used —
    // removing an edge the store no longer holds changes nothing, so every count leaves the
    // same store behind.
    for repeats in 1..=3_usize {
        let case = format!("disassociating {repeats} time(s)");
        let emulator = Emulator::in_region(REGION);
        create_builder(&emulator, &case, "slot-builder").await;
        let first = create_associable_app_block(&emulator, &case, "first-block").await;
        let second = create_associable_app_block(&emulator, &case, "second-block").await;
        associate(&emulator, &case, "slot-builder", &first).await;

        // While the first app block fills the slot, the second is not permitted.
        let request = json!({ "AppBlockBuilderName": "slot-builder", "AppBlockArn": second });
        let message = refusal(
            &emulator,
            &case,
            "AssociateAppBlockBuilderAppBlock",
            request.clone(),
            400,
            NOT_PERMITTED_CODE,
        )
        .await;
        assert_eq!(
            message,
            slot_taken_sentence(),
            "case {case:?}: unexpected sentence {message:?}"
        );

        for attempt in 1..=repeats {
            let answer = emulator
                .ok(
                    &case,
                    "DisassociateAppBlockBuilderAppBlock",
                    json!({ "AppBlockBuilderName": "slot-builder", "AppBlockArn": first }),
                )
                .await;
            assert_eq!(
                answer,
                json!({}),
                "case {case:?} (attempt {attempt}): the result carries no members, got {answer}"
            );
            let reported = associated(&emulator, &case, json!({})).await;
            assert!(
                reported.is_empty(),
                "case {case:?} (attempt {attempt}): the disassociated builder must hold \
                 nothing, got {reported:?}"
            );
        }

        // The IDENTICAL request that was refused now succeeds: the gate is a live check on
        // what the store holds, not a ban on the pair.
        let answer = emulator
            .ok(&case, "AssociateAppBlockBuilderAppBlock", request)
            .await;
        let edge = json!({ "AppBlockBuilderName": "slot-builder", "AppBlockArn": second });
        assert_eq!(
            answer,
            json!({ "AppBlockBuilderAppBlockAssociation": edge }),
            "case {case:?}: the freed slot must take the app block that was refused, got \
             {answer}"
        );
        for selector in [
            json!({ "AppBlockBuilderName": "slot-builder" }),
            json!({ "AppBlockArn": second }),
            json!({}),
        ] {
            let reported = associated(&emulator, &case, selector.clone()).await;
            assert_eq!(
                reported,
                vec![edge.clone()],
                "case {case:?}: the read of {selector} must report the app block the builder \
                 now holds, got {reported:?}"
            );
        }

        // The slot is taken again, so the app block that was just disassociated is now the
        // one with no room — the rule follows the slot rather than either app block.
        let message = refusal(
            &emulator,
            &case,
            "AssociateAppBlockBuilderAppBlock",
            json!({ "AppBlockBuilderName": "slot-builder", "AppBlockArn": first }),
            400,
            NOT_PERMITTED_CODE,
        )
        .await;
        assert_eq!(
            message,
            slot_taken_sentence(),
            "case {case:?}: unexpected sentence {message:?}"
        );

        // Removing a pair the store does not hold leaves the edge it does hold alone.
        emulator
            .ok(
                &case,
                "DisassociateAppBlockBuilderAppBlock",
                json!({ "AppBlockBuilderName": "slot-builder", "AppBlockArn": first }),
            )
            .await;
        let reported = associated(&emulator, &case, json!({})).await;
        assert_eq!(
            reported,
            vec![edge],
            "case {case:?}: removing an edge the store does not hold must change nothing, got \
             {reported:?}"
        );
    }
}

#[tokio::test]
async fn one_app_block_may_fill_the_slot_of_several_builders() {
    // Rows: how many builders the one app block is associated with. The slot is the
    // BUILDER's, so the app block's side of the association takes any number of edges.
    for builders in 1..=3_usize {
        let case = format!("one app block across {builders} builder(s)");
        let emulator = Emulator::in_region(REGION);
        let arn = create_associable_app_block(&emulator, &case, "shared-block").await;

        let mut expected: Vec<Value> = Vec::new();
        for index in 0..builders {
            let builder = format!("sharing-builder-{index}");
            create_builder(&emulator, &case, &builder).await;
            associate(&emulator, &case, &builder, &arn).await;
            expected.push(json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn }));

            // Each builder reads back holding that one app block, and no other.
            let reported = associated(
                &emulator,
                &case,
                json!({ "AppBlockBuilderName": builder.clone() }),
            )
            .await;
            assert_eq!(
                reported,
                vec![json!({ "AppBlockBuilderName": builder, "AppBlockArn": arn })],
                "case {case:?}: builder {builder:?} must hold the shared app block, got \
                 {reported:?}"
            );
        }

        let reported = associated(&emulator, &case, json!({ "AppBlockArn": arn })).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: the app block's side must report every builder that holds it, got \
             {reported:?}"
        );
    }
}

#[tokio::test]
async fn disassociating_an_endpoint_this_account_does_not_hold_is_not_found() {
    let case = "disassociating an endpoint that does not exist";
    let emulator = Emulator::in_region(REGION);
    create_builder(&emulator, case, "stored-builder").await;
    let stored = create_associable_app_block(&emulator, case, "stored-block").await;
    associate(&emulator, case, "stored-builder", &stored).await;
    let expected = vec![json!({
        "AppBlockBuilderName": "stored-builder",
        "AppBlockArn": stored,
    })];

    // Rows: builder names nothing was created under, spanning length, character classes and
    // case — the sentence names the kind, the name the REQUEST asked for and the account
    // that was searched.
    for builder in [
        "absent".to_string(),
        "Mixed.Case_builder-9".to_string(),
        "0123456789".to_string(),
        format!("b{}", "7".repeat(100)),
    ] {
        let message = refusal(
            &emulator,
            case,
            "DisassociateAppBlockBuilderAppBlock",
            json!({ "AppBlockBuilderName": builder, "AppBlockArn": stored }),
            400,
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            message,
            not_found_sentence(BUILDER_KIND, &builder),
            "case {case:?} ({builder:?}): unexpected sentence {message:?}"
        );
    }

    // Rows: ARNs wrong in the name, the Region, the account and the resource type, so the
    // lookup matches the stored ARN in full.
    for arn in [
        app_block_arn("absent-block"),
        format!("arn:aws:appstream:eu-west-1:{ACCOUNT}:app-block/stored-block"),
        format!("arn:aws:appstream:{REGION}:210987654321:app-block/stored-block"),
        format!("arn:aws:appstream:{REGION}:{ACCOUNT}:application/stored-block"),
    ] {
        let message = refusal(
            &emulator,
            case,
            "DisassociateAppBlockBuilderAppBlock",
            json!({ "AppBlockBuilderName": "stored-builder", "AppBlockArn": arn }),
            400,
            "ResourceNotFoundException",
        )
        .await;
        assert_eq!(
            message,
            app_block_not_found_sentence(&arn),
            "case {case:?} ({arn}): unexpected sentence {message:?}"
        );
    }

    // None of those refusals removed the edge the store holds.
    let reported = associated(&emulator, case, json!({})).await;
    assert_eq!(
        reported, expected,
        "case {case:?}: a refused disassociation removes nothing, got {reported:?}"
    );
}
