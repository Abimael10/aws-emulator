// SPDX-License-Identifier: CC0-1.0

//! Which `PatchFilter` keys a patch baseline may select on, end to end: a write of a
//! baseline states its criteria with the patch properties of ITS OWN operating system, and
//! a key naming a property of another operating system's patches is refused rather than
//! stored.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call_in_region`], [`error_code`],
//! [`error_message`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct: an admitted
//! baseline is read back through `GetPatchBaseline`, and a refused one is shown to have left
//! nothing behind by `DescribePatchBaselines` with a `NAME_PREFIX` filter on the name the
//! refused write carried.
//!
//! The rule under test is the one `PatchFilter$Key` states and the model cannot: "the key
//! for the filter. Run the `DescribePatchProperties` command to view lists of valid keys for
//! each operating system type" (`smithy_query(query="shape", shape="PatchFilter")`), whose
//! per-operating-system lists `DescribePatchProperties`' own doc publishes
//! (`smithy_query(query="doc", shape="DescribePatchProperties")`). The suites are tables
//! whose rows differ along what the rule generalises over — the operating system, the key,
//! which of the two places a write states filters carries it, and whether the write is a
//! create or an update — rather than the single create a recorded unit happened to make.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, REGION};

/// How Systems Manager words the refusal of a filter key that names no property of the
/// patches it was handed to filter: the fixed text, then the key the request carried.
fn unknown_filter_key(key: &str) -> String {
    format!("Unknown Filter Key: {key}")
}

/// One row of the per-operating-system key table: a `(operating system, key)` pair, and
/// whether a baseline of that operating system may select on that key.
struct KeyCase<'a> {
    /// The `OperatingSystem` the create names, or `None` for a create that names none and
    /// is therefore about the default one ("the default value is WINDOWS",
    /// `CreatePatchBaseline$OperatingSystem`).
    operating_system: Option<&'a str>,
    /// The `PatchFilter` key the rule selects on, a member of the modelled `PatchFilterKey`
    /// enum (`smithy_query(query="shape", shape="PatchFilterKey")`).
    key: &'a str,
    /// Whether that operating system's patches carry the property the key names.
    valid: bool,
}

/// The table every suite below reads: one row per `(operating system, key)` pair, with the
/// verdict the published per-operating-system property lists give it.
///
/// The valid properties of each operating system are the ones `DescribePatchProperties`
/// publishes for it (`smithy_query(query="doc", shape="DescribePatchProperties")`:
/// "AMAZON_LINUX_2023 Valid properties: PRODUCT | CLASSIFICATION | SEVERITY", "DEBIAN Valid
/// properties: PRODUCT | PRIORITY", "MACOS Valid properties: PRODUCT | CLASSIFICATION",
/// "WINDOWS Valid properties: PRODUCT | PRODUCT_FAMILY | CLASSIFICATION | MSRC_SEVERITY");
/// `PATCH_SET` is Windows's own ("the available filter keys for WINDOWS are PATCH_SET,
/// PRODUCT, PRODUCT_FAMILY, CLASSIFICATION, and MSRC_SEVERITY",
/// `smithy_query(query="doc", shape="PatchFilter")`) beside the Knowledge Base id
/// `PATCH_ID` names ("applies to Windows patches only", `Patch$KbNumber`), and the package
/// properties — `NAME`, `EPOCH`, `VERSION`, `RELEASE`, `ARCH`, `SECTION`, `SECURITY`,
/// `REPOSITORY`, `ADVISORY_ID`, `CVE_ID`, `BUGZILLA_ID` — each "applies to Linux-based
/// managed nodes only" (`Patch$Name`, `Patch$Epoch`, `Patch$CVEIds`, …).
///
/// The rows span operating-system families the issue cited none of (the Debian-based
/// distributions, macOS, SUSE, Red Hat Enterprise Linux and the default operating system a
/// create that names none is about) as well as the Amazon Linux one it did.
const KEY_CASES: &[KeyCase<'static>] = &[
    // The RPM-based Linux distributions: PRODUCT | CLASSIFICATION | SEVERITY, and the
    // package properties of a Linux patch.
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "PRODUCT", valid: true },
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "CLASSIFICATION", valid: true },
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "SEVERITY", valid: true },
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "CVE_ID", valid: true },
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "ARCH", valid: true },
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "MSRC_SEVERITY", valid: false },
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "PRODUCT_FAMILY", valid: false },
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "PATCH_SET", valid: false },
    KeyCase { operating_system: Some("AMAZON_LINUX_2023"), key: "PATCH_ID", valid: false },
    KeyCase { operating_system: Some("AMAZON_LINUX_2"), key: "ADVISORY_ID", valid: true },
    KeyCase { operating_system: Some("AMAZON_LINUX_2"), key: "MSRC_SEVERITY", valid: false },
    KeyCase { operating_system: Some("REDHAT_ENTERPRISE_LINUX"), key: "BUGZILLA_ID", valid: true },
    KeyCase { operating_system: Some("REDHAT_ENTERPRISE_LINUX"), key: "PRIORITY", valid: false },
    KeyCase { operating_system: Some("SUSE"), key: "SEVERITY", valid: true },
    KeyCase { operating_system: Some("SUSE"), key: "PATCH_ID", valid: false },
    // The Debian-based distributions: PRODUCT | PRIORITY, beside the same package
    // properties — and no classification and no severity at all.
    KeyCase { operating_system: Some("UBUNTU"), key: "PRIORITY", valid: true },
    KeyCase { operating_system: Some("UBUNTU"), key: "SECTION", valid: true },
    KeyCase { operating_system: Some("UBUNTU"), key: "REPOSITORY", valid: true },
    KeyCase { operating_system: Some("UBUNTU"), key: "CLASSIFICATION", valid: false },
    KeyCase { operating_system: Some("DEBIAN"), key: "PRIORITY", valid: true },
    KeyCase { operating_system: Some("DEBIAN"), key: "SEVERITY", valid: false },
    KeyCase { operating_system: Some("DEBIAN"), key: "MSRC_SEVERITY", valid: false },
    // macOS: PRODUCT | CLASSIFICATION, and its software updates are no packages.
    KeyCase { operating_system: Some("MACOS"), key: "CLASSIFICATION", valid: true },
    KeyCase { operating_system: Some("MACOS"), key: "PRIORITY", valid: false },
    KeyCase { operating_system: Some("MACOS"), key: "SEVERITY", valid: false },
    KeyCase { operating_system: Some("MACOS"), key: "NAME", valid: false },
    KeyCase { operating_system: Some("MACOS"), key: "CVE_ID", valid: false },
    // Windows: its four published properties, its patch set and its Knowledge Base id —
    // and none of the properties of a Linux package.
    KeyCase { operating_system: Some("WINDOWS"), key: "PRODUCT_FAMILY", valid: true },
    KeyCase { operating_system: Some("WINDOWS"), key: "MSRC_SEVERITY", valid: true },
    KeyCase { operating_system: Some("WINDOWS"), key: "PATCH_SET", valid: true },
    KeyCase { operating_system: Some("WINDOWS"), key: "PATCH_ID", valid: true },
    KeyCase { operating_system: Some("WINDOWS"), key: "SEVERITY", valid: false },
    KeyCase { operating_system: Some("WINDOWS"), key: "PRIORITY", valid: false },
    KeyCase { operating_system: Some("WINDOWS"), key: "NAME", valid: false },
    KeyCase { operating_system: Some("WINDOWS"), key: "CVE_ID", valid: false },
    KeyCase { operating_system: Some("WINDOWS"), key: "SECTION", valid: false },
    // A create that names no operating system is about the default one, which is Windows,
    // so the same two verdicts hold for it.
    KeyCase { operating_system: None, key: "MSRC_SEVERITY", valid: true },
    KeyCase { operating_system: None, key: "SEVERITY", valid: false },
];

/// A value for `key` to search for. The values a filter carries are not what is under test
/// — "the filter values can be exact values for the patch property given as a key, or a
/// wildcard (*), which matches all values" (`PatchFilter`) — so every row searches for the
/// wildcard and differs only in the key.
const FILTER_VALUE: &str = "*";

/// One approval rule selecting on `key`, carrying the member a rule "must include a value
/// for" (`PatchRule$ApproveAfterDays`).
fn rule_on(key: &str) -> Value {
    json!({
        "PatchFilterGroup": {
            "PatchFilters": [{ "Key": key, "Values": [FILTER_VALUE] }],
        },
        "ApproveAfterDays": 7,
        "ComplianceLevel": "CRITICAL",
    })
}

/// The body of a create naming `name` for `operating_system`, stating `key` in the place
/// `in_global_filters` says: the baseline's own global filters, or the filter group of its
/// one approval rule.
fn create_body(
    name: &str,
    operating_system: Option<&str>,
    key: &str,
    in_global_filters: bool,
) -> Value {
    let mut body = json!({ "Name": name });
    if let Some(operating_system) = operating_system {
        body["OperatingSystem"] = json!(operating_system);
    }
    if in_global_filters {
        body["GlobalFilters"] = json!({
            "PatchFilters": [{ "Key": key, "Values": [FILTER_VALUE] }],
        });
        // A baseline has to approve something beside its global filters, and an approved
        // patch list is the selector that states no filter key of its own.
        body["ApprovedPatches"] = json!(["openssl"]);
    } else {
        body["ApprovalRules"] = json!({ "PatchRules": [rule_on(key)] });
    }
    body
}

/// A name no other row uses, in the form `BaselineName` admits
/// (`^[a-zA-Z0-9_\-.]{3,128}$`).
fn baseline_name(prefix: &str, operating_system: Option<&str>, key: &str) -> String {
    format!(
        "{prefix}.{}.{key}",
        operating_system.unwrap_or("DefaultOperatingSystem")
    )
}

/// The `BaselineIdentities` a listing narrowed to the baselines whose name starts with
/// `name` reports.
async fn identities_named(router: &MultiServiceRouter, name: &str) -> Vec<Value> {
    let (status, response) = call_in_region(
        router,
        REGION,
        "DescribePatchBaselines",
        &json!({ "Filters": [{ "Key": "NAME_PREFIX", "Values": [name] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribePatchBaselines of {name} must succeed, got {status} {response}"
    );
    response["BaselineIdentities"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// A create stating a key the operating system's patches do not carry is refused — with the
/// status, the error class and the sentence Systems Manager words, naming the key the
/// request carried — and leaves no baseline behind; one stating a key they do carry is
/// admitted and the baseline reports the criteria it stated.
#[tokio::test]
async fn a_create_states_criteria_with_the_properties_of_its_own_operating_system() {
    for KeyCase {
        operating_system,
        key,
        valid,
    } in KEY_CASES
    {
        let case = format!(
            "a rule on {key} for {}",
            operating_system.unwrap_or("the default operating system")
        );
        let router = router();
        let name = baseline_name("Rule", *operating_system, key);
        let body = create_body(&name, *operating_system, key, false);
        let (status, response) =
            call_in_region(&router, REGION, "CreatePatchBaseline", &body).await;

        if !valid {
            assert_eq!(
                status, 400,
                "case {case}: {body} must be refused, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case}: expected ValidationException, got {response}"
            );
            assert_eq!(
                error_message(&response),
                unknown_filter_key(key),
                "case {case}: expected the sentence naming the key the request carried, got \
                 {response}"
            );
            assert!(
                response["BaselineId"].is_null(),
                "case {case}: a refused create mints no BaselineId, got {response}"
            );
            let identities = identities_named(&router, &name).await;
            assert!(
                identities.is_empty(),
                "case {case}: a refused create leaves no baseline behind, got {identities:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case}: {body} must be admitted, got {status} {response}"
        );
        let baseline_id = response["BaselineId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: a create reports a BaselineId, got {response}"))
            .to_string();

        // The criteria the create stated are the ones the baseline holds: read back through
        // the implemented reader rather than asserted from the request.
        let (status, reported) = call_in_region(
            &router,
            REGION,
            "GetPatchBaseline",
            &json!({ "BaselineId": baseline_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: the minted baseline must be readable, got {status} {reported}"
        );
        assert_eq!(
            reported["ApprovalRules"]["PatchRules"][0]["PatchFilterGroup"]["PatchFilters"][0]
                ["Key"]
                .as_str(),
            Some(*key),
            "case {case}: the baseline reports the key its rule states, got {reported}"
        );
    }
}

/// The global filters a write states are held to the same rule as its rules' filter groups:
/// "a set of global filters used to include patches in the baseline"
/// (`CreatePatchBaseline$GlobalFilters`) is filters of the same shape, so the same keys are
/// the valid ones there.
#[tokio::test]
async fn the_global_filters_are_held_to_the_same_keys_as_the_rules() {
    for KeyCase {
        operating_system,
        key,
        valid,
    } in KEY_CASES
    {
        let case = format!(
            "a global filter on {key} for {}",
            operating_system.unwrap_or("the default operating system")
        );
        let router = router();
        let name = baseline_name("Global", *operating_system, key);
        let body = create_body(&name, *operating_system, key, true);
        let (status, response) =
            call_in_region(&router, REGION, "CreatePatchBaseline", &body).await;

        if !valid {
            assert_eq!(
                (status, error_code(&response).as_str()),
                (400, "ValidationException"),
                "case {case}: {body} must be refused, got {status} {response}"
            );
            assert_eq!(
                error_message(&response),
                unknown_filter_key(key),
                "case {case}: expected the sentence naming the key the request carried, got \
                 {response}"
            );
            let identities = identities_named(&router, &name).await;
            assert!(
                identities.is_empty(),
                "case {case}: a refused create leaves no baseline behind, got {identities:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case}: {body} must be admitted, got {status} {response}"
        );
        let identities = identities_named(&router, &name).await;
        assert_eq!(
            identities.len(),
            1,
            "case {case}: the admitted baseline is the one the listing reports, got \
             {identities:?}"
        );
    }
}

/// One row of the update table: the operating system the baseline was created for, and the
/// key an update tries to state against it.
struct UpdateCase<'a> {
    case: &'a str,
    /// The operating system the create persisted, which no update member moves.
    operating_system: &'a str,
    /// The key the create states, valid for that operating system.
    created_key: &'a str,
    /// The key the update states.
    updated_key: &'a str,
    /// Whether that operating system's patches carry the property `updated_key` names.
    valid: bool,
}

/// An update is held to the keys of the operating system the baseline applies to — the one
/// its create persisted, since no update member carries an operating system — and a refused
/// update leaves the baseline exactly as it was.
#[tokio::test]
async fn an_update_states_criteria_with_the_properties_of_the_baselines_operating_system() {
    let cases = [
        UpdateCase {
            case: "a Windows key on the Linux baseline that was created with a Linux one",
            operating_system: "AMAZON_LINUX_2023",
            created_key: "CLASSIFICATION",
            updated_key: "MSRC_SEVERITY",
            valid: false,
        },
        UpdateCase {
            case: "another property the same distribution publishes",
            operating_system: "AMAZON_LINUX_2023",
            created_key: "CLASSIFICATION",
            updated_key: "SEVERITY",
            valid: true,
        },
        UpdateCase {
            case: "a classification on a Debian-based distribution",
            operating_system: "UBUNTU",
            created_key: "PRIORITY",
            updated_key: "CLASSIFICATION",
            valid: false,
        },
        UpdateCase {
            case: "a package property on a Debian-based distribution",
            operating_system: "UBUNTU",
            created_key: "PRIORITY",
            updated_key: "SECTION",
            valid: true,
        },
        UpdateCase {
            case: "a Linux package property on a Windows baseline",
            operating_system: "WINDOWS",
            created_key: "CLASSIFICATION",
            updated_key: "ARCH",
            valid: false,
        },
        UpdateCase {
            case: "the patch set of a Windows baseline",
            operating_system: "WINDOWS",
            created_key: "CLASSIFICATION",
            updated_key: "PATCH_SET",
            valid: true,
        },
        UpdateCase {
            case: "a severity on a macOS baseline",
            operating_system: "MACOS",
            created_key: "CLASSIFICATION",
            updated_key: "SEVERITY",
            valid: false,
        },
    ];

    for UpdateCase {
        case,
        operating_system,
        created_key,
        updated_key,
        valid,
    } in &cases
    {
        let router = router();
        let name = baseline_name("Update", Some(operating_system), updated_key);
        let (status, created) = call_in_region(
            &router,
            REGION,
            "CreatePatchBaseline",
            &create_body(&name, Some(operating_system), created_key, false),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: the baseline to update must be created, got {status} {created}"
        );
        let baseline_id = created["BaselineId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: a create reports a BaselineId, got {created}"))
            .to_string();

        let (status, response) = call_in_region(
            &router,
            REGION,
            "UpdatePatchBaseline",
            &json!({
                "BaselineId": baseline_id,
                "ApprovalRules": { "PatchRules": [rule_on(updated_key)] },
            }),
        )
        .await;

        if *valid {
            assert_eq!(
                status, 200,
                "case {case}: the update must be admitted, got {status} {response}"
            );
            assert_eq!(
                response["ApprovalRules"]["PatchRules"][0]["PatchFilterGroup"]["PatchFilters"][0]
                    ["Key"]
                    .as_str(),
                Some(*updated_key),
                "case {case}: the update reports the key it stated, got {response}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&response).as_str()),
            (400, "ValidationException"),
            "case {case}: the update must be refused, got {status} {response}"
        );
        assert_eq!(
            error_message(&response),
            unknown_filter_key(updated_key),
            "case {case}: expected the sentence naming the key the update carried, got {response}"
        );

        // A refused update writes nothing: the baseline still states the criteria its
        // create persisted.
        let (status, reported) = call_in_region(
            &router,
            REGION,
            "GetPatchBaseline",
            &json!({ "BaselineId": baseline_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: the baseline must still be readable, got {status} {reported}"
        );
        assert_eq!(
            reported["ApprovalRules"]["PatchRules"][0]["PatchFilterGroup"]["PatchFilters"][0]
                ["Key"]
                .as_str(),
            Some(*created_key),
            "case {case}: a refused update leaves the baseline's criteria as they were, got \
             {reported}"
        );
    }
}
