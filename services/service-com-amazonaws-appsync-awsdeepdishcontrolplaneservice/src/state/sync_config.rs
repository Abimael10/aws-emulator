// SPDX-License-Identifier: CC0-1.0

//! The sync configuration a resolver or a function may be written with: the conflict
//! handler it names, the Lambda function that handler resolves a conflict with, and the
//! refusals for a configuration AppSync could not resolve a conflict by.
//!
//! `SyncConfig` "Specifies which Conflict Detection strategy and Resolution strategy to
//! use when the resolver is invoked"
//! (`smithy_query(query="shape", shape="SyncConfig")`), and one of the resolution
//! strategies its `conflictHandler` may name is configured by a member of its own:
//!
//! * `conflictHandler` is "The Conflict Resolution strategy to perform in the event of a
//!   conflict", and of the strategies it documents only one is parameterised —
//!   "`LAMBDA`: Resolve conflicts with an Lambda function supplied in the
//!   `LambdaConflictHandlerConfig`";
//! * `lambdaConflictHandlerConfig` is "The `LambdaConflictHandlerConfig` when configuring
//!   `LAMBDA` as the Conflict Handler", and holds one member,
//!   `lambdaConflictHandlerArn` — "The Amazon Resource Name (ARN) for the Lambda function
//!   to use as the Conflict Handler".
//!
//! Three rules follow, and each is refused with the service's `BadRequestException`
//! (HTTP 400) — the only client error for a malformed value these operations model —
//! before anything is written:
//!
//! * a configuration that resolves conflicts with a Lambda function has to SUPPLY one, so
//!   the `LAMBDA` handler without a `lambdaConflictHandlerConfig` names no function to
//!   resolve a conflict with ([`LAMBDA_CONFLICT_HANDLER_CONFIG_MISSING_MESSAGE`]). Every
//!   other handler is configured by nothing and needs no such configuration;
//! * a `lambdaConflictHandlerConfig` that IS supplied has to name a function this account
//!   holds: the member is an ARN, and AppSync invokes the function it names under the
//!   caller's own account, so a value that is not a Lambda function ARN of the calling
//!   account names no conflict handler at all
//!   ([`INVALID_LAMBDA_CONFLICT_HANDLER_ARN_MESSAGE`]). The ARN is read through the
//!   workspace's one caller-supplied-ARN parse (`::service_common::arn`), so what counts
//!   as a Lambda function ARN here is what counts as one anywhere in the tree;
//! * a RESOLVER's configuration has to have something to detect a conflict on:
//!   `syncConfig` is "The `SyncConfig` for a resolver attached to a versioned data source"
//!   (`CreateResolverRequest$syncConfig`), and a data source is configured for versioned
//!   items by its DynamoDB settings ("Set to TRUE to use Conflict Detection and Resolution
//!   with this data source", `DynamodbDataSourceConfig$versioned`), so a resolver
//!   configuring conflict resolution against any other data source — or against none —
//!   detects conflicts on nothing ([`UNVERSIONED_DATA_SOURCE_MESSAGE`]).
//!
//! The first two rules are over the MEMBERS of `SyncConfig` rather than over one request's
//! values, so they hold for every operation that writes a sync configuration
//! (`CreateResolver`, `UpdateResolver`, `CreateFunction`, `UpdateFunction`); the third is
//! about the data source a RESOLVER resolves against, which is the pairing AppSync's own
//! sentence states, so it holds on the two calls that write a resolver. A refused call
//! leaves the resolver or function it named exactly as it was — which is what a later read
//! and a later listing report.

use crate::ops::types::SyncConfig;

use super::DataSourceRecord;

/// The `ConflictHandlerType` that resolves a conflict with a function of the caller's:
/// "`LAMBDA`: Resolve conflicts with an Lambda function supplied in the
/// `LambdaConflictHandlerConfig`" (`SyncConfig$conflictHandler`). The one handler whose
/// behaviour is configured by a member beside it, and so the one a configuration can leave
/// unconfigured.
pub const LAMBDA_CONFLICT_HANDLER: &str = "LAMBDA";

/// What `lambdaConflictHandlerArn` has to name: "the Lambda function to use as the
/// Conflict Handler" (`LambdaConflictHandlerConfig$lambdaConflictHandlerArn`), i.e. a
/// function of the `lambda` namespace, so an ARN of another service — or of another
/// resource of Lambda's own, a layer or an event source mapping — names no handler.
const LAMBDA_FUNCTION: ::service_common::arn::Requirement = ::service_common::arn::Requirement {
    vendor: "lambda",
    resource_type: Some("function"),
    label: "a Lambda function ARN",
};

/// The text a `BadRequestException` (HTTP 400) carries for a sync configuration that
/// resolves conflicts with a Lambda function but supplies none: AppSync's fixed sentence,
/// which names the member that is missing and nothing of the request that left it out.
pub const LAMBDA_CONFLICT_HANDLER_CONFIG_MISSING_MESSAGE: &str =
    "Lambda config must not be null when configuring Lambda as the conflict handler.";

/// The text a `BadRequestException` (HTTP 400) carries for a supplied Lambda conflict
/// handler configuration that names no function of the calling account: AppSync's fixed
/// sentence, which names the member it judged and nothing of the value that broke the
/// rule.
pub const INVALID_LAMBDA_CONFLICT_HANDLER_ARN_MESSAGE: &str =
    "LambdaConflictHandlerArn is not a valid Lambda Arn in the calling account.";

/// Whether `arn` names a Lambda function of the account this emulator serves.
///
/// The ARN's own fields say so: its namespace and resource type make it a function
/// ([`LAMBDA_FUNCTION`]) and its account field is the calling account's, which is the one
/// account every call to this emulator is made by (`::engine::emulator_account_id`). A
/// function of ANOTHER account is a function this caller cannot name here, exactly as a
/// value that is no function ARN at all is.
fn names_own_lambda_function(arn: &str) -> bool {
    LAMBDA_FUNCTION.admits(arn)
        && ::service_common::arn::parse(arn)
            .is_some_and(|parsed| parsed.account == ::engine::emulator_account_id())
}

/// The refusal text for a sync configuration AppSync could not resolve a conflict with, or
/// `None` for one it could — a request that carries no `syncConfig`, one whose
/// `conflictHandler` is configured by nothing and that supplies no Lambda configuration,
/// or one that supplies a configuration naming a Lambda function of this account.
///
/// One call states ONE refusal: a configuration that is missing states that, and a
/// configuration that is there is judged on the function it names.
pub fn sync_config_refusal(sync_config: Option<&SyncConfig>) -> Option<String> {
    let sync_config = sync_config?;
    let lambda_handler = sync_config
        .conflict_handler
        .as_deref()
        .is_some_and(|handler| handler == LAMBDA_CONFLICT_HANDLER);
    let Some(lambda_config) = sync_config.lambda_conflict_handler_config.as_ref() else {
        return lambda_handler.then(|| LAMBDA_CONFLICT_HANDLER_CONFIG_MISSING_MESSAGE.to_string());
    };
    let arn = lambda_config
        .lambda_conflict_handler_arn
        .as_deref()
        .unwrap_or_default();
    (!names_own_lambda_function(arn))
        .then(|| INVALID_LAMBDA_CONFLICT_HANDLER_ARN_MESSAGE.to_string())
}

/// The text a `BadRequestException` (HTTP 400) carries for a resolver that carries a sync
/// configuration while resolving against a data source that keeps no versioned items:
/// AppSync's fixed sentence, which names nothing of the request that broke the rule.
pub const UNVERSIONED_DATA_SOURCE_MESSAGE: &str =
    "Data source must be configured for versioned items when specifying SyncConfig on the \
     resolver.";

/// Whether `data_source` keeps VERSIONED items, i.e. whether conflicts can arise on it at
/// all: a data source is configured for them by its DynamoDB settings — "Set to TRUE to
/// use Conflict Detection and Resolution with this data source"
/// (`DynamodbDataSourceConfig$versioned`) — and no other data source type carries such a
/// member, so no other type can keep them.
fn keeps_versioned_items(data_source: &DataSourceRecord) -> bool {
    data_source
        .dynamodb_config
        .as_ref()
        .is_some_and(|config| config.versioned)
}

/// The refusal text for a resolver whose sync configuration has nothing to detect a
/// conflict ON, or `None` when it has.
///
/// `syncConfig` is "The `SyncConfig` for a resolver attached to a VERSIONED data source"
/// (`CreateResolverRequest$syncConfig`): conflict detection and resolution work on the
/// item versions the data source keeps, so a resolver that configures them has to resolve
/// against a data source configured for versioned items ([`keeps_versioned_items`]) — and
/// a resolver that names no data source at all (one whose data source is not this API's,
/// or a pipeline resolver, which resolves through its functions) names none that could be.
///
/// `data_source` is the data source the resolver resolves against as the API holds it, or
/// `None` when the resolver names none.
pub fn versioned_data_source_refusal(
    sync_config: Option<&SyncConfig>,
    data_source: Option<&DataSourceRecord>,
) -> Option<String> {
    sync_config?;
    (!data_source.is_some_and(keeps_versioned_items))
        .then(|| UNVERSIONED_DATA_SOURCE_MESSAGE.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::LambdaConflictHandlerConfig;

    /// One case of the handler table: what it exercises, the `conflictHandler` the
    /// configuration names (`None` where it names none), and the Lambda configuration it
    /// supplies — `None` for a configuration carrying none at all, and `Some(None)` for
    /// one that carries it while naming no function.
    type Case<'a> = (&'a str, Option<&'a str>, Option<Option<&'a str>>);

    /// The ARN of a Lambda function of the account this emulator serves, built the way
    /// every ARN in this crate is (`::engine::emulator_arn`), so the account is never
    /// written out here.
    fn own_function(name: &str) -> String {
        ::engine::emulator_arn("lambda", "us-east-1", &format!("function:{name}"))
    }

    /// A sync configuration over `handler`, carrying the Lambda configuration or not.
    fn config(handler: Option<&str>, lambda: Option<Option<&str>>) -> SyncConfig {
        SyncConfig {
            conflict_handler: handler.map(str::to_string),
            conflict_detection: Some("VERSION".to_string()),
            lambda_conflict_handler_config: lambda.map(|arn| LambdaConflictHandlerConfig {
                lambda_conflict_handler_arn: arn.map(str::to_string),
            }),
        }
    }

    #[test]
    fn a_configured_handler_is_not_refused() {
        // One case per `ConflictHandlerType` value the model lists
        // (`smithy_query(query="shape", shape="ConflictHandlerType")`): only the Lambda
        // handler is configured by a member of its own, so only it can be unconfigured.
        let own = own_function("resolve");
        let qualified = own_function("resolve:live");
        let cases: &[Case<'_>] = &[
            ("no conflict handler", None, None),
            ("optimistic concurrency", Some("OPTIMISTIC_CONCURRENCY"), None),
            ("automerge", Some("AUTOMERGE"), None),
            ("no conflict resolution", Some("NONE"), None),
            // A value outside the published set never reaches this reading — the request
            // boundary refuses it on the modelled enum — so what is read here is the one
            // published handler a configuration can leave unconfigured.
            (
                "the lambda handler with its function",
                Some("LAMBDA"),
                Some(Some(own.as_str())),
            ),
            (
                "a qualified function",
                Some("LAMBDA"),
                Some(Some(qualified.as_str())),
            ),
            (
                "a function supplied beside another handler",
                Some("AUTOMERGE"),
                Some(Some(own.as_str())),
            ),
        ];
        for (case, handler, lambda) in cases {
            let refusal = sync_config_refusal(Some(&config(*handler, *lambda)));
            assert!(
                refusal.is_none(),
                "case {case:?}: expected no refusal, got {refusal:?}"
            );
        }
    }

    #[test]
    fn the_lambda_handler_without_its_configuration_is_refused() {
        let refusal = sync_config_refusal(Some(&config(Some(LAMBDA_CONFLICT_HANDLER), None)));
        assert_eq!(
            refusal.as_deref(),
            Some(LAMBDA_CONFLICT_HANDLER_CONFIG_MISSING_MESSAGE),
            "case the lambda handler supplying no function: expected \
             {LAMBDA_CONFLICT_HANDLER_CONFIG_MISSING_MESSAGE:?}, got {refusal:?}"
        );
    }

    #[test]
    fn a_configuration_naming_no_function_of_this_account_is_refused() {
        let cases: &[(&str, Option<&str>)] = &[
            ("no ARN at all", None),
            ("an empty ARN", Some("")),
            ("a bare function name", Some("resolve")),
            (
                "an ARN of another account",
                Some("arn:aws:lambda:us-east-1:210987654321:function:resolve"),
            ),
            (
                "an ARN of another service",
                Some("arn:aws:states:us-east-1:000000000000:stateMachine:resolve"),
            ),
            (
                "another resource of Lambda's own",
                Some("arn:aws:lambda:us-east-1:000000000000:layer:resolve"),
            ),
            (
                "an account-less ARN",
                Some("arn:aws:lambda:us-east-1::function:resolve"),
            ),
        ];
        for (case, arn) in cases {
            let refusal = sync_config_refusal(Some(&config(
                Some(LAMBDA_CONFLICT_HANDLER),
                Some(*arn),
            )));
            assert_eq!(
                refusal.as_deref(),
                Some(INVALID_LAMBDA_CONFLICT_HANDLER_ARN_MESSAGE),
                "case {case:?}: expected {INVALID_LAMBDA_CONFLICT_HANDLER_ARN_MESSAGE:?}, \
                 got {refusal:?}"
            );
        }
    }

    #[test]
    fn no_sync_configuration_is_nothing_to_refuse() {
        let refusal = sync_config_refusal(None);
        assert!(
            refusal.is_none(),
            "case a request carrying no syncConfig: expected no refusal, got {refusal:?}"
        );
    }
}
