// SPDX-License-Identifier: CC0-1.0

//! AWS IAM Identity Center's published default-quota catalog, under the `sso`
//! `ServiceCode`.

use super::*;

/// AWS IAM Identity Center's published default-quota catalog.
///
/// SOURCE: AWS's published Service Quotas data for the `sso` service code (what `aws
/// service-quotas list-aws-default-service-quotas --service-code sso` reports, which is
/// the "Quotas for IAM Identity Center" page of its user guide), and the `QuotaCode`s
/// that catalog is keyed on.
///
/// The service's quotas are the planes of ONE instance of IAM Identity Center — the
/// permission sets it holds, the policies one permission set may carry, and the
/// identities and applications the instance serves — so every row names the plane it
/// counts rather than the account.
///
/// None of them is a GLOBAL quota: an IAM Identity Center instance belongs to the Region
/// it was enabled in, so the planes of that instance are counted per Region, which is what
/// `GlobalQuota` reports for every row here.
///
/// The recorded listing states the quota published under `L-2FDFC908` (the groups that may
/// be assigned to a permission set per account or to an application, 100) and under
/// `L-D519A297` (the Regions IAM Identity Center may be enabled in, 6). The managed-policy
/// and trusted-token-issuer counts this catalog used to key on those two codes are NOT
/// published here: no source available here states AWS's code for them, so they are left
/// to the default-quota-set gap rather than served under another quota's code. A row whose
/// published `Description` no source available here states carries none, rather than a
/// sentence this emulator words itself.
pub(in crate::state) fn sso_quotas() -> Vec<QuotaRow> {
    vec![
        quota("Applications per instance", 50.0)
            .code("L-0299121C")
            .describe(
                "The maximum number of applications that you can configure in an IAM \
                 Identity Center instance.",
            ),
        quota(
            "Total number of groups that can be assigned to a permission set per AWS \
             account, or to an application",
            100.0,
        )
        .code("L-2FDFC908")
        .fixed(),
        quota("Customer managed policies per permission set", 20.0)
            .code("L-3C8D41A0")
            .fixed()
            .describe(
                "The maximum number of customer managed policies that you can attach to a \
                 permission set.",
            ),
        quota("Groups per instance", 100_000.0)
            .code("L-7E1D4E33")
            .describe(
                "The maximum number of groups in the identity store of an IAM Identity \
                 Center instance.",
            ),
        quota("Inline policy size per permission set", 32_768.0)
            .code("L-89954265")
            .unit("Bytes")
            .fixed()
            .describe(
                "The maximum size, in bytes, of the inline policy of a permission set.",
            ),
        quota("Permission sets", 500.0)
            .code("L-B44C7A29")
            .describe(
                "The maximum number of permission sets in an IAM Identity Center instance.",
            ),
        quota("Permission sets per AWS account", 50.0)
            .code("L-C4FCA052")
            .describe(
                "The maximum number of permission sets that you can provision in one AWS \
                 account.",
            ),
        quota("Total number of regions for IAM Identity Center", 6.0).code("L-D519A297"),
    ]
}
