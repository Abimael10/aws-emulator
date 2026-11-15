// SPDX-License-Identifier: CC0-1.0

//! The vocabulary this service refuses an unreadable request DOCUMENT in, end to end over
//! HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the request boundary, the handler and serialization — the same path a
//! client takes. Two rules, each about a refusal a client branches on:
//!
//! * a constraint broken by a member NESTED inside the request's payload makes the document
//!   one this API cannot read, which CloudFront answers with HTTP 400 and `MalformedXML`:
//!   `ValidationException` is no error this service's model declares, so a client that
//!   catches CloudFront's own errors would fall through to a generic failure instead of its
//!   input-correction path. The sentence the boundary wrote states the member and the
//!   constraint it broke and is kept as it stands — except for a `@pattern`, which is
//!   reported as the pattern the SERVICE publishes for the member: unanchored (a pattern
//!   constraint is a containment test) and with the partition segment of an ARN generalised
//!   over the partitions an ARN can name.
//! * which values a member admits is a property of the MEMBER, so one member is refused in
//!   one sentence whichever layer read the value: a CloudFront function association's event
//!   type admits `viewer-request` and `viewer-response` — "you cannot use origin-facing event
//!   types (origin-request and origin-response) with a CloudFront function"
//!   (`smithy_query(query="doc",shape="FunctionAssociation",member="EventType")`) — while the
//!   `EventType` enum it targets carries the origin-facing values too, so the boundary reads
//!   a wider set than the member admits. Both report the same set, in the same words.
//!
//! The rows span what each rule generalises over: for the first, the constraint broken (a
//! required member that is absent, a `@length` maximum, a `@pattern`), the operation and the
//! payload it carries (an invalidation batch, a tag set, a distribution configuration) and
//! the DEPTH of the member inside the payload, beside the control that an absent payload is
//! still reported as the payload member rather than as the member nested in it; for the
//! second, the value submitted (a value the enum admits but the member does not, both of
//! them; a value no enum admits, in three forms) and the behaviour that carries the
//! association (the default cache behaviour and an ordered one), which is what the member
//! path generalises over.
//!
//! The harness (the router and the request helpers) is [`crate::distribution_lifecycle`]'s,
//! reused rather than copied; the expected sentences are LITERALS here, so a wrong constant
//! in the crate cannot make this suite pass.

use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{
    call, create_distribution, create_invalidation, emulator, Answer, API_VERSION,
};

/// The code CloudFront answers a request document it cannot read with.
const MALFORMED_XML: &str = "MalformedXML";

/// The code the FRAMEWORK names a request it refused before any handler ran. Not a shape this
/// service's model declares, so no response of this service may carry it for a document
/// defect.
const FRAMEWORK_REFUSAL_CODE: &str = "ValidationException";

/// A function ARN in the shape `FunctionAssociation$FunctionARN` admits
/// (`^arn:aws:cloudfront::[0-9]{12}:function\/[a-zA-Z0-9-_]{1,64}$`), naming a function this
/// account does not hold: the rows below are about the EVENT TYPE beside it, which every
/// refusal under test is decided on before anything resolves the function.
const WELL_FORMED_FUNCTION_ARN: &str = "arn:aws:cloudfront::123456789012:function/absent-fn";

// ---------------------------------------------------------------------------
// A constraint on a member of the request document
// ---------------------------------------------------------------------------

/// One request whose document breaks a constraint, and the sentence this service refuses it
/// with.
struct Row {
    case: &'static str,
    method: &'static str,
    uri: String,
    body: String,
    /// The whole `Message` the refusal carries.
    message: String,
}

/// A `DistributionConfig` whose default cache behaviour carries `associations`, under
/// `reference`.
fn config_with_associations(reference: &str, associations: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>origin.example.com</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{associations}\
         </DefaultCacheBehavior>\
         <Comment>{reference}</Comment><PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled></DistributionConfig>"
    )
}

/// A `DistributionConfig` whose ORDERED cache behaviour carries `associations`, under
/// `reference`: the same configuration, with the association one level deeper.
fn config_with_ordered_associations(reference: &str, associations: &str) -> String {
    format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>{reference}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items>\
         <Origin><Id>primary</Id><DomainName>origin.example.com</DomainName></Origin>\
         </Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy></DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>1</Quantity><Items><CacheBehavior>\
         <PathPattern>/images/*</PathPattern><TargetOriginId>primary</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>{associations}\
         </CacheBehavior></Items></CacheBehaviors>\
         <Comment>{reference}</Comment><PriceClass>PriceClass_All</PriceClass>\
         <Enabled>true</Enabled></DistributionConfig>"
    )
}

/// The `FunctionAssociations` wrapper attaching `arn` at `event_type`, stating its own length.
fn function_associations(arn: &str, event_type: &str) -> String {
    format!(
        "<FunctionAssociations><Quantity>1</Quantity><Items><FunctionAssociation>\
         <FunctionARN>{arn}</FunctionARN><EventType>{event_type}</EventType>\
         </FunctionAssociation></Items></FunctionAssociations>"
    )
}

/// A distribution to submit invalidation batches against.
async fn distribution(router: &MultiServiceRouter) -> String {
    let created = create_distribution(router, &config_with_associations("ref-vocabulary", "")).await;
    assert_eq!(
        created.status, 201,
        "case the fixture distribution: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    created
        .root()
        .text_at("Id")
        .expect("the created distribution reports an Id")
        .to_string()
}

/// The `Message` a refusal carries.
fn message(answer: &Answer) -> String {
    answer
        .root()
        .find_text("Message")
        .unwrap_or_default()
        .to_string()
}

/// Every constraint a member of the request document can break is refused with HTTP 400 and
/// `MalformedXML`, carrying the sentence that names the member and the constraint — never with
/// the framework's own `ValidationException`.
#[tokio::test]
async fn a_constraint_on_a_document_member_is_refused_as_malformed_xml() {
    let router = emulator();
    let id = distribution(&router).await;
    let invalidation = format!("/{API_VERSION}/distribution/{id}/invalidation");
    let distributions = format!("/{API_VERSION}/distribution");

    let rows = vec![
        // A required member of the payload itself.
        Row {
            case: "an invalidation batch naming no caller reference",
            method: "POST",
            uri: invalidation.clone(),
            body: format!(
                "<InvalidationBatch xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                 <Paths><Quantity>1</Quantity><Items><Path>/one.html</Path></Items></Paths>\
                 </InvalidationBatch>"
            ),
            message: "1 validation error detected: Value null at \
                      'invalidationBatch.callerReference' failed to satisfy constraint: Member \
                      must not be null"
                .to_string(),
        },
        // A required member one level deeper.
        Row {
            case: "an invalidation batch whose paths state no quantity",
            method: "POST",
            uri: invalidation.clone(),
            body: format!(
                "<InvalidationBatch xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
                 <Paths><Items><Path>/one.html</Path></Items></Paths>\
                 <CallerReference>ref-no-quantity</CallerReference></InvalidationBatch>"
            ),
            message: "1 validation error detected: Value null at \
                      'invalidationBatch.paths.quantity' failed to satisfy constraint: Member \
                      must not be null"
                .to_string(),
        },
        // A `@length` maximum, on a member of a list inside the payload. `TagKey` is
        // documented "between 1 and 128 characters"
        // (`smithy_query(query="shape",shape="TagKey")`).
        Row {
            case: "a tag key past the length a tag key may carry",
            method: "POST",
            uri: format!("/{API_VERSION}/tagging?Operation=Tag&Resource=arn:aws:cloudfront::123456789012:distribution/{id}"),
            body: format!(
                "<Tags xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\"><Items>\
                 <Tag><Key>{}</Key><Value>v</Value></Tag></Items></Tags>",
                "k".repeat(129)
            ),
            message: format!(
                "1 validation error detected: Value '{}' at 'tags.items.1.member.key' failed to \
                 satisfy constraint: Member must have length less than or equal to 128",
                "k".repeat(129)
            ),
        },
        // A `@pattern`, reported as the service publishes it: unanchored, and with the
        // partition segment generalised.
        Row {
            case: "a function association naming an ARN of another service",
            method: "POST",
            uri: distributions.clone(),
            body: config_with_associations(
                "ref-bad-function-arn",
                &function_associations(
                    "arn:aws:lambda:us-east-1:123456789012:function:edge-fn",
                    "viewer-request",
                ),
            ),
            message: "1 validation error detected: Value \
                      'arn:aws:lambda:us-east-1:123456789012:function:edge-fn' at \
                      'distributionConfig.defaultCacheBehavior.functionAssociations.items.1.\
                      member.functionARN' failed to satisfy constraint: Member must satisfy \
                      regular expression pattern: \
                      arn:aws(-[a-z]+)*:cloudfront::[0-9]{12}:function\\/[a-zA-Z0-9-_]{1,64}"
                .to_string(),
        },
        // The control: a request with a body to give that carried NONE has no payload at all,
        // so the member that is really absent is the payload itself rather than a member
        // nested inside it.
        Row {
            case: "a create carrying no document at all",
            method: "POST",
            uri: distributions.clone(),
            body: String::new(),
            message: "1 validation error detected: Value null at 'distributionConfig' failed to \
                      satisfy constraint: Member must not be null"
                .to_string(),
        },
    ];

    for row in rows {
        let answer = call(
            &router,
            row.method,
            &row.uri,
            Some(&row.body),
            None,
        )
        .await;
        let case = row.case;
        assert_eq!(
            answer.status, 400,
            "case {case}: expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            MALFORMED_XML,
            "case {case}: expected {MALFORMED_XML}, got {:?} in {}",
            answer.error_code(),
            answer.raw
        );
        assert_ne!(
            answer.error_code(),
            FRAMEWORK_REFUSAL_CODE,
            "case {case}: no response may carry the framework's own code, got {}",
            answer.raw
        );
        let actual = message(&answer);
        assert_eq!(
            actual, row.message,
            "case {case}: expected the sentence {:?}, got {actual:?}",
            row.message
        );
    }
}

// ---------------------------------------------------------------------------
// One member, one sentence
// ---------------------------------------------------------------------------

/// Where the association sits in the configuration, and the member path the refusal names it
/// by.
#[derive(Clone, Copy, Debug)]
enum Behavior {
    /// The default cache behaviour.
    Default,
    /// The first ordered cache behaviour.
    Ordered,
}

impl Behavior {
    /// The member path a refusal about this behaviour's first function association names.
    fn member_path(self) -> &'static str {
        match self {
            Self::Default => {
                "distributionConfig.defaultCacheBehavior.functionAssociations.items.1.member.\
                 eventType"
            }
            Self::Ordered => {
                "distributionConfig.cacheBehaviors.items.1.member.functionAssociations.items.1.\
                 member.eventType"
            }
        }
    }

    /// The configuration carrying `associations` on this behaviour.
    fn config(self, reference: &str, associations: &str) -> String {
        match self {
            Self::Default => config_with_associations(reference, associations),
            Self::Ordered => config_with_ordered_associations(reference, associations),
        }
    }
}

/// A CloudFront function association admits exactly `viewer-request` and `viewer-response`, so
/// every other value is refused in ONE sentence — the same set, the same words and the same
/// code — whether the value is one the `EventType` enum admits (read by the operation) or one
/// it does not (read by the request boundary).
#[tokio::test]
async fn one_sentence_states_the_set_a_function_association_event_type_admits() {
    let router = emulator();

    // Each row: the value submitted. The first two are values the model's `EventType` enum
    // admits — "origin-request" and "origin-response", the origin-facing half no CloudFront
    // function runs on — and the rest are values no enum admits, in the forms a client
    // mistypes one in.
    let values = [
        "origin-request",
        "origin-response",
        "Viewer-Request",
        "viewer_request",
        "viewer-request-and-response",
    ];

    for behavior in [Behavior::Default, Behavior::Ordered] {
        for (index, value) in values.iter().enumerate() {
            let case = format!("{behavior:?} at {value:?}");
            let body = behavior.config(
                &format!("ref-event-{index}-{behavior:?}"),
                &function_associations(WELL_FORMED_FUNCTION_ARN, value),
            );
            let answer = create_distribution(&router, &body).await;
            assert_eq!(
                answer.status, 400,
                "case {case}: expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                MALFORMED_XML,
                "case {case}: expected {MALFORMED_XML}, got {:?} in {}",
                answer.error_code(),
                answer.raw
            );
            let expected = format!(
                "1 validation errors detected: Value '{value}' at '{}' failed to satisfy \
                 constraint: Member must satisfy value set: [viewer-request, viewer-response]",
                behavior.member_path()
            );
            let actual = message(&answer);
            assert_eq!(
                actual, expected,
                "case {case}: expected the sentence {expected:?}, got {actual:?}"
            );
        }
    }

    // The control: the same association at a value the member DOES admit is read, so the
    // configuration is refused for the function it names rather than for its event type.
    let accepted = create_distribution(
        &router,
        &config_with_associations(
            "ref-event-viewer",
            &function_associations(WELL_FORMED_FUNCTION_ARN, "viewer-request"),
        ),
    )
    .await;
    let actual = message(&accepted);
    assert!(
        !actual.contains("value set"),
        "case a viewer event: expected no value-set refusal, got {actual:?}"
    );

    // And nothing above was stored: no invalidation surface exists for a distribution the
    // refusals never created.
    let absent = create_invalidation(
        &router,
        "E1REFUSEDDIST",
        &format!(
            "<InvalidationBatch xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
             <Paths><Quantity>1</Quantity><Items><Path>/one.html</Path></Items></Paths>\
             <CallerReference>ref-absent</CallerReference></InvalidationBatch>"
        ),
    )
    .await;
    assert_eq!(
        absent.status, 404,
        "case a refused create stores nothing: expected HTTP 404 for a distribution no create \
         made, got {} with {}",
        absent.status, absent.raw
    );
}
