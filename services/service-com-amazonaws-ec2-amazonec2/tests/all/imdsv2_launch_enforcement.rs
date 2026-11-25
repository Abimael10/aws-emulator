// SPDX-License-Identifier: CC0-1.0

//! The account's IMDSv2 ENFORCEMENT at launch — the account-level setting that decides
//! whether `RunInstances` may launch an instance at all — driven end to end over the wire EC2
//! speaks (`ec2Query`): the setting is put in place through `ModifyInstanceMetadataDefaults`,
//! the launches are real `RunInstances` calls, and what each one left behind is read back
//! through `DescribeInstances`.
//!
//! THE RULE UNDER TEST: "specifies whether to enforce the requirement of IMDSv2 on an instance
//! at the time of launch. When enforcement is enabled, the instance can't launch unless IMDSv2
//! (`HttpTokens`) is set to `required`"
//! (`ModifyInstanceMetadataDefaultsRequest$HttpTokensEnforced`). The question is asked of the
//! `HttpTokens` the instance would REALLY run with, which is the documented order of
//! precedence for the metadata options — the launch's own `MetadataOptions`, then the
//! account-level default, then the member's documented default, with `required` the default
//! for an AMI whose `ImdsSupport` is `v2.0` and an account that states no preference
//! (`InstanceMetadataOptionsRequest$HttpTokens`) — so a launch that asks for `optional` and
//! one that merely inherits it are the same launch to this setting. A launch the setting does
//! not admit is refused HTTP 400 `UnsupportedOperation` with the fixed sentence that refusal
//! carries, and nothing is created; every other launch is served, with the metadata
//! options it runs under readable back on the instance.
//!
//! The rows span what the rule generalises over: where the effective `HttpTokens` comes from
//! (the launch's own member, the account's default, the documented default), which value it
//! resolves to, the state of the enforcement setting itself (`enabled`, `disabled`, cleared
//! with `no-preference`, never set), and whether the launch carries `DryRun`. No row is the
//! specification: each is one input along the rule.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{child_text, document, param, set_items, Param};

/// The Region every call here is signed for: the one the default network is seeded for, so a
/// launch that names no subnet lands in the account's default subnet.
const REGION: &str = "us-east-1";

/// EC2's error code for a request that asked for something it does not support for what the
/// request named, and the status it carries.
const UNSUPPORTED_OPERATION: &str = "UnsupportedOperation";
const UNSUPPORTED_STATUS: u16 = 400;

/// The sentence EC2 refuses such a launch with. FIXED text: it names nothing of the request.
///
/// The error class's own fact: `optional` is what "means that you can use either IMDSv2 or
/// IMDSv1" and `required` what "means that IMDSv1 is disabled"
/// (`InstanceMetadataOptionsRequest$HttpTokens`), which is what the account's enforcement has
/// done to IMDSv1 here.
const IMDSV2_ENFORCED_LAUNCH_MESSAGE: &str = "IMDSv1 is disabled for this account.";

/// EC2's answer to a `DryRun` the service WOULD have served, and the status it carries.
const DRY_RUN_OPERATION: &str = "DryRunOperation";
const DRY_RUN_STATUS: u16 = 412;

/// The emulator the tests drive.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One call to `action` with `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// An image id of the account a launch can name, chosen so the launch's own metadata options
/// and the account's defaults are the only things that decide its `HttpTokens`: an image whose
/// `ImdsSupport` is NOT `v2.0`, because "if the value of `ImdsSupport` for the Amazon Machine
/// Image (AMI) for your instance is `v2.0` and the account level default is set to
/// `no-preference`, the default is `required`" (`InstanceMetadataOptionsRequest$HttpTokens`) —
/// which would flip the rows that expect the documented `optional` default.
async fn image_id(router: &MultiServiceRouter) -> String {
    let case = "the image the rows launch from";
    let (status, images) = call(router, "DescribeImages", &[]).await;
    assert_eq!(
        status, 200,
        "the published images must be readable, got {status} {images}"
    );
    let parsed = document(case, &images);
    set_items(parsed.root_element(), "imagesSet")
        .into_iter()
        .find(|item| {
            child_text(*item, "imdsSupport").as_deref() != Some(IMDS_SUPPORT_V2)
                && child_text(*item, "imageId").is_some()
        })
        .and_then(|item| child_text(item, "imageId"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: no published image leaves IMDSv2 unrequired in {images}")
        })
}

/// `ImdsSupport`'s `v2.0`: the AMI declaration that makes `required` the launch's default
/// (`RegisterImageRequest$ImdsSupport`).
const IMDS_SUPPORT_V2: &str = "v2.0";

/// Put one account-level IMDS default setting to `value`.
async fn set_default(router: &MultiServiceRouter, case: &str, member: &str, value: &str) {
    let (status, body) = call(
        router,
        "ModifyInstanceMetadataDefaults",
        &[param(member, value)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the account default {member}={value} must be set, got {status} {body}"
    );
}

/// Every instance id the account holds, read back through `DescribeInstances`.
async fn instance_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, instances) = call(router, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {instances}"
    );
    elements(&instances, "instanceId")
}

/// The `httpTokens` the instance `instance_id` reports
/// (`InstanceMetadataOptionsResponse$HttpTokens`, reported under `Instance$MetadataOptions`).
async fn reported_http_tokens(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> String {
    let (status, described) = call(
        router,
        "DescribeInstances",
        &[param("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read back must be served, got {status} {described}"
    );
    element(&described, "httpTokens")
        .unwrap_or_else(|| panic!("case {case:?}: no <httpTokens> in {described}"))
}

/// The form fields of a launch of one instance, with the `MetadataOptions` members `options`
/// sets and `DryRun` when the row asks for it.
fn launch_params(
    image_id: &str,
    options: &[(&str, &str)],
    dry_run: bool,
) -> Vec<Param> {
    let mut params = vec![
        param("ImageId", image_id),
        param("MinCount", "1"),
        param("MaxCount", "1"),
    ];
    for (member, value) in options {
        params.push(param(&format!("MetadataOptions.{member}"), value));
    }
    if dry_run {
        params.push(param("DryRun", "true"));
    }
    params
}

/// One row: the account's IMDS defaults, the metadata options the launch carries, and the
/// `HttpTokens` the instance would run with — which is what decides the launch.
struct Row {
    case: &'static str,
    /// The account's own `HttpTokens` default in force for the launch, or `None` for an
    /// account that states no preference for it.
    account_http_tokens: Option<&'static str>,
    /// The `MetadataOptions` members the launch carries.
    options: &'static [(&'static str, &'static str)],
    /// The `HttpTokens` the launch resolves to, which the launched instance reports.
    effective: &'static str,
    /// Whether the account's enforcement setting admits that launch.
    admitted: bool,
}

/// `HttpTokensState`'s two members, as the wire carries them.
const REQUIRED: &str = "required";
const OPTIONAL: &str = "optional";

/// The rows: every combination of where the effective `HttpTokens` comes from and what the
/// enforcement setting is, crossed in [`the_accounts_enforcement_decides_the_launch`].
const ROWS: &[Row] = &[
    Row {
        case: "a launch that asks for IMDSv2 itself",
        account_http_tokens: None,
        options: &[("HttpTokens", REQUIRED)],
        effective: REQUIRED,
        admitted: true,
    },
    Row {
        case: "a launch that asks for IMDSv1 to stay usable",
        account_http_tokens: None,
        options: &[("HttpTokens", OPTIONAL)],
        effective: OPTIONAL,
        admitted: false,
    },
    Row {
        // The documented default of an account that states no preference and an AMI that
        // declares no IMDSv2 support.
        case: "a launch that names no metadata options at all",
        account_http_tokens: None,
        options: &[],
        effective: OPTIONAL,
        admitted: false,
    },
    Row {
        // The account's own default stands in for what the launch left out, so the instance
        // requires IMDSv2 without the request saying so.
        case: "a launch that inherits IMDSv2 from the account's default",
        account_http_tokens: Some(REQUIRED),
        options: &[],
        effective: REQUIRED,
        admitted: true,
    },
    Row {
        case: "a launch that inherits IMDSv1 from the account's default",
        account_http_tokens: Some(OPTIONAL),
        options: &[],
        effective: OPTIONAL,
        admitted: false,
    },
    Row {
        // The launch's own member outranks the account's default, both ways round.
        case: "a launch that overrides the account's IMDSv2 default with IMDSv1",
        account_http_tokens: Some(REQUIRED),
        options: &[("HttpTokens", OPTIONAL)],
        effective: OPTIONAL,
        admitted: false,
    },
    Row {
        case: "a launch that overrides the account's IMDSv1 default with IMDSv2",
        account_http_tokens: Some(OPTIONAL),
        options: &[("HttpTokens", REQUIRED)],
        effective: REQUIRED,
        admitted: true,
    },
    Row {
        // A metadata option that is not the IMDSv2 requirement decides nothing about the
        // launch; the effective `HttpTokens` beside it still does.
        case: "a launch that sets the endpoint and the hop limit beside IMDSv2",
        account_http_tokens: None,
        options: &[
            ("HttpTokens", REQUIRED),
            ("HttpEndpoint", "enabled"),
            ("HttpPutResponseHopLimit", "2"),
        ],
        effective: REQUIRED,
        admitted: true,
    },
];

/// Drive one row's launch and assert what it answered, and what it left behind.
async fn assert_launch(
    router: &MultiServiceRouter,
    case: &str,
    image_id: &str,
    row: &Row,
    enforced: bool,
) {
    let before = instance_ids(router, case).await;
    let (status, body) = call(
        router,
        "RunInstances",
        &launch_params(image_id, row.options, false),
    )
    .await;
    if enforced && !row.admitted {
        assert_eq!(
            (status, error_code(&body).as_str(), error_message(&body).as_str()),
            (
                UNSUPPORTED_STATUS,
                UNSUPPORTED_OPERATION,
                IMDSV2_ENFORCED_LAUNCH_MESSAGE
            ),
            "case {case:?}: expected the unsupported-launch refusal, got {status} {body}"
        );
        let after = instance_ids(router, case).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused launch must create nothing, got {after:?}"
        );
        return;
    }
    assert_eq!(
        status, 200,
        "case {case:?}: the launch must be served, got {status} {body}"
    );
    let instance_id = element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: no <instanceId> in {body}"));
    let reported = reported_http_tokens(router, case, &instance_id).await;
    assert_eq!(
        reported, row.effective,
        "case {case:?}: expected the instance to run with HttpTokens {:?}, got {reported:?}",
        row.effective
    );
    let after = instance_ids(router, case).await;
    assert_eq!(
        after.len(),
        before.len() + 1,
        "case {case:?}: expected exactly the launched instance, got {after:?}"
    );
}

/// RULE: with the account's IMDSv2 enforcement `enabled`, a launch whose effective
/// `HttpTokens` is not `required` cannot launch — 400 `UnsupportedOperation`, nothing created
/// — and one whose effective `HttpTokens` IS `required` is served; with enforcement off, or
/// never set, every one of those launches is served.
#[tokio::test]
async fn the_accounts_enforcement_decides_the_launch() {
    // (the case of the enforcement setting, the value to put it to, whether it enforces)
    let settings: [(&str, Option<&str>, bool); 4] = [
        ("enforcement enabled", Some("enabled"), true),
        ("enforcement disabled", Some("disabled"), false),
        // "To remove a parameter's account-level default setting, specify `no-preference`"
        // (`ModifyInstanceMetadataDefaults`): a cleared setting enforces nothing.
        ("enforcement cleared again", Some("no-preference"), false),
        ("enforcement never set", None, false),
    ];
    for (setting, value, enforced) in settings {
        // One account per setting: the rows launch into it one after another, each one
        // naming the account default it needs — the ones a row states, and `no-preference`
        // for the ones it does not, so no row inherits the row before it.
        let router = emulator();
        let image_id = image_id(&router).await;
        if let Some(value) = value {
            // The setting is put to `enabled` first for the cleared case, so clearing it is
            // a change rather than a no-op.
            if value == "no-preference" {
                set_default(&router, setting, "HttpTokensEnforced", "enabled").await;
            }
            set_default(&router, setting, "HttpTokensEnforced", value).await;
        }
        for row in ROWS {
            let case = format!("{setting}: {}", row.case);
            let account_http_tokens = row.account_http_tokens.unwrap_or("no-preference");
            set_default(&router, &case, "HttpTokens", account_http_tokens).await;
            assert_launch(&router, &case, &image_id, row, enforced).await;
        }
    }
}

/// RULE (the probe is answered by the same decision): a `DryRun` "checks whether you have the
/// required permissions for the action, without actually making the request"
/// (`RunInstancesRequest$DryRun`), so a launch that would have been refused for its own
/// metadata options is answered THAT refusal rather than the dry-run verdict, and one the
/// account's enforcement admits is answered the verdict; neither launches anything.
#[tokio::test]
async fn a_dry_run_launch_is_answered_by_the_same_decision() {
    let router = emulator();
    let image_id = image_id(&router).await;
    let case = "enforcement enabled";
    set_default(&router, case, "HttpTokensEnforced", "enabled").await;

    // (case, the metadata options the probe carries, the status and code it must answer)
    type Probe = (
        &'static str,
        &'static [(&'static str, &'static str)],
        u16,
        &'static str,
    );
    let probes: [Probe; 2] = [
        (
            "a probe of a launch the enforcement does not admit",
            &[("HttpTokens", OPTIONAL)],
            UNSUPPORTED_STATUS,
            UNSUPPORTED_OPERATION,
        ),
        (
            "a probe of a launch it does admit",
            &[("HttpTokens", REQUIRED)],
            DRY_RUN_STATUS,
            DRY_RUN_OPERATION,
        ),
    ];
    for (case, options, status_expected, code_expected) in probes {
        let (status, body) = call(
            &router,
            "RunInstances",
            &launch_params(&image_id, options, true),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (status_expected, code_expected),
            "case {case:?}: expected HTTP {status_expected} {code_expected}, got {status} {body}"
        );
    }
    let ids = instance_ids(&router, case).await;
    assert!(
        ids.is_empty(),
        "case {case:?}: a probe must launch nothing, got {ids:?}"
    );
}

/// RULE (enforcement is a fact of the ACCOUNT, read at the instant of the launch): turning it
/// on refuses the next launch of the same shape, and turning it off again serves it — nothing
/// about the decision is remembered from an earlier call.
#[tokio::test]
async fn enforcement_decides_each_launch_as_it_stands() {
    let router = emulator();
    let image_id = image_id(&router).await;
    let options: &[(&str, &str)] = &[("HttpTokens", OPTIONAL)];

    // (case, the enforcement setting now in force, whether the launch is served)
    let steps: [(&str, Option<&str>, bool); 4] = [
        ("before any enforcement is set", None, true),
        ("once enforcement is enabled", Some("enabled"), false),
        ("once it is cleared again", Some("no-preference"), true),
        ("once it is enabled a second time", Some("enabled"), false),
    ];
    let mut launched = 0usize;
    for (case, setting, served) in steps {
        if let Some(setting) = setting {
            set_default(&router, case, "HttpTokensEnforced", setting).await;
        }
        let (status, body) = call(
            &router,
            "RunInstances",
            &launch_params(&image_id, options, false),
        )
        .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: the launch must be served, got {status} {body}"
            );
            launched += 1;
        } else {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (UNSUPPORTED_STATUS, UNSUPPORTED_OPERATION),
                "case {case:?}: expected the unsupported-launch refusal, got {status} {body}"
            );
        }
        let ids = instance_ids(&router, case).await;
        assert_eq!(
            ids.len(),
            launched,
            "case {case:?}: expected {launched} instance(s) of this account, got {ids:?}"
        );
    }
}
