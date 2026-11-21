// SPDX-License-Identifier: CC0-1.0

//! What a Config refusal says about the REQUEST that drew it, end to end over the wire.
//!
//! An error code is shared by many legs, so the message is the whole of what a client can
//! branch on — and whether it carries anything of the request is itself the rule. Five
//! rules are pinned here, each over a TABLE of inputs rather than one recorded call:
//!
//! 1. a command this service cannot read as a Config query is a SYNTAX error, reported at
//!    the position of the submitted expression where the reading stopped — the token the
//!    reading could not get past, or, for a command cut short, the position one past its
//!    last character — and nothing else: no prose of whatever read the text;
//! 2. `GetAggregateResourceConfig` refuses an identifier the aggregator holds no current
//!    configuration item for with ONE fixed sentence, which quotes none of the four
//!    members that identifier carries, whatever they are;
//! 3. the service-linked recorder surface words a refusal for the DIMENSION the request
//!    selected the recorder by: the service-principal legs name the service principal, and
//!    never the recorder name the caller never sent;
//! 4. `PutResourceConfig` refuses a `ResourceType` the CloudFormation registry holds no
//!    registration for as the input-validation failure of the value it was sent —
//!    `ValidationException`, client 400 — and records nothing. The refusal is keyed on that
//!    member alone, so it is the same whatever else the request carries, and it never
//!    clears, because nothing a caller retries registers a type;
//! 5. the position a syntax error reports counts CHARACTERS of the submitted expression,
//!    and is always a position that expression has: a line it was written over, at a
//!    character of that line or the one just past its end.
//!
//! Every case drives the emulator the way a client does — an `awsJson1_1` `POST /`
//! through the same `MultiServiceRouter` the production router builds — reusing the
//! `Emulator` harness `config_setup_and_rule_loop` defines and the `refusal` helper
//! `config_connectors` shares.

use serde_json::{json, Value};

use crate::config_connectors::refusal;
use crate::config_setup_and_rule_loop::Emulator;
use crate::config_write_path_validation::{recording_emulator, resource_config};

/// The one sentence `GetAggregateResourceConfig` refuses a never-discovered resource
/// with, written out here rather than read from the service's own constant so this suite
/// PINS it instead of restating it.
const NOT_DISCOVERED_TEXT: &str =
    "You have specified a resource that is either unknown or has not been discovered.";

/// The service-principal variant of the sentence Config refuses a configuration recorder
/// it cannot resolve with, pinned here for the same reason.
const UNRECOGNIZED_PRINCIPAL_TEXT: &str =
    "The service principal of the configuration recorder you entered cannot be found. \
     Verify the configuration recorder service principal, and try again.";

/// A service Config DOES link a configuration recorder for, from the catalogue
/// `smithy_query(query="doc", shape="PutServiceLinkedConfigurationRecorder")` points at.
const LINKED_PRINCIPAL: &str = "observabilityadmin.amazonaws.com";

/// `ResourceId`'s length maximum, from `smithy_query(query="shape", shape="ResourceId")`:
/// 1..768.
const RESOURCE_ID_MAX_LENGTH: usize = 768;

/// Entries of `com.amazonaws.configservice#ResourceType`, from
/// `smithy_query(query="shape", shape="ResourceType")`.
const EC2_INSTANCE: &str = "AWS::EC2::Instance";
const EC2_VOLUME: &str = "AWS::EC2::Volume";
const S3_BUCKET: &str = "AWS::S3::Bucket";

/// The emulator's own account id, the account its aggregator aggregates.
fn account() -> String {
    emulator_server::emulator_account_id().to_string()
}

/// One `AggregateResourceIdentifier`, with every member the model requires.
fn identifier(account_id: &str, region: &str, resource_type: &str, resource_id: &str) -> Value {
    json!({
        "SourceAccountId": account_id,
        "SourceRegion": region,
        "ResourceType": resource_type,
        "ResourceId": resource_id,
    })
}

/// An emulator holding an aggregator named `name` over this account in every Region.
async fn emulator_with_an_aggregator(name: &str) -> Emulator {
    let emulator = Emulator::new();
    emulator
        .ok(
            "PutConfigurationAggregator",
            &json!({
                "ConfigurationAggregatorName": name,
                "AccountAggregationSources": [{
                    "AccountIds": [account()],
                    "AllAwsRegions": true,
                }],
            }),
        )
        .await;
    emulator
}

/// The message `SelectResourceConfig` refuses `expression` with, requiring the modelled
/// `InvalidExpressionException` and its client status.
async fn refused_expression(case: &str, emulator: &Emulator, expression: &str) -> String {
    let (status, code, message) = refusal(
        emulator,
        "SelectResourceConfig",
        &json!({ "Expression": expression }),
    )
    .await;
    assert_eq!(
        (status, code.as_str()),
        (400, "InvalidExpressionException"),
        "case {case:?}: expected HTTP 400 InvalidExpressionException, got {status} {code} with \
         message {message:?}"
    );
    message
}

#[tokio::test]
async fn an_unreadable_query_is_a_syntax_error_at_the_position_the_reading_stopped_at() {
    let emulator = Emulator::new();

    // A command whose reading stopped AT A TOKEN of it reports that token's position.
    // Rows differ along what makes the command unreadable — a keyword standing where a
    // property belongs, a command that opens with something other than a Config query's
    // own keyword, a clause the Config query form has no place for, an alias — and along
    // WHERE in the text that happens: the token is named per row and the column expected
    // is the one that token starts at in the expression the row sends, so no row carries
    // a position written by hand.
    let cases: [(&str, &str, &str); 7] = [
        (
            "a keyword standing where the first property belongs",
            "SELECT WHERE",
            "WHERE",
        ),
        (
            "a command that is not a SELECT at all",
            "DELETE resourceId",
            "DELETE",
        ),
        (
            // The recorded case of a stored query whose expression is not the language
            // at all: the reading stops at the very first token.
            "text that is no command of the language",
            "NOT VALID CONFIG SQL ((()",
            "NOT",
        ),
        (
            "a DISTINCT a Config query has no place for",
            "SELECT DISTINCT resourceId",
            "DISTINCT",
        ),
        (
            "a FROM clause, which a Config query has no place for",
            "SELECT resourceId FROM aws",
            "FROM",
        ),
        (
            "an alias on a selected property",
            "SELECT resourceId AS id",
            "AS",
        ),
        (
            "a HAVING clause, which a Config query has no place for",
            "SELECT resourceType GROUP BY resourceType HAVING COUNT(*) > 1",
            "HAVING",
        ),
    ];

    for (case, expression, token) in cases {
        let column = expression
            .find(token)
            .unwrap_or_else(|| panic!("case {case:?}: {token:?} is part of {expression:?}"))
            + 1;
        let expected = format!("syntax error at line 1, column {column}");
        let message = refused_expression(case, &emulator, expression).await;
        assert_eq!(
            message, expected,
            "case {case:?}: expected the reading of {expression:?} to stop at {token:?}, got \
             {message:?}"
        );
        // Nothing of the expression, and nothing of whatever read it, travels in the
        // sentence: the position is the whole refusal.
        assert!(
            !message.contains(token),
            "case {case:?}: the refusal quotes no text of the expression, got {message:?}"
        );
    }

    // The line is counted too where the reading stops at a token: the same fault reads
    // differently when the command is written over several lines.
    let case = "a clause indented on the second line";
    let message = refused_expression(case, &emulator, "SELECT resourceId\n  FROM aws").await;
    assert_eq!(
        message, "syntax error at line 2, column 3",
        "case {case:?}: expected the position of the clause on the second line, got {message:?}"
    );
    let case = "a keyword the reading could not get past, on the third line";
    let message = refused_expression(case, &emulator, "SELECT resourceId\n\nORDER").await;
    assert_eq!(
        message, "syntax error at line 3, column 1",
        "case {case:?}: expected the position of the keyword on the third line, got {message:?}"
    );

    // A command CUT SHORT stopped at no token of itself — it needed one more than was
    // sent — so it is reported where the text ran out: one past its last character. Rows
    // differ along what was left open (a construct, a clause, a keyword), along the
    // length of what was written, and along how many lines it was written over, so the
    // line is counted too.
    for (case, expression) in [
        ("an unclosed construct", "SELECT ((( invalid"),
        ("a clause cut off after the keyword", "SELECT resourceId WHERE"),
        ("a condition left open", "SELECT resourceId WHERE ("),
        ("a condition with no value", "SELECT resourceId WHERE resourceType ="),
        ("the keyword alone", "SELECT"),
        ("the keyword and a space", "SELECT "),
        ("a second line opened and left empty", "SELECT\n"),
        ("a clause left open on the second line", "SELECT resourceId\nWHERE ("),
    ] {
        let message = refused_expression(case, &emulator, expression).await;
        let line = expression.split('\n').count();
        let column = expression
            .rsplit('\n')
            .next()
            .map_or(1, |last| last.chars().count() + 1);
        let expected = format!("syntax error at line {line}, column {column}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expression:?} to be reported where its text ran out, \
             {expected:?}, got {message:?}"
        );
    }

    // The control: a command that IS a Config query is read and served, so the rows above
    // are refused for what they carry and not because the reading refuses everything.
    for (case, expression) in [
        ("one property", "SELECT resourceId"),
        (
            "every clause of the documented form",
            "SELECT resourceType, COUNT(*) WHERE resourceType = 'AWS::EC2::Volume' GROUP BY \
             resourceType ORDER BY resourceType LIMIT 10",
        ),
        ("a command written over two lines", "SELECT resourceId\nWHERE resourceId = 'vol-1'"),
    ] {
        let (status, parsed) = emulator
            .call("SelectResourceConfig", &json!({ "Expression": expression }))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a Config query is served, got {status} with body {parsed}"
        );
    }
}

#[tokio::test]
async fn the_same_syntax_error_position_is_reported_wherever_the_language_is_read() {
    // `SelectAggregateResourceConfig` runs the same language over the aggregated
    // inventory, and `PutStoredQuery` saves a query written in it, so one unreadable
    // command is refused the same way by all three.
    let aggregator = "cross-read";
    let emulator = emulator_with_an_aggregator(aggregator).await;
    // Eighteen characters, none of which closes the construct the command opened, so the
    // reading ran out of text at the nineteenth position.
    let expression = "SELECT ((( invalid";
    let expected = "syntax error at line 1, column 19";

    let selected = refused_expression("the select path", &emulator, expression).await;
    assert_eq!(
        selected, expected,
        "the select path reports where the reading stopped, got {selected:?}"
    );

    let (status, code, message) = refusal(
        &emulator,
        "SelectAggregateResourceConfig",
        &json!({ "ConfigurationAggregatorName": aggregator, "Expression": expression }),
    )
    .await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, "InvalidExpressionException", expected),
        "the aggregate select path reports the same position, got {status} {code} {message:?}"
    );

    let (status, code, message) = refusal(
        &emulator,
        "PutStoredQuery",
        &json!({ "StoredQuery": { "QueryName": "unreadable", "Expression": expression } }),
    )
    .await;
    assert_eq!(
        (status, message.as_str()),
        (400, expected),
        "the stored-query path reports the same position, got {status} {code} {message:?}"
    );
}

#[tokio::test]
async fn a_never_discovered_aggregate_resource_is_refused_without_quoting_the_identifier() {
    let aggregator = "org-wide";
    let emulator = emulator_with_an_aggregator(aggregator).await;
    let long_id = format!("i-{}", "a".repeat(RESOURCE_ID_MAX_LENGTH - 2));

    // Rows differ along every member of the identifier: the resource type, the id (an
    // ordinary one, a single character at the `ResourceId` length minimum, and one at its
    // 768-character maximum), the source account, and the source Region. The sentence is
    // the same for all of them, and quotes none of them.
    let cases: [(&str, Value); 6] = [
        (
            "an instance that was never recorded",
            identifier(&account(), "us-west-2", EC2_INSTANCE, "i-0123456789abcdef0"),
        ),
        (
            "another resource type",
            identifier(&account(), "us-west-2", EC2_VOLUME, "vol-0123456789abcdef0"),
        ),
        (
            "an id of one character, the length minimum",
            identifier(&account(), "us-west-2", S3_BUCKET, "b"),
        ),
        (
            "an id at the length maximum",
            identifier(&account(), "us-west-2", EC2_INSTANCE, &long_id),
        ),
        (
            "another source account",
            identifier("222222222222", "us-west-2", EC2_INSTANCE, "i-0abc"),
        ),
        (
            "another source Region",
            identifier(&account(), "eu-west-1", EC2_INSTANCE, "i-0abc"),
        ),
    ];

    for (case, sent) in cases {
        let (status, code, message) = refusal(
            &emulator,
            "GetAggregateResourceConfig",
            &json!({
                "ConfigurationAggregatorName": aggregator,
                "ResourceIdentifier": sent,
            }),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "ResourceNotDiscoveredException", NOT_DISCOVERED_TEXT),
            "case {case:?}: expected the fixed not-discovered refusal, got {status} {code} \
             {message:?}"
        );
        for member in ["ResourceId", "ResourceType", "SourceAccountId", "SourceRegion"] {
            let value = sent[member].as_str().expect("the row sends every member");
            // A value of a few characters can be a substring of ordinary words, so the
            // echo is checked on the values long enough for the check to mean anything;
            // the sentence asserted above is the whole of the message for every row,
            // which is what says the identifier is not in it.
            assert!(
                value.len() < 4 || !message.contains(value),
                "case {case:?}: the refusal quotes nothing of {member} ({value:?}), got \
                 {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_service_linked_recorder_refusal_names_the_dimension_the_request_used() {
    // A delete that resolves the recorder by SERVICE PRINCIPAL states the account and the
    // principal it looked for, naming that member — never the recorder name, which such a
    // request cannot carry. Rows differ along the principal and along whether a recorder
    // for it was created and deleted first, or never existed at all.
    for (case, principal, create_first) in [
        ("a linked service whose recorder was deleted", LINKED_PRINCIPAL, true),
        ("a linked service with no recorder at all", LINKED_PRINCIPAL, false),
        (
            "another linked service with no recorder",
            "telemetry-enablement.observabilityadmin.amazonaws.com",
            false,
        ),
    ] {
        let emulator = Emulator::new();
        if create_first {
            emulator
                .ok(
                    "PutServiceLinkedConfigurationRecorder",
                    &json!({ "ServicePrincipal": principal }),
                )
                .await;
            emulator
                .ok(
                    "DeleteServiceLinkedConfigurationRecorder",
                    &json!({ "ServicePrincipal": principal }),
                )
                .await;
        }

        let (status, code, message) = refusal(
            &emulator,
            "DeleteServiceLinkedConfigurationRecorder",
            &json!({ "ServicePrincipal": principal }),
        )
        .await;
        let expected = format!(
            "Configuration recorder (AccountId: {}, Service Principal: {principal}) cannot be \
             found",
            account()
        );
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                400,
                "NoSuchConfigurationRecorderException",
                expected.as_str()
            ),
            "case {case:?}: expected the refusal to name the service principal, got {status} \
             {code} {message:?}"
        );
        assert!(
            !message.contains("name of the configuration recorder"),
            "case {case:?}: a request that sent no recorder name is not told to verify one, got \
             {message:?}"
        );
    }

    // A ServicePrincipal that names a service Config links no recorder for is refused
    // with the service-principal variant of the recorder-not-found sentence, which quotes
    // nothing of the value. Rows differ along how the value misses: a well-formed
    // principal of no such integration, a principal of an Amazon Web Services service
    // Config links no recorder for, and a value that is no service principal at all.
    for (case, principal) in [
        ("a well-formed principal of no integration", "not-a-real-integration.amazonaws.com"),
        ("a service Config links no recorder for", "ec2.amazonaws.com"),
        ("a value that is no service principal", "not-a-principal"),
    ] {
        let emulator = Emulator::new();
        let (status, code, message) = refusal(
            &emulator,
            "PutServiceLinkedConfigurationRecorder",
            &json!({ "ServicePrincipal": principal }),
        )
        .await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "ValidationException", UNRECOGNIZED_PRINCIPAL_TEXT),
            "case {case:?}: expected the service-principal sentence, got {status} {code} \
             {message:?}"
        );
        assert!(
            !message.contains(principal),
            "case {case:?}: the refusal quotes nothing of the value, got {message:?}"
        );
        // Nothing was created for it: the listing reports no recorder at all.
        let listed = emulator.ok("ListConfigurationRecorders", &json!({})).await;
        assert_eq!(
            listed["ConfigurationRecorderSummaries"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: a refused create leaves no recorder behind, got {listed}"
        );
    }

    // The control: the linked service IS created and deleted, so the refusals above are
    // about the recorder the request named and not about the operation.
    let emulator = Emulator::new();
    emulator
        .ok(
            "PutServiceLinkedConfigurationRecorder",
            &json!({ "ServicePrincipal": LINKED_PRINCIPAL }),
        )
        .await;
    emulator
        .ok(
            "DeleteServiceLinkedConfigurationRecorder",
            &json!({ "ServicePrincipal": LINKED_PRINCIPAL }),
        )
        .await;
}

#[tokio::test]
async fn an_unregistered_resource_type_is_refused_by_the_registry_lookups_own_class() {
    // Rows differ along WHY the CloudFormation registry holds no registration for the
    // type: organizations nothing registered a type under, a name that is no registry type
    // name at all, a module name, and a registered type misspelled or spelled in another
    // case. The class is the input-validation one, because what failed is the value the
    // request carried at that member.
    let cases: [(&str, &str); 6] = [
        ("an organization nothing registered", "Sim2Real::PrecisionTest::Resource"),
        ("another unregistered organization", "Acme::Network::Switch"),
        ("a type name of one segment", "Switch"),
        ("a registry module name", "Acme::Network::Switch::MODULE"),
        ("a misspelled registered type", "AWS::EC2::Volumes"),
        ("a registered type in another case", "aws::ec2::volume"),
    ];

    for (case, resource_type) in cases {
        let emulator = recording_emulator().await;
        let (status, code, message) = refusal(
            &emulator,
            "PutResourceConfig",
            &resource_config(resource_type, "r-1", r#"{"state":"new"}"#),
        )
        .await;
        let expected = format!(
            "Cannot find registered Resource Type with name {resource_type}. Please register \
             the Resource Type with Cloudformation and try again."
        );
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                400,
                "ValidationException",
                expected.as_str()
            ),
            "case {case:?}: expected the registration precondition for {resource_type:?}, got \
             {status} with {code} and {message:?}"
        );

        // Nothing is recorded: a refused write leaves no configuration item behind, so
        // the inventory counts nothing of the type it named.
        let counted = emulator.ok("GetDiscoveredResourceCounts", &json!({})).await;
        assert_eq!(
            counted["resourceCounts"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: the refused write recorded nothing, got {counted}"
        );
    }

    // The control: a type the registry DOES publish is recorded and read back, so the
    // rows above are refused for their type and not by the write path as such.
    let emulator = recording_emulator().await;
    emulator
        .ok(
            "PutResourceConfig",
            &resource_config(EC2_VOLUME, "vol-0001", r#"{"state":"in-use"}"#),
        )
        .await;
    let history = emulator
        .ok(
            "GetResourceConfigHistory",
            &json!({ "resourceType": EC2_VOLUME, "resourceId": "vol-0001" }),
        )
        .await;
    assert_eq!(
        history["configurationItems"].as_array().map(Vec::len),
        Some(1),
        "the registered type is recorded and read back, got {history}"
    );
}

/// `ResourceType`'s length maximum, from `smithy_query(query="operation",
/// operation="PutResourceConfig")`: `ResourceTypeString`, 1..196.
const RESOURCE_TYPE_MAX_LENGTH: usize = 196;

/// Another entry of `com.amazonaws.configservice#ResourceType`, used as the control of the
/// registration rule below so the control is not a type the rows beside it name.
const KMS_KEY: &str = "AWS::KMS::Key";

/// The sentence Config refuses a `ResourceType` the CloudFormation registry holds no
/// registration for with: fixed text whose one variable part is the type the request
/// carried. Written out here rather than read from the service's own helper, so this suite
/// PINS the template instead of restating it.
fn unregistered_type_text(resource_type: &str) -> String {
    format!(
        "Cannot find registered Resource Type with name {resource_type}. Please register the \
         Resource Type with Cloudformation and try again."
    )
}

/// Require that `PutResourceConfig` refuses `body` for the registration of `resource_type`
/// as the INPUT-VALIDATION class — client 400, `ValidationException` — with that
/// registration sentence.
///
/// The report names the permission class as well, because that is the other class this leg
/// could be answered with and the two have opposite remediations: one is fixed by
/// registering the type, the other by changing a role.
async fn refused_registration(case: &str, emulator: &Emulator, resource_type: &str, body: &Value) {
    let (status, code, message) = refusal(emulator, "PutResourceConfig", body).await;
    let expected = unregistered_type_text(resource_type);
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, "ValidationException", expected.as_str()),
        "case {case:?}: expected the registration precondition for {resource_type:?} as the \
         input-validation class rather than an InsufficientPermissionsException, got {status} \
         with {code} and {message:?}"
    );
}

#[tokio::test]
async fn the_registration_refusal_is_keyed_on_the_resource_type_alone_and_never_clears() {
    let emulator = recording_emulator().await;

    // Rows differ along what the rule generalises over in the NAME the registry holds no
    // registration for: its length — one character, the member's length minimum, and 196,
    // its maximum — its script, its organization segment (including every name the
    // operation's own doc reserves: "You cannot use the organization names 'amzn',
    // 'amazon', 'alexa', 'custom' with custom resource types"), and the separator between
    // its segments.
    let longest = format!(
        "Acme::Vendor::{}",
        "W".repeat(RESOURCE_TYPE_MAX_LENGTH - "Acme::Vendor::".len())
    );
    let cases: [(&str, &str); 9] = [
        ("a type name of one character, the length minimum", "x"),
        ("a type name at the 196-character length maximum", longest.as_str()),
        (
            "a non-ASCII organization and type name",
            "Acmé::Réseau::Commutateur",
        ),
        ("a reserved organization name", "amzn::Widget::Resource"),
        (
            "a second reserved organization name",
            "amazon::Widget::Resource",
        ),
        (
            "a third reserved organization name",
            "alexa::Widget::Resource",
        ),
        (
            "a fourth reserved organization name",
            "custom::Widget::Resource",
        ),
        (
            "a published type name with a trailing space",
            "AWS::S3::Bucket ",
        ),
        (
            "a published type name with single-colon separators",
            "AWS:S3:Bucket",
        ),
    ];

    for (case, resource_type) in cases {
        let body = resource_config(resource_type, "r-1", r#"{"state":"new"}"#);
        refused_registration(case, &emulator, resource_type, &body).await;
        // Nothing a caller RETRIES registers a type with CloudFormation, so this refusal
        // never clears: the call answers the same class and the same sentence however
        // often it is made.
        refused_registration(
            &format!("{case}, called a second time"),
            &emulator,
            resource_type,
            &body,
        )
        .await;
    }

    // The refusal is keyed on the ResourceType alone, so the same unregistered type draws
    // it whatever ELSE the request carries: rows differ along the resource name, the tags,
    // the shape of the configuration document, the resource id (one at its 768-character
    // length maximum) and the schema version.
    let unregistered = "Acme::Network::Router";
    let longest_id = format!("r-{}", "9".repeat(RESOURCE_ID_MAX_LENGTH - 2));
    let bodies: [(&str, Value); 5] = [
        (
            "the required members only",
            json!({
                "ResourceType": unregistered,
                "SchemaVersionId": "1-0-0",
                "ResourceId": "r-2",
                "Configuration": "{}",
            }),
        ),
        (
            "a resource name and tags beside them",
            json!({
                "ResourceType": unregistered,
                "SchemaVersionId": "1-0-0",
                "ResourceId": "r-3",
                "ResourceName": "router-1",
                "Configuration": "{}",
                "Tags": { "env": "prod", "team": "network" },
            }),
        ),
        (
            "a nested configuration document",
            json!({
                "ResourceType": unregistered,
                "SchemaVersionId": "1-0-0",
                "ResourceId": "r-4",
                "Configuration": r#"{"ports":[{"id":1},{"id":2}],"vlan":{"id":7}}"#,
            }),
        ),
        (
            "a resource id at its length maximum",
            json!({
                "ResourceType": unregistered,
                "SchemaVersionId": "1-0-0",
                "ResourceId": longest_id,
                "Configuration": "{}",
            }),
        ),
        (
            "another schema version",
            json!({
                "ResourceType": unregistered,
                "SchemaVersionId": "00000000-0000-0000-0000-000000000009",
                "ResourceId": "r-5",
                "Configuration": "{}",
            }),
        ),
    ];
    for (case, body) in bodies {
        refused_registration(case, &emulator, unregistered, &body).await;
    }

    // Not one of the rows recorded anything: the inventory counts no resource at all.
    let counted = emulator.ok("GetDiscoveredResourceCounts", &json!({})).await;
    assert_eq!(
        counted["resourceCounts"].as_array().map(Vec::len),
        Some(0),
        "a refused write records nothing, got {counted}"
    );

    // The recorder precondition is judged AHEAD of the registry lookup: with no recorder
    // running, the same unregistered type is refused for the recorder, so a caller is told
    // the precondition it has to meet before the value it has to fix.
    let idle = Emulator::new();
    let (status, code, message) = refusal(
        &idle,
        "PutResourceConfig",
        &resource_config(unregistered, "r-6", "{}"),
    )
    .await;
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (
            400,
            "NoRunningConfigurationRecorderException",
            "You must create and start a configuration recorder before you call the \
             PutResourceConfig API."
        ),
        "expected the recorder precondition ahead of the registry lookup, got {status} with \
         {code} and {message:?}"
    );

    // The control: a published type none of the rows named is recorded and read back, so
    // they are refused for the names they carried and not by this write path as such.
    emulator
        .ok(
            "PutResourceConfig",
            &resource_config(KMS_KEY, "key-0001", r#"{"enabled":true}"#),
        )
        .await;
    let history = emulator
        .ok(
            "GetResourceConfigHistory",
            &json!({ "resourceType": KMS_KEY, "resourceId": "key-0001" }),
        )
        .await;
    assert_eq!(
        history["configurationItems"].as_array().map(Vec::len),
        Some(1),
        "the published type is recorded and read back, got {history}"
    );
}

/// The position Config reports for an expression cut short: one past its last CHARACTER —
/// the line is how many lines the text was written over, and the column counts the
/// characters of its last line, so the position names a character the caller wrote rather
/// than a byte of its encoding.
fn ran_out_message(expression: &str) -> String {
    let last_line = expression.split('\n').next_back().unwrap_or(expression);
    let line = expression.split('\n').count();
    let column = last_line.chars().count() + 1;
    format!("syntax error at line {line}, column {column}")
}

/// The line and column a syntax-error message reports, read off the fixed text it is
/// worded with, or `None` for a refusal of the other kind — a command that IS read and
/// breaks a rule of the query language carries that rule's own sentence instead.
fn reported_position(message: &str) -> Option<(usize, usize)> {
    let located = message.strip_prefix("syntax error at line ")?;
    let (line, column) = located.split_once(", column ")?;
    Some((line.parse().ok()?, column.parse().ok()?))
}

#[tokio::test]
async fn a_query_cut_short_is_reported_where_its_text_ran_out_in_characters() {
    let emulator = Emulator::new();

    // A command cut short stopped at no token of itself — it needed one more than was
    // sent — so it is reported one past its last character. Rows differ along what was
    // left open, along the length of what was written, along the script of its text
    // (non-ASCII letters, each one character of several bytes, so a position counted in
    // bytes would differ), and along how many lines it was written over.
    for (case, expression) in [
        ("one bracket left open", "SELECT ("),
        ("three brackets left open", "SELECT ((( invalid"),
        ("five brackets left open before a property", "SELECT ((((( resourceId"),
        ("a non-ASCII property behind an open bracket", "SELECT ((( résumé"),
        (
            "a condition on a non-ASCII tag with no value",
            "SELECT resourceId WHERE tags.ключ =",
        ),
        (
            "a bracket left open after a non-ASCII literal",
            "SELECT resourceId WHERE tags.ключ = 'значение' AND (",
        ),
        (
            "a condition left open on the third line",
            "SELECT resourceId\nWHERE resourceType = 'AWS::S3::Bucket'\n  AND (",
        ),
    ] {
        let message = refused_expression(case, &emulator, expression).await;
        let expected = ran_out_message(expression);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expression:?} to be reported where its text ran out, \
             {expected:?}, got {message:?}"
        );
    }
}

#[tokio::test]
async fn every_reported_position_is_a_position_of_the_submitted_expression() {
    let emulator = Emulator::new();

    // Every PREFIX of one well-formed Config query, cut at a character boundary: a set of
    // expressions this suite did not write by hand, each of which the reading either
    // serves or stops somewhere inside. Wherever it stops, the position it reports is one
    // the submitted text HAS — a line the text was written over, at a character of that
    // line or the one just past its end — so a tool highlighting that position never
    // points outside what the caller sent.
    let query = "SELECT resourceId, tags.ключ\nWHERE resourceType = 'AWS::S3::Bucket'\n\
                 ORDER BY resourceId\nLIMIT 5";
    let cuts = query
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(query.len()))
        .skip(1);
    let mut positions = 0usize;

    for end in cuts {
        let prefix = &query[..end];
        let case = format!("the first {} character(s)", prefix.chars().count());
        let (status, code, message) = refusal(
            &emulator,
            "SelectResourceConfig",
            &json!({ "Expression": prefix }),
        )
        .await;
        if status == 200 {
            continue;
        }
        assert_eq!(
            (status, code.as_str()),
            (400, "InvalidExpressionException"),
            "case {case:?}: a command the reading will not serve is refused as a malformed \
             expression, got {status} {code} with message {message:?}"
        );
        let Some((line, column)) = reported_position(&message) else {
            // The other refusal this code stands for: a command that is read and breaks a
            // rule of the query language, worded with that rule's own sentence.
            assert!(
                !message.is_empty(),
                "case {case:?}: a refusal of {prefix:?} carries a message, got none"
            );
            continue;
        };
        positions += 1;
        let lines: Vec<&str> = prefix.split('\n').collect();
        assert!(
            (1..=lines.len()).contains(&line),
            "case {case:?}: expected a line of {prefix:?} (1..={}), got {line} from {message:?}",
            lines.len()
        );
        let characters = lines[line - 1].chars().count() + 1;
        assert!(
            (1..=characters).contains(&column),
            "case {case:?}: expected a column of line {line} of {prefix:?} (1..={characters}), \
             got {column} from {message:?}"
        );
    }

    assert!(
        positions > 10,
        "expected the prefixes of {query:?} to report many positions, got {positions}"
    );
}
