// SPDX-License-Identifier: CC0-1.0

//! The catalogue of AWS-authored extensions AppConfig publishes to every account, read as
//! a SET and as published text.
//!
//! The rule under test: the AWS-authored half of an account's extension catalogue is
//! AppConfig's own published data, so a reader of it reports exactly the entries AppConfig
//! publishes — no entry it does not publish, and none of its entries missing — each under
//! the published identifier a caller then passes to `GetExtension` or
//! `CreateExtensionAssociation`, and each carrying AppConfig's published description
//! verbatim; nothing in that data is read off the request or the account's state, so the
//! only environment-specific part of an entry is the partition and Region of its ARN.
//!
//! The table ([`PUBLISHED_CATALOGUE`]) is the whole published catalogue rather than the one
//! entry a client happened to look for, written out here rather than read from the
//! service's own constant so a row of that constant that drifted from the published data
//! fails here instead of agreeing with itself. Its rows differ along what the rule
//! generalises over: which extension, which product it integrates, how many parameters it
//! declares (none and one), and how long and how structured its published sentence is. The
//! identifier forms (id, prose name, ARN) and the page size the catalogue is walked at are
//! the cases within a row. Every case drives a real HTTP request through the router the
//! emulator's front door uses.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_appconfig_amazonappconfig::state::AWS_AUTHORED_EXTENSION_VERSION;

use crate::appconfig_workflow::{account, create_application, router, Answer};
use crate::aws_authored_extensions::authored_arn;
use crate::extension_associations::{associate, get_association};
use crate::extensions::get_extension;
use crate::list_extensions::walk;
use crate::resource_tags::arn;

/// One entry of AppConfig's published AWS-authored extension catalogue.
///
/// `pub(crate)` because the suite that asserts this published text is the SAME text
/// whatever the request and whatever the account holds
/// (`crate::authored_catalogue_fixed_text`) reads these rows rather than hand-copying them
/// a third time.
pub(crate) struct PublishedExtension {
    /// The published identifier: what a caller passes as an `ExtensionIdentifier`, and the
    /// tail of the ARN each of the extension's versions is reported under.
    pub(crate) id: &'static str,
    /// The prose name published beside the id.
    pub(crate) name: &'static str,
    /// The published sentence describing what the extension does, verbatim.
    pub(crate) description: &'static str,
    /// The parameter names an association of this extension supplies values for.
    pub(crate) parameters: &'static [&'static str],
}

/// `Description` is `@length 0..1024`
/// (`smithy_query(query="shape",shape="ExtensionSummary")`), so no published sentence may
/// be longer than this.
const DESCRIPTION_MAX: usize = 1024;

/// The whole catalogue AppConfig publishes to an account that has created no extension of
/// its own.
///
/// Source: the catalogue an AppConfig `ListExtensions` call reports for such an account, as
/// the issues this suite grew from quote it entry by entry, together with the AWS AppConfig
/// User Guide's predefined-extension list for the parameter each association supplies. The
/// rows differ in how their sentence opens as well as in which product it names: the
/// EventBridge entry sends events to EventBridge, the Amazon SNS entry events to the
/// configured topic and the Amazon SQS entry messages to the configured queue, so no row's
/// sentence can be read off a sibling's.
pub(crate) const PUBLISHED_CATALOGUE: &[PublishedExtension] = &[
    PublishedExtension {
        id: "AWS.AppConfig.FeatureFlags",
        name: "AppConfig Feature Flags Helper",
        description: "Validates AppConfig feature flag data automatically against a JSON schema \
                      that includes structure and constraints. Also transforms feature flag data \
                      prior to sending to the client. This extension is automatically associated \
                      to configuration profiles with type \"AWS.AppConfig.FeatureFlags\".",
        parameters: &[],
    },
    PublishedExtension {
        id: "AWS.AppConfig.JiraIntegration",
        name: "AppConfig integration with Atlassian Jira",
        description: "Exports feature flag data from AWS AppConfig into Jira. The lifecycle of \
                      each feature flag in AppConfig is tracked in Jira as an individual issue. \
                      Customers can see in Jira when flags are updated, turned on or off. Works \
                      in conjunction with the AppConfig app in the Atlassian Marketplace and is \
                      automatically associated to configuration profiles configured within that \
                      app.",
        parameters: &[],
    },
    PublishedExtension {
        id: "AWS.Evidently.ProjectImporter",
        name: "Amazon CloudWatch Evidently Project importer",
        description: "Imports Evidently Project data for use with its client-side evaluation \
                      feature, powered by AppConfig. This extension is automatically associated \
                      to ConfigurationProfiles created by Evidently.",
        parameters: &[],
    },
    PublishedExtension {
        id: "AWS.AppConfig.DeploymentNotificationsToEventBridge",
        name: "AppConfig deployment events to Amazon EventBridge",
        description: "Sends events to Amazon EventBridge when a deployment of configuration data \
                      in AppConfig is started, completed, or rolled back. Can be associated to \
                      the following resources in AppConfig: Application, Environment, \
                      Configuration Profile.",
        parameters: &[],
    },
    PublishedExtension {
        id: "AWS.AppConfig.DeploymentNotificationsToSns",
        name: "AppConfig deployment events to Amazon SNS",
        description: "Sends events to the configured Amazon SNS topic when a deployment of \
                      configuration data in AppConfig is started, completed, or rolled back. Can \
                      be associated to the following resources in AppConfig: Application, \
                      Environment, Configuration Profile.",
        parameters: &["topicArn"],
    },
    PublishedExtension {
        id: "AWS.AppConfig.DeploymentNotificationsToSqs",
        name: "AppConfig deployment events to Amazon SQS",
        description: "Sends messages to the configured Amazon SQS queue when a deployment of \
                      configuration data in AppConfig is started, completed, or rolled back. Can \
                      be associated to the following resources in AppConfig: Application, \
                      Environment, Configuration Profile.",
        parameters: &["queueArn"],
    },
];

/// The ARN field an extension AppConfig itself authors leaves EMPTY, because no account
/// owns it: how a client tells the published half of the catalogue from its own extensions.
const ACCOUNT_FIELD: usize = 4;

/// The `extension/<id>/<version>` tail of every AWS-authored entry of a listing, as a set.
///
/// An entry is authored exactly when its ARN names no account, which is the one structural
/// difference between a published entry and one this account created.
fn authored_tails(case: &str, listed: &[Value]) -> BTreeSet<String> {
    listed
        .iter()
        .filter_map(|item| {
            let value = item.get("Arn").and_then(Value::as_str).unwrap_or_else(|| {
                panic!("case {case:?}: every listed extension reports an Arn, got {item}")
            });
            let fields: Vec<&str> = value.splitn(6, ':').collect();
            assert_eq!(
                fields.len(),
                6,
                "case {case:?}: an ARN has six fields, got {value}"
            );
            (fields[ACCOUNT_FIELD].is_empty()).then(|| fields[5].to_string())
        })
        .collect()
}

/// The tails [`PUBLISHED_CATALOGUE`] says the listing must report, and nothing else.
fn published_tails() -> BTreeSet<String> {
    PUBLISHED_CATALOGUE
        .iter()
        .map(|published| {
            format!(
                "extension/{}/{AWS_AUTHORED_EXTENSION_VERSION}",
                published.id
            )
        })
        .collect()
}

/// A value for one published parameter name, so an association can be created for a row
/// that declares one. The value is the caller's, not the extension's: it is only echoed
/// back.
fn parameter_value(name: &str) -> String {
    let service = if name.starts_with("topic") { "sns" } else { "sqs" };
    format!(
        "arn:aws:{service}:us-east-1:{}:appconfig-catalog-notifications",
        account()
    )
}

/// The `Parameters` map an association of `published` supplies.
fn parameters_for(published: &PublishedExtension) -> Value {
    let mut map = serde_json::Map::new();
    for name in published.parameters {
        map.insert((*name).to_string(), Value::from(parameter_value(name)));
    }
    Value::Object(map)
}

/// A 200 answer's body, with the status named in the failure.
fn body_of(case: &str, answer: &Answer) -> Value {
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: expected 200, got {} / {}",
        answer.status,
        String::from_utf8_lossy(&answer.body)
    );
    answer.json()
}

/// The AWS-authored entries a fresh account reads are EXACTLY the published catalogue:
/// every published entry is there under its published identifier, and no entry AppConfig
/// does not publish is.
///
/// Cases: the page size the catalogue is walked at — one entry per page (the published
/// minimum), the published maximum, and no page size at all — because the set a client ends
/// up with may not depend on how it paged through the listing.
#[tokio::test]
async fn the_authored_catalogue_is_exactly_the_published_entry_set() {
    let router = router();
    let expected = published_tails();
    for (case, page_size) in [
        ("one entry per page", Some(1)),
        ("the largest page", Some(50)),
        ("no page size", None),
    ] {
        let listed = walk(&router, case, page_size).await;
        let actual = authored_tails(case, &listed);
        let missing: Vec<&String> = expected.difference(&actual).collect();
        let extra: Vec<&String> = actual.difference(&expected).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "case {case:?}: expected the published catalogue, missing {missing:?} and \
             serving {extra:?} it does not publish (got {actual:?})"
        );
    }
}

/// Every published entry carries AppConfig's published description verbatim — the sentence
/// published for THAT extension, within the modelled bound, reported identically by every
/// reader of the entry.
///
/// Rows: the whole published catalogue. Cases within a row: the listing's summary, and
/// `GetExtension` by id, by prose name, by the ARN of the published version, and by that
/// ARN with the version pinned.
#[tokio::test]
async fn every_published_entry_reports_its_published_description_verbatim() {
    let router = router();
    let listed = walk(&router, "the published catalogue", None).await;

    for published in PUBLISHED_CATALOGUE {
        let case = published.id;
        let summary = listed
            .iter()
            .find(|item| item.get("Id").and_then(Value::as_str) == Some(published.id))
            .unwrap_or_else(|| {
                panic!("case {case:?}: the listing must report this extension, got {listed:?}")
            });
        let served = summary
            .get("Description")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: a published entry carries a description, got {summary}")
            });
        assert_eq!(
            served, published.description,
            "case {case:?}: expected the published description, got {served:?}"
        );
        // Published for every entry: text AppConfig keys to the extension, within the
        // member's modelled bound.
        assert!(
            !served.trim().is_empty() && served.len() <= DESCRIPTION_MAX,
            "case {case:?}: expected a published sentence of at most {DESCRIPTION_MAX} \
             characters, got {served:?}"
        );
        assert_eq!(
            summary.get("Name").and_then(Value::as_str),
            Some(published.name),
            "case {case:?}: expected the published name, got {summary}"
        );

        let expected_arn = authored_arn(published.id);
        for (form, identifier, version) in [
            ("by id", published.id.to_string(), None),
            ("by name", published.name.to_string(), None),
            ("by ARN", expected_arn.clone(), None),
            (
                "by ARN at its version",
                expected_arn.clone(),
                Some(AWS_AUTHORED_EXTENSION_VERSION),
            ),
        ] {
            let read = body_of(case, &get_extension(&router, &identifier, version).await);
            assert_eq!(
                read.get("Description").and_then(Value::as_str),
                Some(served),
                "case {case:?} {form}: expected the description the listing reports, got {read}"
            );
        }
    }
}

/// Every published identifier is one a client can USE: the identifier AppConfig publishes
/// resolves to the published extension when an association is created naming it, whichever
/// published form the client took from the catalogue.
///
/// Rows: the whole published catalogue, including the entries that declare a parameter an
/// association must supply a value for. Cases within a row: the id, the prose name and the
/// version's ARN.
#[tokio::test]
async fn every_published_identifier_resolves_when_an_extension_is_associated() {
    let router = router();
    let application = create_application(&router, "catalog-association-app").await;
    let resource_arn = arn(&format!("application/{application}"));

    for published in PUBLISHED_CATALOGUE {
        let case = published.id;
        let expected_arn = authored_arn(published.id);
        let parameters = parameters_for(published);
        for (form, identifier) in [
            ("by id", published.id.to_string()),
            ("by name", published.name.to_string()),
            ("by ARN", expected_arn.clone()),
        ] {
            let created = associate(
                &router,
                &json!({
                    "ExtensionIdentifier": identifier,
                    "ResourceIdentifier": resource_arn,
                    "Parameters": parameters,
                }),
            )
            .await;
            assert_eq!(
                created.status,
                201,
                "case {case:?} {form}: expected 201 for an association of a published \
                 extension, got {} / {}",
                created.status,
                String::from_utf8_lossy(&created.body)
            );
            let body = created.json();
            assert_eq!(
                body.get("ExtensionArn").and_then(Value::as_str),
                Some(expected_arn.as_str()),
                "case {case:?} {form}: expected the published version's ARN, got {body}"
            );
            assert_eq!(
                body.get("ExtensionVersionNumber").and_then(Value::as_i64),
                Some(i64::from(AWS_AUTHORED_EXTENSION_VERSION)),
                "case {case:?} {form}: expected the published version, got {body}"
            );

            // Read back through the association's own reader, so what the create resolved
            // is what a later caller sees.
            let id = body
                .get("Id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("case {case:?} {form}: expected an Id, got {body}"));
            let read = body_of(case, &get_association(&router, id).await);
            assert_eq!(
                read.get("ExtensionArn").and_then(Value::as_str),
                Some(expected_arn.as_str()),
                "case {case:?} {form}: the association reads back bound to the published \
                 extension, got {read}"
            );
            assert_eq!(
                read.get("Parameters").cloned().unwrap_or(Value::Null),
                parameters,
                "case {case:?} {form}: expected the supplied parameter values, got {read}"
            );
        }
    }
}
