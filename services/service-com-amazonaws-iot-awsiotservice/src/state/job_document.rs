// SPDX-License-Identifier: CC0-1.0

//! How a job document's SUBSTITUTION PLACEHOLDERS are resolved into the document a
//! device is handed and a reader reports post-substitution.
//!
//! A job document carries placeholders of two kinds, and the service resolves BOTH when
//! it serves the resolved view:
//!
//! * `${aws:iot:parameter:<key>}` — resolved from the job's own `documentParameters`,
//!   "the values the job document's substitution parameters resolve to"
//!   (`CreateJobRequest$documentParameters`).
//! * `${aws:iot:package:<packageName>:version:<versionName>:attributes:<attributeKey>}`
//!   — resolved from the ATTRIBUTES of a package version the job is associated with
//!   through `destinationPackageVersions`, "the package version Amazon Resource Names
//!   (ARNs) that are installed on the device when the job successfully completes"
//!   (`smithy_query(query="doc",shape="CreateJobRequest",
//!   member="destinationPackageVersions")`). A version's `attributes` are "metadata that
//!   can be used to define a package version's configuration. For example, the S3 file
//!   location, configuration options that are being sent to the device or fleet"
//!   (`CreatePackageVersionRequest$attributes`) — which is exactly what such a
//!   placeholder stands for.
//!
//! The association is what makes a placeholder resolvable: only the versions the job
//! NAMES are consulted, so a placeholder naming a version the job is not associated
//! with — or an attribute the named version does not carry — is reported as written,
//! because nothing here knows what it resolves to. Both kinds resolve in ONE pass over
//! the document in this module, so every server of the resolved view (the reader of a
//! job's document and the copy a job's executions hand the device) resolves the same
//! placeholders the same way.
//!
//! The document BEFORE substitution is the stored one, which is what a reader asked for
//! the pre-substitution view reports ([`super::RegisteredJob::document`]).

use super::*;

/// How AWS IoT's job-document substitution writes a parameter into a job document:
/// the parameter's key inside `${aws:iot:parameter:…}`.
const JOB_DOCUMENT_PARAMETER_OPEN: &str = "${aws:iot:parameter:";

/// How AWS IoT's Software Package Catalog writes a package version's ATTRIBUTE into a
/// job document: the package name, the version name and the attribute key inside
/// `${aws:iot:package:…}`, as
/// [`package_attribute_placeholder_key`] lays the three out.
const JOB_DOCUMENT_PACKAGE_OPEN: &str = "${aws:iot:package:";

/// The text a package-attribute placeholder carries between
/// [`JOB_DOCUMENT_PACKAGE_OPEN`] and its closing brace:
/// `<packageName>:version:<versionName>:attributes:<attributeKey>`.
///
/// It is the KEY one attribute of one package version is addressed by in a job document,
/// so building the bindings and scanning the document use this one spelling and cannot
/// disagree about it.
pub fn package_attribute_placeholder_key(
    package_name: &str,
    version_name: &str,
    attribute_key: &str,
) -> String {
    format!("{package_name}:version:{version_name}:attributes:{attribute_key}")
}

/// Every package-attribute placeholder a job's PACKAGE ASSOCIATION resolves, keyed by
/// the text [`package_attribute_placeholder_key`] spells.
///
/// `destination_package_versions` are package version ARNs, so the package and version
/// each one names are read out of the ARN
/// ([`::service_common::iot_jobs::package_version_from_arn`]) and looked up in the
/// catalog `packages` holds: every attribute of every version the job names becomes one
/// binding. An ARN that is not a package version ARN, and one naming a package or a
/// version the catalog does not hold, contributes nothing — there is no attribute value
/// to substitute — so a placeholder for it is left as written.
///
/// The bindings are bounded by the association itself: a job names at most
/// [`MAX_DESTINATION_PACKAGE_VERSIONS`] versions, and "the combined size of all the
/// attributes on a package version is limited to 3KB"
/// (`CreatePackageVersionRequest$attributes`).
pub fn job_package_attributes(
    packages: &BTreeMap<String, RegisteredPackage>,
    destination_package_versions: &[String],
) -> BTreeMap<String, String> {
    let mut bindings = BTreeMap::new();
    for arn in destination_package_versions {
        let Some((package_name, version_name)) =
            ::service_common::iot_jobs::package_version_from_arn(arn)
        else {
            continue;
        };
        let Some(version) = packages
            .get(package_name)
            .and_then(|package| package.versions.get(version_name))
        else {
            continue;
        };
        for (attribute_key, value) in &version.attributes {
            bindings.insert(
                package_attribute_placeholder_key(package_name, version_name, attribute_key),
                value.clone(),
            );
        }
    }
    bindings
}

/// Resolve a job document's substitution placeholders with the values the job's own
/// parameters and its package association supply.
///
/// Every `${aws:iot:parameter:<key>}` whose key `parameters` has a value for, and every
/// `${aws:iot:package:…:attributes:<key>}` whose attribute `package_attributes` binds
/// ([`job_package_attributes`]), is replaced by that value; a placeholder nothing binds
/// is left exactly as written, because nothing here knows what it resolves to. The
/// result is what a reader reports for the resolved view of the document, while the
/// stored document itself is the view before substitution.
///
/// `Err` carries the length in bytes the resolved document reached, for a document whose
/// placeholders would expand it past [`MAX_JOB_DOCUMENT_BYTES`]: the growth is checked as
/// the document is built, so one value cannot make a reader allocate without bound.
pub fn resolve_job_document(
    document: &str,
    parameters: &BTreeMap<String, String>,
    package_attributes: &BTreeMap<String, String>,
) -> Result<String, usize> {
    // Each placeholder FORM with the values it resolves from. A form whose values are
    // empty resolves nothing, so a document carrying only those is reported as written.
    let forms: [(&str, &BTreeMap<String, String>); 2] = [
        (JOB_DOCUMENT_PARAMETER_OPEN, parameters),
        (JOB_DOCUMENT_PACKAGE_OPEN, package_attributes),
    ];
    if forms.iter().all(|(_, values)| values.is_empty()) {
        return Ok(document.to_string());
    }
    let mut resolved = String::with_capacity(document.len());
    let mut rest = document;
    // One pass over the document, always taking the placeholder that comes FIRST
    // whichever form it is written in, so a document mixing the two resolves both in
    // document order and a value a substitution writes is never rescanned.
    while let Some((open, opener, values)) = forms
        .iter()
        .filter(|(_, values)| !values.is_empty())
        .filter_map(|(opener, values)| rest.find(opener).map(|open| (open, *opener, *values)))
        .min_by_key(|(open, _, _)| *open)
    {
        let (before, placeholder) = rest.split_at(open);
        let key_onwards = &placeholder[opener.len()..];
        let Some(close) = key_onwards.find('}') else {
            // No closing brace: the rest of the document carries no placeholder of this
            // form at all, and none of another form can close either, so it is reported
            // as written.
            break;
        };
        resolved.push_str(before);
        match values.get(&key_onwards[..close]) {
            Some(value) => resolved.push_str(value),
            None => resolved.push_str(&placeholder[..opener.len() + close + 1]),
        }
        if resolved.len() > MAX_JOB_DOCUMENT_BYTES {
            return Err(resolved.len());
        }
        rest = &key_onwards[close + 1..];
    }
    resolved.push_str(rest);
    if resolved.len() > MAX_JOB_DOCUMENT_BYTES {
        return Err(resolved.len());
    }
    Ok(resolved)
}
