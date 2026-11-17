// SPDX-License-Identifier: CC0-1.0

//! How this registry states a refused PARAMETER, end to end over the wire.
//!
//! Every refusal below already carries the right HTTP status (400); what these suites pin
//! is the error class and the clause, which ECR builds from the request and the model to
//! one template — `Invalid parameter at '<parameter>' failed to satisfy constraint:
//! '<constraint>'`. Six rules, one suite each:
//!
//! 1. a value this registry does not recognise, where the modelled shape declares NO
//!    enum, fails a stable constraint phrase about the parameter — never a value set the
//!    model does not declare, and never this emulator's own catalogue of what it serves;
//! 2. which validator caught the value decides how both halves are spelled: a parameter
//!    ECR validates itself (one the request carries, and an ARN it must RESOLVE wherever
//!    the request carries one) is named by its API label with the constraint in ECR's own
//!    words, while a value the model-driven walk caught — a member of a NESTED structure,
//!    or a parameter whose value the registry only RECORDS — keeps that walk's path and
//!    the `@pattern` as the model declares it;
//! 3. which operations police `RepositoryName`'s `@pattern` is ECR's own per-OPERATION
//!    fact: a listing LOOKS the name up, so a name no repository carries — a registry-
//!    qualified image reference passed where a name belongs included — is the modelled
//!    not-found error there and a parameter failure on an operation that does police the
//!    expression, while the modelled length bounds are stated as a parameter failure by
//!    both;
//! 4. a value outside an enumerated type states that type's whole value set in the order
//!    the values' own hash table is walked, not the order the model declares;
//! 5. whether the expression ECR publishes for a value class is DELIMITED inside the
//!    clause belongs to the class: the account-id class is stated bare, the
//!    repository-name classes delimited;
//! 6. a list ECR reads no value out of is refused at the LIST member with the element's
//!    requirement nested in the collection form, while a list whose values it resolves is
//!    refused one value at a time under the singular of its name.
//!
//! Each suite is a TABLE whose rows differ along what its rule generalises over — the
//! shape of the unrecognised value and the operation asked, which constraint failed and
//! where in the request the member sits — and the recorded instance each cause was filed
//! against is ONE row, marked as such. Every case goes through the router, so each
//! exercises routing, the boundary validator, the handler, the persisted state and
//! serialization.

use serde_json::{json, Value};
use service_com_amazonaws_ecr_amazonec2containerregistry_v20150921::state::ACCOUNT_SETTINGS;

use crate::wire::{call, emulator, REGISTRY_ID};

/// The error code ECR states a refused parameter with: the client-input error 57 of this
/// model's 58 operations declare.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The whole clause, written out literally — the point of these suites is that the
/// service builds exactly this from the request, so computing it from the implementation
/// would assert nothing.
fn clause(parameter: &str, constraint: &str) -> String {
    format!("Invalid parameter at '{parameter}' failed to satisfy constraint: '{constraint}'")
}

/// The constraint an account setting name this registry does not serve fails.
///
/// `GetAccountSetting$name` and `PutAccountSetting$name` target `AccountSettingName`,
/// which `smithy_query(query="shape", shape="AccountSettingName")` gives as a string of
/// length 1..64 with NO enum trait — so the constraint is this stable phrase, which names
/// no value set.
const UNKNOWN_SETTING_CONSTRAINT: &str = "Invalid account setting name provided";

/// A name this registry does not serve is refused by both account-setting operations with
/// the stable constraint phrase, and the refusal publishes neither a value set nor the
/// registry's own catalogue of settings.
///
/// The rows differ along the shape of the unrecognised name — a name of no setting, a
/// real one differently cased, a prefix and a suffixed form of a real one, and the
/// shortest and longest the model's `@length(1..64)` admits — crossed with both
/// operations that take the parameter. The recorded instance is the first row.
#[tokio::test]
async fn an_unrecognised_account_setting_name_states_a_constraint_the_model_declares() {
    let longest = "A".repeat(64);
    let names: Vec<(&str, String)> = vec![
        // The recorded instance: a name of no setting at all.
        ("a name of no setting", "NOT_A_SETTING".to_string()),
        (
            "a real name in lower case",
            "basic_scan_type_version".to_string(),
        ),
        ("a prefix of a real name", "BASIC_SCAN_TYPE".to_string()),
        ("a real name with a suffix", "BLOB_MOUNTING_V2".to_string()),
        ("the shortest name the model admits", "A".to_string()),
        ("the longest name the model admits", longest),
    ];

    let router = emulator();
    let expected = clause("name", UNKNOWN_SETTING_CONSTRAINT);
    for (case, name) in names {
        for operation in ["GetAccountSetting", "PutAccountSetting"] {
            let case = format!("{case} on {operation}");
            let answer = call(
                &router,
                operation,
                &json!({"name": name, "value": "ENABLED"}),
            )
            .await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                INVALID_PARAMETER,
                "case {case:?}: expected {INVALID_PARAMETER}, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.message(),
                expected,
                "case {case:?}: expected the clause {expected:?}, got {:?}",
                answer.message()
            );
            // Two things the model does not declare, so the refusal must state neither:
            // an enum-membership failure, and the set of settings this registry serves.
            assert!(
                !answer.message().contains("enum value set"),
                "case {case:?}: the clause must assert no enum constraint, got {:?}",
                answer.message()
            );
            for spec in ACCOUNT_SETTINGS {
                assert!(
                    !answer.message().contains(spec.name),
                    "case {case:?}: the clause must not publish the setting {:?}, got {:?}",
                    spec.name,
                    answer.message()
                );
            }
        }
    }
}

/// `RepositoryName`'s `@pattern` as ECR'S OWN validation publishes it: without the `^` /
/// `$` the model anchors it with, and with the namespace separator bare, because `/` is
/// no regular-expression metacharacter and `\/` matches exactly what `/` does.
///
/// `smithy_query(query="shape", shape="RepositoryName")` gives
/// `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$`.
const REPOSITORY_NAME_EXPRESSION: &str =
    r"[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*";

/// `PullThroughCacheRuleRepositoryPrefix`'s `@pattern` as ECR'S OWN validation publishes
/// it, by the same two rules as [`REPOSITORY_NAME_EXPRESSION`].
///
/// `smithy_query(query="shape", shape="PullThroughCacheRuleRepositoryPrefix")` gives
/// `^([a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*\/?|ROOT)$`.
const PREFIX_EXPRESSION: &str =
    r"([a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*/?|ROOT)";

/// `SigningRepositoryFilterValue`'s `@pattern` as THE MODEL declares it — anchors
/// included, because the model-driven walk states the expression it was given.
///
/// `smithy_query(query="shape", shape="SigningRepositoryFilter")`, member `filter`.
const SIGNING_FILTER_PATTERN: &str =
    "^(?:[a-z0-9*]+(?:[._-][a-z0-9*]+)*/)*[a-z0-9*]+(?:[._-][a-z0-9*]+)*$";

/// `PrincipalArn`'s `@pattern` as THE MODEL declares it, anchors included
/// (`smithy_query(query="shape", shape="PrincipalArn")`): the registry RECORDS a principal
/// ARN as the key of an exclusion rather than resolving it to a resource, so nothing of
/// ECR's own checks its form and the model's constraint is what refuses it.
const PRINCIPAL_ARN_PATTERN: &str =
    r"^arn:aws(-[a-z]+)*:iam::[0-9]{12}:(role|user)/[\w+=,.@-]+(/[\w+=,.@-]+)*$";

/// `ImageTagMutabilityExclusionFilterValue`'s `@pattern`, as the model declares it
/// (`smithy_query(query="shape", shape="ImageTagMutabilityExclusionFilter")`, member
/// `filter`) — the same nested-member rule reached through another operation.
const EXCLUSION_FILTER_PATTERN: &str = "^[0-9a-zA-Z._*-]{1,128}$";

/// The path the validating walk descends to the `filter` of the `rule`-th signing rule's
/// `filter`-th repository filter, with the 1-based element steps it spells
/// (`vendor/smithy-schema-server/src/constraints.rs`, "Path fidelity").
fn signing_filter_path(rule: usize, filter: usize, member: &str) -> String {
    format!("signingConfiguration.rules.{rule}.member.repositoryFilters.{filter}.member.{member}")
}

/// One signing rule: `arn` as its signing profile, plus `filters` when it carries any.
fn signing_rule(arn: &str, filters: Option<Vec<Value>>) -> Value {
    match filters {
        Some(filters) => json!({"signingProfileArn": arn, "repositoryFilters": filters}),
        None => json!({"signingProfileArn": arn}),
    }
}

/// One well-formed repository filter.
fn wildcard_filter(filter: &str) -> Value {
    json!({"filter": filter, "filterType": "WILDCARD_MATCH"})
}

/// A refused parameter is stated by whichever validator caught it: ECR's own validation
/// names the API parameter and states the constraint in ECR's words, and the model-driven
/// walk names the path it descended and states the model's own `@pattern`.
///
/// The rows differ along what the rule generalises over: where the member sits (a
/// top-level member, an element of a top-level list, a member of a structure nested in a
/// list, and the SECOND such element so a position cannot be mistaken for a name), which
/// constraint failed (`@pattern` on a plain string, on an ARN ECR resolves and on one it
/// only records, `@enum`, `@length`, `@required`), and which operation was called. The
/// three recorded instances are marked.
#[tokio::test]
async fn a_refused_parameter_is_stated_by_the_validator_that_caught_it() {
    let well_formed_profile =
        format!("arn:aws:signer:eu-west-1:{REGISTRY_ID}:/signing-profiles/WellFormed");
    let long_filter = "a".repeat(257);
    // (case, operation, request, the parameter the clause names, the constraint it states)
    let rows: Vec<(&str, &str, Value, String, String)> = vec![
        (
            "a top-level member outside its pattern",
            "CreateRepository",
            json!({"repositoryName": "Invalid_Name"}),
            "repositoryName".to_string(),
            format!("must satisfy regular expression '{REPOSITORY_NAME_EXPRESSION}'"),
        ),
        (
            // The recorded instance: a top-level ARN whose value the registry only
            // records, so the model's own constraint is what states it.
            "a top-level ARN the registry records rather than resolves",
            "RegisterPullTimeUpdateExclusion",
            json!({"principalArn": "not-an-arn"}),
            "principalArn".to_string(),
            format!("Member must satisfy regular expression pattern: {PRINCIPAL_ARN_PATTERN}"),
        ),
        (
            "the same member on the operation that removes an exclusion",
            "DeregisterPullTimeUpdateExclusion",
            json!({"principalArn": "arn:aws:iam::000000000000:group/admins"}),
            "principalArn".to_string(),
            format!("Member must satisfy regular expression pattern: {PRINCIPAL_ARN_PATTERN}"),
        ),
        (
            "an element of a top-level list, which is one value of that parameter",
            "DescribeRepositories",
            json!({"repositoryNames": ["well-formed", "ALSO-Upper"]}),
            "repositoryName".to_string(),
            format!("must satisfy regular expression '{REPOSITORY_NAME_EXPRESSION}'"),
        ),
        (
            // The recorded instance: ECR checks the form of an ARN it must RESOLVE in its
            // own code, wherever the request carries it, so the clause names the API
            // parameter and not the walk's path to it.
            "an ARN ECR resolves, inside a list of structures",
            "PutSigningConfiguration",
            json!({"signingConfiguration": {
                "rules": [signing_rule("not-a-valid-signer-arn", None)],
            }}),
            "signingProfileArn".to_string(),
            "The signing profile ARN 'not-a-valid-signer-arn' must be well formed".to_string(),
        ),
        (
            "an ARN inside the SECOND element of that list",
            "PutSigningConfiguration",
            json!({"signingConfiguration": {
                "rules": [
                    signing_rule(&well_formed_profile, None),
                    signing_rule("arn:aws:iam::000000000000:role/NotASigningProfile", None),
                ],
            }}),
            "signingProfileArn".to_string(),
            "The signing profile ARN 'arn:aws:iam::000000000000:role/NotASigningProfile' \
             must be well formed"
                .to_string(),
        ),
        (
            // The recorded instance: a member of a structure NESTED in the request, which
            // nothing of ECR's own names, so the walk's path and wording stand.
            "a nested structure's member outside its pattern",
            "PutSigningConfiguration",
            json!({"signingConfiguration": {
                "rules": [signing_rule(
                    &well_formed_profile,
                    Some(vec![wildcard_filter("Team/App")]),
                )],
            }}),
            signing_filter_path(1, 1, "filter"),
            format!("Member must satisfy regular expression pattern: {SIGNING_FILTER_PATTERN}"),
        ),
        (
            "the same member in the second rule's second filter",
            "PutSigningConfiguration",
            json!({"signingConfiguration": {
                "rules": [
                    signing_rule(&well_formed_profile, Some(vec![wildcard_filter("team/app")])),
                    signing_rule(
                        &well_formed_profile,
                        Some(vec![wildcard_filter("team/app"), wildcard_filter("Team/App")]),
                    ),
                ],
            }}),
            signing_filter_path(2, 2, "filter"),
            format!("Member must satisfy regular expression pattern: {SIGNING_FILTER_PATTERN}"),
        ),
        (
            "a nested structure's member outside its enum",
            "PutSigningConfiguration",
            json!({"signingConfiguration": {
                "rules": [signing_rule(
                    &well_formed_profile,
                    Some(vec![json!({"filter": "team/app", "filterType": "EXACT_MATCH"})]),
                )],
            }}),
            signing_filter_path(1, 1, "filterType"),
            "Member must satisfy enum value set: [WILDCARD_MATCH]".to_string(),
        ),
        (
            "a nested structure's member past its maximum length",
            "PutSigningConfiguration",
            json!({"signingConfiguration": {
                "rules": [signing_rule(
                    &well_formed_profile,
                    Some(vec![wildcard_filter(&long_filter)]),
                )],
            }}),
            signing_filter_path(1, 1, "filter"),
            "Member must have length less than or equal to 256".to_string(),
        ),
        (
            "a nested structure's required member left out",
            "PutSigningConfiguration",
            json!({"signingConfiguration": {
                "rules": [signing_rule(
                    &well_formed_profile,
                    Some(vec![json!({"filterType": "WILDCARD_MATCH"})]),
                )],
            }}),
            signing_filter_path(1, 1, "filter"),
            "Member must not be null".to_string(),
        ),
        (
            "a nested structure's member in another operation",
            "CreateRepository",
            json!({
                "repositoryName": "well-formed",
                "imageTagMutability": "MUTABLE_WITH_EXCLUSION",
                "imageTagMutabilityExclusionFilters": [
                    {"filterType": "WILDCARD", "filter": "team/app"},
                ],
            }),
            "imageTagMutabilityExclusionFilters.1.member.filter".to_string(),
            format!("Member must satisfy regular expression pattern: {EXCLUSION_FILTER_PATTERN}"),
        ),
        (
            // The same element rule reached through another operation and another list of
            // names, so the singular is the rule rather than one operation's wording.
            "an element of another operation's name list",
            "BatchGetRepositoryScanningConfiguration",
            json!({"repositoryNames": ["well-formed", "Also-Upper"]}),
            "repositoryName".to_string(),
            format!("must satisfy regular expression '{REPOSITORY_NAME_EXPRESSION}'"),
        ),
        (
            // A collection whose name is the `-es` plural of its parameter, so undoing the
            // plural is more than dropping an `s`.
            "an element of a list whose name takes the -es plural",
            "DescribePullThroughCacheRules",
            json!({"ecrRepositoryPrefixes": ["Not A Prefix"]}),
            "ecrRepositoryPrefix".to_string(),
            format!("must satisfy regular expression '{PREFIX_EXPRESSION}'"),
        ),
        (
            "an element of the shortest such list name",
            "DescribeRepositoryCreationTemplates",
            json!({"prefixes": ["Not A Prefix"]}),
            "prefix".to_string(),
            format!("must satisfy regular expression '{PREFIX_EXPRESSION}'"),
        ),
    ];

    let router = emulator();
    for (case, operation, request, parameter, constraint) in rows {
        let answer = call(&router, operation, &request).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400 from {operation}, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.error_code(),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER} from {operation}, got {:?}",
            answer.body
        );
        let expected = clause(&parameter, &constraint);
        assert_eq!(
            answer.message(),
            expected,
            "case {case:?}: expected the clause {expected:?}, got {:?}",
            answer.message()
        );
    }
}

/// Assert `operation` refuses `request` with HTTP 400, `InvalidParameterException` and
/// exactly the clause `parameter` / `constraint` states.
async fn assert_clause(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    operation: &str,
    request: &Value,
    parameter: &str,
    constraint: &str,
) {
    let answer = call(router, operation, request).await;
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected HTTP 400 from {operation}, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.error_code(),
        INVALID_PARAMETER,
        "case {case:?}: expected {INVALID_PARAMETER} from {operation}, got {:?}",
        answer.body
    );
    let expected = clause(parameter, constraint);
    assert_eq!(
        answer.message(),
        expected,
        "case {case:?}: expected the clause {expected:?}, got {:?}",
        answer.message()
    );
}

/// The sentence ECR states a repository the registry does not hold with, written out
/// literally for the same reason every other sentence in these suites is.
fn repository_not_found(name: &str) -> String {
    format!("The repository with name '{name}' does not exist in the registry with id '{REGISTRY_ID}'")
}

/// Which operations police `RepositoryName`'s `@pattern` is ECR's own per-OPERATION fact,
/// and `ListImages` is one that does NOT: every name inside the modelled length bounds
/// reaches its lookup, so a name this registry holds no repository for is the modelled
/// `RepositoryNotFoundException` naming the name as the request spelled it.
///
/// `RepositoryName` is modelled `@length(min: 2, max: 256)` with the `@pattern`
/// `^[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*(\/[a-z0-9]+((\.|_|__|-+)[a-z0-9]+)*)*$`
/// (`smithy_query(query="shape", shape="RepositoryName")`). The recorded instance is one
/// call of this operation naming a registry-qualified image reference — an image URI passed
/// where a repository name belongs, whose authority carries a port the expression does not
/// admit — answered as a repository that does not exist. The operations that DO police the
/// expression are the other half of the rule, pinned by the suite below.
///
/// The rows differ along what the rule generalises over: the shape of the name — a plain
/// name, a namespaced one, registry-qualified references in two Regions and two accounts,
/// an authority carrying a port (the recorded shape), a reference carrying a tag, an
/// uppercase letter, a space, and both ends of the admitted length — plus the two length
/// bounds, which are the one thing this operation still refuses the name against.
#[tokio::test]
async fn every_length_legal_name_a_listing_names_is_looked_up() {
    /// What the answer to one name is: the lookup's own not-found, or the parameter clause
    /// stating the length bound the name failed.
    enum Answer {
        LookedUp,
        Refused(&'static str),
    }

    let too_long = "a".repeat(257);
    let longest = "a".repeat(256);
    let longest_case = format!("{} characters, the longest the model admits", longest.len());
    // (case, the repositoryName sent, what the answer is)
    let names: Vec<(&str, &str, Answer)> = vec![
        ("a plain name no repository carries", "absent-repository", Answer::LookedUp),
        ("a namespaced name", "team/absent-repository", Answer::LookedUp),
        (
            "a registry-qualified image reference",
            "000000000000.dkr.ecr.us-west-2.amazonaws.com/absent-repository",
            Answer::LookedUp,
        ),
        (
            "another account and Region's reference",
            "111111111111.dkr.ecr.eu-west-1.amazonaws.com/team/absent-repository",
            Answer::LookedUp,
        ),
        (
            // The recorded instance's shape.
            "an authority carrying a port",
            "127.0.0.1:4566/absent",
            Answer::LookedUp,
        ),
        ("a reference carrying a tag", "absent-repository:latest", Answer::LookedUp),
        ("an uppercase letter", "Absent-Repository", Answer::LookedUp),
        ("a space", "absent repository", Answer::LookedUp),
        ("the shortest name the model admits", "ab", Answer::LookedUp),
        (&longest_case, &longest, Answer::LookedUp),
        (
            "one character — one below the modelled minimum",
            "a",
            Answer::Refused("Member must have length greater than or equal to 2"),
        ),
        (
            "257 characters — one above the modelled maximum",
            &too_long,
            Answer::Refused("Member must have length less than or equal to 256"),
        ),
    ];

    let router = emulator();
    for (shape, name, expected) in &names {
        let case = format!("{shape} on ListImages");
        let answer = call(&router, "ListImages", &json!({"repositoryName": name})).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} with {:?}",
            answer.status, answer.body
        );
        let (code, expected) = match expected {
            Answer::LookedUp => ("RepositoryNotFoundException", repository_not_found(name)),
            Answer::Refused(constraint) => {
                (INVALID_PARAMETER, clause("repositoryName", constraint))
            }
        };
        assert_eq!(
            answer.error_code(),
            code,
            "case {case:?}: expected {code}, got {:?}",
            answer.body
        );
        assert_eq!(
            answer.message(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answer.message()
        );
    }

    // The rest of the member's contract, which taking the expression off the boundary
    // leaves untouched: the name is still `@required`, so a call naming no repository at
    // all is refused before any lookup, and a name the registry DOES hold is served.
    assert_clause(
        &router,
        "no repositoryName at all on ListImages",
        "ListImages",
        &json!({}),
        "repositoryName",
        "Member must not be null",
    )
    .await;
    let held = "held-repository";
    let created = call(&router, "CreateRepository", &json!({"repositoryName": held})).await;
    assert_eq!(
        created.status, 200,
        "the repository a served listing needs must be created, got {:?}",
        created.body
    );
    let listed = call(&router, "ListImages", &json!({"repositoryName": held})).await;
    assert_eq!(
        listed.status, 200,
        "a name this registry holds a repository for must be listed, got {} with {:?}",
        listed.status, listed.body
    );
}

/// A `repositoryName` outside the modelled expression IS a parameter failure on an
/// operation whose validator polices it, and a name the expression admits is looked up
/// there: the other half of the per-operation rule above.
///
/// The recorded pair is one client's two calls: `DescribeImages` naming a reference that
/// carries a tag, answered with the clause stating the expression ECR publishes, where
/// `ListImages` naming a registry-qualified reference was answered with the not-found error.
///
/// The rows differ along what the rule generalises over: the shape of the name (a plain
/// name, a namespaced one, a registry-qualified reference in two Regions and two accounts —
/// every one of which the expression admits — against an authority carrying a port, a
/// reference carrying a tag, and an uppercase letter, which it does not) crossed with the
/// operations that take the parameter, spanning a metadata read, a policy read, a push
/// preparation and a delete.
#[tokio::test]
async fn a_name_outside_the_expression_is_a_parameter_where_the_expression_is_policed() {
    // (case, the repositoryName sent, whether the expression admits it)
    let names: Vec<(&str, &str, bool)> = vec![
        ("a plain name no repository carries", "absent-repository", true),
        ("a namespaced name", "team/absent-repository", true),
        (
            "a registry-qualified image reference",
            "000000000000.dkr.ecr.us-west-2.amazonaws.com/absent-repository",
            true,
        ),
        (
            "another account and Region's reference",
            "111111111111.dkr.ecr.eu-west-1.amazonaws.com/team/absent-repository",
            true,
        ),
        ("an authority carrying a port", "127.0.0.1:4566/absent", false),
        // The recorded instance's shape, on the recorded operation.
        ("a reference carrying a tag", "absent-repository:latest", false),
        ("an uppercase letter", "Absent-Repository", false),
    ];
    let operations = [
        "DescribeImages",
        "GetRepositoryPolicy",
        "InitiateLayerUpload",
        "DeleteRepository",
    ];

    let router = emulator();
    let malformed = clause(
        "repositoryName",
        &format!("must satisfy regular expression '{REPOSITORY_NAME_EXPRESSION}'"),
    );
    for (shape, name, admitted) in &names {
        for operation in operations {
            let case = format!("{shape} on {operation}");
            let answer = call(&router, operation, &json!({"repositoryName": name})).await;
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected HTTP 400, got {} with {:?}",
                answer.status, answer.body
            );
            let (code, expected) = if *admitted {
                ("RepositoryNotFoundException", repository_not_found(name))
            } else {
                (INVALID_PARAMETER, malformed.clone())
            };
            assert_eq!(
                answer.error_code(),
                code,
                "case {case:?}: expected {code}, got {:?}",
                answer.body
            );
            assert_eq!(
                answer.message(),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                answer.message()
            );
        }
    }
}

/// `TagStatus` as the clause states it. The type declares `[TAGGED, UNTAGGED, ANY]`
/// (`smithy_query(query="shape", shape="TagStatus")`), and the stated set is NOT that
/// order: the admissible values are held in a hash table keyed on each value's own text
/// and the table is what is enumerated, so `UNTAGGED` is read from an earlier bucket than
/// the two that share one, and of those two the later-declared `ANY` comes first.
const TAG_STATUS_SET: &str = "[UNTAGGED, ANY, TAGGED]";

/// The same for `ImageStatusFilter`, declared `[ACTIVE, ARCHIVED, ACTIVATING, ANY]`.
const IMAGE_STATUS_SET: &str = "[ARCHIVED, ACTIVE, ANY, ACTIVATING]";

/// The same for `ImageTagMutability`, declared
/// `[MUTABLE, IMMUTABLE, IMMUTABLE_WITH_EXCLUSION, MUTABLE_WITH_EXCLUSION]`.
const IMAGE_TAG_MUTABILITY_SET: &str =
    "[IMMUTABLE, MUTABLE, MUTABLE_WITH_EXCLUSION, IMMUTABLE_WITH_EXCLUSION]";

/// The same for `TargetStorageClass`, declared `[STANDARD, ARCHIVE]`.
const TARGET_STORAGE_CLASS_SET: &str = "[ARCHIVE, STANDARD]";

/// The same for `EncryptionType`, declared `[AES256, KMS, KMS_DSSE]`.
const ENCRYPTION_TYPE_SET: &str = "[AES256, KMS_DSSE, KMS]";

/// A value outside an enumerated type states the type's WHOLE value set, in the order the
/// values' own hash table is walked — not the order the model declares them in.
///
/// The rows differ along what the rule generalises over: which enumerated type the member
/// targets (five of them, of two, three and four values, one of which has two values
/// sharing a bucket), where the member sits (a top-level member, a member of a nested
/// structure), and which operation carries it — the order belongs to the TYPE, so the same
/// type states the same set wherever it is carried. The recorded instance is
/// `filter.tagStatus` on `ListImages`.
#[tokio::test]
async fn a_value_outside_an_enumerated_type_states_the_set_in_the_stated_order() {
    // (case, operation, request, the parameter the clause names, the value set it states)
    let rows: Vec<(&str, &str, Value, &str, &str)> = vec![
        (
            // The recorded instance.
            "a nested filter's tag status",
            "ListImages",
            json!({"repositoryName": "well-formed", "filter": {"tagStatus": "NOT_A_STATUS"}}),
            "filter.tagStatus",
            TAG_STATUS_SET,
        ),
        (
            // The same type through another operation, which is what makes the order the
            // type's rather than one operation's.
            "the same type on another operation",
            "DescribeImages",
            json!({"repositoryName": "well-formed", "filter": {"tagStatus": "SOMETIMES"}}),
            "filter.tagStatus",
            TAG_STATUS_SET,
        ),
        (
            "a four-value type in the same filter",
            "ListImages",
            json!({"repositoryName": "well-formed", "filter": {"imageStatus": "FROZEN"}}),
            "filter.imageStatus",
            IMAGE_STATUS_SET,
        ),
        (
            "a top-level member's four-value type",
            "CreateRepository",
            json!({"repositoryName": "well-formed", "imageTagMutability": "SOMETIMES"}),
            "imageTagMutability",
            IMAGE_TAG_MUTABILITY_SET,
        ),
        (
            "a top-level member's two-value type",
            "UpdateImageStorageClass",
            json!({
                "repositoryName": "well-formed",
                "imageId": {"imageTag": "v1"},
                "targetStorageClass": "GLACIER",
            }),
            "targetStorageClass",
            TARGET_STORAGE_CLASS_SET,
        ),
        (
            "a nested structure's three-value type",
            "CreateRepository",
            json!({
                "repositoryName": "well-formed",
                "encryptionConfiguration": {"encryptionType": "ROT13"},
            }),
            "encryptionConfiguration.encryptionType",
            ENCRYPTION_TYPE_SET,
        ),
    ];

    let router = emulator();
    for (case, operation, request, parameter, set) in rows {
        let constraint = format!("Member must satisfy enum value set: {set}");
        assert_clause(&router, case, operation, &request, parameter, &constraint).await;
    }
}

/// `RegistryId`'s expression as ECR publishes it: `smithy_query(query="shape",
/// shape="RegistryId")` gives `^[0-9]{12}$`, and ECR states it without the anchors that
/// make the match total.
const REGISTRY_ID_EXPRESSION: &str = "[0-9]{12}";

/// The expression ECR states for the account-id class is stated BARE, with nothing around
/// it inside the clause's own quotes, while the repository-name classes' expression is
/// delimited — the difference belongs to the value class and not to the member, the
/// operation or the constraint.
///
/// The rows differ along what the rule generalises over: how the account id is malformed
/// (too few digits, too many, a letter among them, a separator, no characters at all) and
/// which operation carries the parameter, crossed against the two classes whose expression
/// IS delimited. The recorded instance is an account id on `DescribeRepositories`.
#[tokio::test]
async fn the_account_id_expression_is_stated_bare_and_a_repository_name_delimited() {
    // Every value here fails `^[0-9]{12}$` and nothing else in its request does.
    let malformed: Vec<(&str, &str)> = vec![
        ("eleven digits", "00000000000"),
        ("thirteen digits", "0000000000000"),
        ("twelve characters, one a letter", "00000000000a"),
        ("twelve digits with a separator", "0000-00000000"),
        ("no characters at all", ""),
    ];
    // Operations carrying `registryId`, and what else their request needs.
    let carriers: Vec<(&str, Value)> = vec![
        // The recorded instance's operation.
        ("DescribeRepositories", json!({})),
        ("CreateRepository", json!({"repositoryName": "well-formed"})),
        ("ListImages", json!({"repositoryName": "well-formed"})),
        ("DescribeImages", json!({"repositoryName": "well-formed"})),
        ("DeleteRepository", json!({"repositoryName": "well-formed"})),
    ];

    let router = emulator();
    let bare = format!("must satisfy regular expression {REGISTRY_ID_EXPRESSION}");
    for (shape, registry_id) in &malformed {
        for (operation, extra) in &carriers {
            let case = format!("{shape} on {operation}");
            let mut request = extra.clone();
            request["registryId"] = json!(registry_id);
            assert_clause(&router, &case, operation, &request, "registryId", &bare).await;
        }
    }

    // The other half of the rule: the classes whose expression ECR DOES delimit, so the
    // bare form above is the account-id class's and not every parameter's.
    let delimited: Vec<(&str, &str, Value, &str, String)> = vec![
        (
            "a repository name",
            "CreateRepository",
            json!({"repositoryName": "Invalid_Name"}),
            "repositoryName",
            format!("must satisfy regular expression '{REPOSITORY_NAME_EXPRESSION}'"),
        ),
        (
            "a pull through cache rule prefix",
            "DeletePullThroughCacheRule",
            json!({"ecrRepositoryPrefix": "Not A Prefix"}),
            "ecrRepositoryPrefix",
            format!("must satisfy regular expression '{PREFIX_EXPRESSION}'"),
        ),
    ];
    for (case, operation, request, parameter, constraint) in delimited {
        assert_clause(&router, case, operation, &request, parameter, &constraint).await;
    }
}

/// A value of a LIST this registry acts on no value of is refused at the LIST member, with
/// what each element must satisfy nested in the collection form — while a list ECR
/// resolves the values of is refused one value at a time, under the singular of its name.
///
/// `registryIds` is the list in question: `smithy_query(query="operation",
/// operation="GetAuthorizationToken")` shows the member `@deprecated` ("This field is
/// deprecated. The returned authorization token can be used to access any Amazon ECR
/// registry that the IAM principal has access to, specifying a registry ID doesn't change
/// the permissions scope of the authorization token."), so ECR reads no value out of it.
///
/// The rows differ along what the rule generalises over: how the element is malformed, how
/// many elements the list carries and WHICH of them offends (the first, the second, the
/// last of the ten the `@length(1..10)` admits), and whether the constraint the request
/// broke is an element's or the COLLECTION's own — crossed against the list whose values
/// ECR does resolve. The recorded instance is a single malformed element.
#[tokio::test]
async fn a_list_this_registry_reads_no_value_of_is_refused_at_the_list_member() {
    /// One well-formed account id, so a row can place the offending element anywhere.
    const WELL_FORMED: &str = "000000000000";

    let nested = format!(
        "Member must satisfy constraint: [Member must satisfy regular expression pattern: \
         {REGISTRY_ID_EXPRESSION}]"
    );
    let ten_with_last_malformed: Vec<String> = (0..9)
        .map(|_| WELL_FORMED.to_string())
        .chain(std::iter::once("00000000000".to_string()))
        .collect();

    // (case, the registryIds sent, the parameter the clause names, the constraint)
    let rows: Vec<(&str, Value, &str, String)> = vec![
        (
            // The recorded instance.
            "one element of too few digits",
            json!(["00000000000"]),
            "registryIds",
            nested.clone(),
        ),
        (
            "one element carrying a letter",
            json!(["01234567890a"]),
            "registryIds",
            nested.clone(),
        ),
        (
            "the SECOND element, so no position is in the clause",
            json!([WELL_FORMED, "0000000000000"]),
            "registryIds",
            nested.clone(),
        ),
        (
            "the last of the ten the list admits",
            json!(ten_with_last_malformed),
            "registryIds",
            nested.clone(),
        ),
        (
            // The COLLECTION's own constraint, which no element broke: stated as the
            // collection's own requirement, with nothing nested.
            "the empty list, below the list's own minimum",
            json!([]),
            "registryIds",
            "Member must have length greater than or equal to 1".to_string(),
        ),
        (
            "eleven elements, past the list's own maximum",
            json!((0..11).map(|_| WELL_FORMED).collect::<Vec<&str>>()),
            "registryIds",
            "Member must have length less than or equal to 10".to_string(),
        ),
    ];

    let router = emulator();
    for (case, registry_ids, parameter, constraint) in rows {
        let request = json!({"registryIds": registry_ids});
        assert_clause(
            &router,
            case,
            "GetAuthorizationToken",
            &request,
            parameter,
            &constraint,
        )
        .await;
    }

    // The other half of the rule: a list whose values this registry RESOLVES is refused
    // one value at a time, so its clause names that one value's parameter — the singular
    // of the list's name — and nests nothing.
    assert_clause(
        &router,
        "an element of a list the registry resolves",
        "DescribeRepositories",
        &json!({"repositoryNames": ["well-formed", "ALSO-Upper"]}),
        "repositoryName",
        &format!("must satisfy regular expression '{REPOSITORY_NAME_EXPRESSION}'"),
    )
    .await;
}
