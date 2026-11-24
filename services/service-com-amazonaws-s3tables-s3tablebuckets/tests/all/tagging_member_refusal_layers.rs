// SPDX-License-Identifier: CC0-1.0

//! WHICH layer refuses a tagging request, as the error code a client branches on
//! reports it.
//!
//! The rule under test: a modelled constraint on an operation input's member is
//! checked in front of the service, so a request breaking one never reaches a
//! handler — it is refused with HTTP 400 under the request validator's own code
//! (`ValidationException`, which names no shape of this model, because the
//! refusal is of the request's shape and not of anything the service decided),
//! carrying that layer's envelope with the clause stated at the member the model
//! declares the constraint on; the member classes the service words a sentence
//! of its own for — the ARNs its tag surface is addressed by — carry instead the
//! one client-input error the model declares (`BadRequestException`) and a fixed
//! sentence. Which layer answers is decided by the MEMBER the constraint is
//! declared on and by nothing else: not by the taggable resource the request
//! addresses, not by whether that resource exists, and not by the path the
//! request arrived on. Either refusal is stated before anything is read or
//! written, so every resource's tags survive it.
//!
//! The one member of this service's inputs carrying a constraint declared on a
//! COLLECTION's element shape is `UntagResource.tagKeys`, a `TagKeyList` whose
//! member targets `TagKey` (`@length(1, 128)`, by `smithy_query(query="shape",
//! shape="TagKeyList,TagKey")`), so the bounds below are the model's and the
//! clause states them at the list member.
//!
//! What the suites beside this one pin, and this one does not repeat: the form
//! of that clause over the bounds an element can miss and the positions it can
//! sit in (`collection_element_constraint_refusals.rs`), the service's own
//! sentence per refused ARN across its operations (`constraint_refusals.rs`,
//! `resource_tagging.rs`, `untag_resource.rs`), and the validator's envelope
//! over the other members of this model (`parameter_validation_refusals.rs`).
//! This suite holds the two layers side by side on ONE operation, and spans what
//! the layer choice must be independent of: the taggable resource form (a table
//! bucket, a table, an ARN nothing was created under) and the arrival path (a
//! client's own, and the in-process callback another emulated service reaches
//! this one through).

use std::collections::BTreeMap;

use serde_json::json;

use crate::harness::{
    expected_bucket_arn, listed_tags, served, tag_map, tags_body, Emulator, Reply,
};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The code the request validator answers under.
const PARAMETER_VALIDATION: &str = "ValidationException";

/// The one client-input error this model declares, which every refusal the
/// SERVICE makes carries (`smithy_query(query="shape",
/// shape="BadRequestException")`).
const BAD_REQUEST: &str = "BadRequestException";

/// Every error `UntagResource` declares (`smithy_query(query="operation",
/// operation="UntagResource")`). The validator's code is none of them: that is
/// what tells a client the request was refused for the shape of a parameter
/// rather than by a rule of the service.
const MODELLED_ERRORS: [&str; 6] = [
    "BadRequestException",
    "ConflictException",
    "ForbiddenException",
    "InternalServerErrorException",
    "NotFoundException",
    "TooManyRequestsException",
];

/// The sentence the SERVICE states a value that addresses no taggable resource
/// with.
const INVALID_RESOURCE_ARN: &str = "The specified resourceArn is not valid.";

/// `TagKey`'s length bounds, as `TagKeyList`'s member declares them
/// (`smithy_query(query="shape", shape="TagKeyList,TagKey")`).
const TAG_KEY_MIN: usize = 1;
const TAG_KEY_MAX: usize = 128;

/// A tag key every resource below carries, so a call that was NOT refused would
/// be visible in a later read.
const RESIDENT_KEY: &str = "owner";

/// A second resident key, so a refusal can be shown to take neither.
const OTHER_KEY: &str = "env";

/// The arrival paths a request reaches this operation on.
const ARRIVALS: [(&str, bool); 2] = [("a client", false), ("the in-process callback", true)];

/// The whole sentence a refusal of a `tagKeys` element carries: the element
/// shape's declared set stated at the list member, with no element position and
/// no value of the request in it.
fn expected_element_message() -> String {
    format!(
        "1 validation error detected: Value at 'tagKeys' failed to satisfy constraint: Member \
         must satisfy constraint: [Member must have length less than or equal to {TAG_KEY_MAX}, \
         Member must have length greater than or equal to {TAG_KEY_MIN}]"
    )
}

/// A tag key of `length` characters, so a case states the length it drives
/// rather than a literal.
fn key_of(length: usize) -> String {
    "k".repeat(length)
}

/// One taggable resource, and the tags a read of it must keep reporting.
struct Subject {
    label: &'static str,
    arn: String,
}

/// The resources a case addresses: one of each taggable form, plus an ARN of a
/// bucket nothing was created under — well formed, so the constraint on the list
/// member is all that can refuse the call.
struct Subjects {
    bucket: Subject,
    table: Subject,
    absent: Subject,
    /// The tag set both live resources carry.
    resident: BTreeMap<String, String>,
}

impl Subjects {
    /// Every resource a refused call is addressed to, live or not.
    fn addressed(&self) -> [&Subject; 3] {
        [&self.bucket, &self.table, &self.absent]
    }

    /// The resources a later read reports tags for.
    fn live(&self) -> [&Subject; 2] {
        [&self.bucket, &self.table]
    }
}

/// Create a table bucket and a table in it, both carrying the same two tags.
async fn subjects(emulator: &Emulator, bucket: &str) -> Subjects {
    let bucket_arn = served(
        emulator
            .create_table_bucket(REGION, &json!({ "name": bucket }))
            .await,
        "CreateTableBucket",
        bucket,
    )
    .text("arn");
    served(
        emulator
            .create_namespace(REGION, &bucket_arn, &json!({ "namespace": ["ns"] }))
            .await,
        "CreateNamespace",
        bucket,
    );
    let table_arn = served(
        emulator
            .create_table(
                REGION,
                &bucket_arn,
                "ns",
                &json!({ "name": "tagged_table", "format": "ICEBERG" }),
            )
            .await,
        "CreateTable",
        bucket,
    )
    .text("tableARN");

    let resident = tag_map(&[RESIDENT_KEY, OTHER_KEY]);
    for arn in [&bucket_arn, &table_arn] {
        served(
            emulator.tag_resource(REGION, arn, &tags_body(&resident)).await,
            "TagResource",
            bucket,
        );
    }

    Subjects {
        bucket: Subject {
            label: "a table bucket",
            arn: bucket_arn,
        },
        table: Subject {
            label: "a table",
            arn: table_arn,
        },
        absent: Subject {
            label: "an ARN nothing was created under",
            arn: expected_bucket_arn(REGION, &format!("{bucket}-absent")),
        },
        resident,
    }
}

/// `UntagResource` on whichever arrival path the case drives.
async fn untag(emulator: &Emulator, arn: &str, keys: &[&str], in_process: bool) -> Reply {
    if in_process {
        emulator.untag_resource_in_process(REGION, arn, keys).await
    } else {
        emulator.untag_resource(REGION, arn, keys).await
    }
}

/// Assert `reply` is the request validator's refusal of the `tagKeys` member:
/// its own code, which is none of the errors the operation models, and the
/// clause stated at the list member with nothing of the request echoed.
fn assert_validator_refusal(reply: &Reply, case: &str, sent: &[String]) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(PARAMETER_VALIDATION),
        "case {case:?}: expected the request validator's code, got {actual:?}",
        actual = reply.error_code
    );
    for modelled in MODELLED_ERRORS {
        assert_ne!(
            reply.error_code.as_deref(),
            Some(modelled),
            "case {case:?}: a parameter refusal must not answer under a modelled error, got \
             {actual:?}",
            actual = reply.error_code
        );
    }
    let message = reply.message();
    assert_eq!(
        message,
        expected_element_message(),
        "case {case:?}: expected the element set stated at the list member, got {message:?}"
    );
    for value in sent.iter().filter(|value| value.len() > TAG_KEY_MAX) {
        assert!(
            !message.contains(value.as_str()),
            "case {case:?}: the refusal must echo no value, got {message:?}"
        );
    }
}

/// Assert `reply` is the SERVICE's own refusal: the one client-input error its
/// model declares, worded with the sentence it states that class with.
fn assert_service_refusal(reply: &Reply, case: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some(BAD_REQUEST),
        "case {case:?}: expected the modelled {BAD_REQUEST}, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        INVALID_RESOURCE_ARN,
        "case {case:?}: expected the service's fixed sentence, got {actual:?}",
        actual = reply.message()
    );
}

/// Assert both live resources still carry exactly the tags they were given.
async fn assert_tags_survived(emulator: &Emulator, subjects: &Subjects, case: &str) {
    for subject in subjects.live() {
        let after = listed_tags(emulator, REGION, &subject.arn, case).await;
        assert_eq!(
            after, subjects.resident,
            "case {case:?}: a refused removal must leave {label} as it was, got {after:?}",
            label = subject.label
        );
    }
}

/// The key lists a refused case names: an element one character over the
/// maximum, one far over it beside a resident key, and one under the minimum —
/// each travelling with a key the resource really carries, so a call that was
/// served rather than refused would show up in a later read.
fn refused_key_cases() -> Vec<(String, Vec<String>)> {
    let one_over = key_of(TAG_KEY_MAX + 1);
    let far_over = key_of(TAG_KEY_MAX * 3);
    let under = String::new();

    vec![
        (
            "an element one character over the maximum".to_string(),
            vec![one_over.clone(), RESIDENT_KEY.to_string()],
        ),
        (
            "an element far over the maximum".to_string(),
            vec![RESIDENT_KEY.to_string(), far_over],
        ),
        (
            "an element under the minimum".to_string(),
            vec![under, OTHER_KEY.to_string()],
        ),
    ]
}

#[tokio::test]
async fn an_element_constraint_is_the_validators_refusal_whatever_the_call_addresses() {
    let emulator = Emulator::new();
    let subjects = subjects(&emulator, "refusal-layer-bucket").await;

    for (label, keys) in refused_key_cases() {
        for subject in subjects.addressed() {
            for (arrival, in_process) in ARRIVALS {
                let case = format!("{label} on {form} from {arrival}", form = subject.label);
                let borrowed: Vec<&str> = keys.iter().map(String::as_str).collect();
                let refused = untag(&emulator, &subject.arn, &borrowed, in_process).await;
                assert_validator_refusal(&refused, &case, &keys);
            }
        }
        assert_tags_survived(&emulator, &subjects, &label).await;
    }
}

#[tokio::test]
async fn the_member_class_decides_which_layer_refuses_a_removal() {
    let emulator = Emulator::new();
    let subjects = subjects(&emulator, "refusal-layer-classes").await;

    // The same operation, the same well-formed key, addressed by values that
    // cannot be a taggable resource's ARN: nothing ARN-shaped, an account id one
    // digit short of the modelled twelve, and one naming no resource after the
    // bucket segment. That member is a class the service words itself, so the
    // refusal is the modelled client-input error.
    let malformed_arns = [
        "not-a-taggable-arn".to_string(),
        format!("arn:aws:s3tables:{REGION}:00000000000:bucket/refusal-layer-classes"),
        format!("arn:aws:s3tables:{REGION}:000000000000:bucket/"),
    ];
    let mut service_codes = Vec::new();
    for malformed in &malformed_arns {
        for (arrival, in_process) in ARRIVALS {
            let case = format!("the resourceArn {malformed:?} from {arrival}");
            let refused = untag(&emulator, malformed, &[RESIDENT_KEY], in_process).await;
            assert_service_refusal(&refused, &case);
            service_codes.push(refused.error_code.clone());
        }
    }

    // The same operation on a resident resource, refused for the shape of the
    // list member instead: the other layer, under a code none of this model's
    // errors carries.
    let too_long = key_of(TAG_KEY_MAX + 1);
    let validator = untag(&emulator, &subjects.bucket.arn, &[too_long.as_str()], false).await;
    assert_validator_refusal(&validator, "an element over the maximum", &[too_long]);

    for code in &service_codes {
        assert_ne!(
            code.as_deref(),
            validator.error_code.as_deref(),
            "case \"the two layers\": a parameter refusal and a refusal the service worded \
             itself must differ in code, got {code:?} for both"
        );
    }

    assert_tags_survived(&emulator, &subjects, "both layers").await;
}

#[tokio::test]
async fn a_key_inside_the_bound_is_served_on_every_taggable_form() {
    let emulator = Emulator::new();
    let subjects = subjects(&emulator, "refusal-layer-bounds").await;

    // The shortest and the longest key `TagKey` admits, so what the rows above
    // refuse is the keys outside the bound and not the member itself — on each
    // taggable form, because the bound belongs to the member.
    for length in [TAG_KEY_MIN, TAG_KEY_MAX] {
        let key = key_of(length);
        for subject in subjects.live() {
            let case =
                format!("a key of {length} characters on {form}", form = subject.label);
            let mut tags = subjects.resident.clone();
            tags.extend(tag_map(std::slice::from_ref(&key)));
            served(
                emulator
                    .tag_resource(REGION, &subject.arn, &tags_body(&tags))
                    .await,
                "TagResource",
                &case,
            );

            let removed = emulator
                .untag_resource(REGION, &subject.arn, &[key.as_str()])
                .await;
            assert_eq!(
                removed.status, 204,
                "case {case:?}: a key at the published bound must be served, got {status} {body}",
                status = removed.status,
                body = removed.body
            );
            let after = listed_tags(&emulator, REGION, &subject.arn, &case).await;
            assert_eq!(
                after, subjects.resident,
                "case {case:?}: exactly the named key must go, got {after:?}"
            );
        }
    }
}
