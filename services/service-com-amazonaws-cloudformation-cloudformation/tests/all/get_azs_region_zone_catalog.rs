// SPDX-License-Identifier: CC0-1.0

//! `Fn::GetAZs` lists the Availability Zones the Region really has, end to end over the
//! awsQuery wire.
//!
//! The rule under test: "the intrinsic function Fn::GetAZs returns an array that lists
//! Availability Zones for a specified Region" — so the array a stack resolves is the zones
//! of that Region, which is the SAME set Amazon EC2's `DescribeAvailabilityZones` reports
//! for it, in the same order. A Region's zones are not three letters counted off the
//! alphabet: several Regions expose more than three, and some expose letters that are not
//! the first three.
//!
//! Nothing here hard-codes a zone list. Each row deploys an outputs-only stack in one
//! Region and compares what `Fn::GetAZs` resolved against what EC2 answers over its own
//! wire for that Region, so the two services are held to one catalogue rather than to a
//! constant this suite keeps.
//!
//! The rows span what the rule generalises over: a Region with more zones than the common
//! three (`us-east-1`), another with a different count again (`us-west-2`), one whose zone
//! letters are not contiguous (`ap-northeast-1`), and one that exposes the common three
//! (`eu-west-2`). Each row also resolves the zones of ANOTHER Region, through both forms of
//! the argument the function takes — the empty string and the Region pseudo parameter for
//! the stack's own Region, a Region name for a foreign one.
//!
//! The wire harness is the crate's own.

use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call_in, member, router, settled_stack, xml_text, xml_texts};
use crate::stack_lifecycle::output_value;

/// The Regions these rows deploy in, each paired with the FOREIGN Region whose zones its
/// stack also resolves: every row names a Region other than its own, so a row cannot pass
/// by reading the zones of the Region it was signed for twice.
const ROWS: &[(&str, &str)] = &[
    ("us-east-1", "us-west-2"),
    ("us-west-2", "ap-northeast-1"),
    ("ap-northeast-1", "eu-west-2"),
    ("eu-west-2", "us-east-1"),
];

/// An outputs-only template that resolves the zones of the stack's own Region through both
/// forms of the argument that names it, and the zones of `foreign` by name.
fn template(foreign: &str) -> String {
    format!(
        "Outputs:\n  \
         LocalZones:\n    Value: !Join [\",\", !GetAZs \"\"]\n  \
         RegionRefZones:\n    Value: !Join [\",\", !GetAZs {{Ref: \"AWS::Region\"}}]\n  \
         ForeignZones:\n    Value: !Join [\",\", !GetAZs \"{foreign}\"]\n"
    )
}

/// The zone NAMES `DescribeAvailabilityZones` reports for `region`, in the order it reports
/// them, read through Amazon EC2's own `ec2Query` wire on the same router.
///
/// EC2 is regional, so the Region asked about is the one the request is signed for; the
/// answer carries each zone's name as `<zoneName>`.
async fn ec2_zone_names(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/ec2/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from_static(
            b"Action=DescribeAvailabilityZones&Version=2016-11-15",
        ))
        .expect("a well-formed EC2 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {region}: DescribeAvailabilityZones must answer 200, got {status} with {answered}"
    );
    let zones = xml_texts(&answered, "zoneName");
    assert!(
        !zones.is_empty(),
        "case {region}: DescribeAvailabilityZones reported no zone at all: {answered}"
    );
    for zone in &zones {
        assert!(
            zone.starts_with(region),
            "case {region}: expected every reported zone to be of that Region, got {zone:?}"
        );
    }
    zones
}

/// RULE: the array `Fn::GetAZs` resolves for a Region is the zones Amazon EC2 reports for
/// it — whichever Region, and whichever of the three forms the argument takes.
#[tokio::test]
async fn get_azs_resolves_the_regions_own_zones() {
    let router = router();

    // Every stack is created first and read afterwards: a stack's terminal status is due at
    // an instant, so the rows settle alongside each other rather than one after another.
    for (index, (region, foreign)) in ROWS.iter().enumerate() {
        let stack = format!("getazs-zones-{index}");
        let (code, created) = call_in(
            &router,
            region,
            "CreateStack",
            &[
                member("StackName", &stack),
                member("TemplateBody", &template(foreign)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {region}: CreateStack must answer 200, got {code} with {created}"
        );
    }

    for (index, (region, foreign)) in ROWS.iter().enumerate() {
        let case = format!("the zones of {region}, with {foreign} named beside them");
        let stack = format!("getazs-zones-{index}");
        let described = settled_stack(&case, &router, region, &stack).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            reported, "CREATE_COMPLETE",
            "case {case}: expected the stack to deploy, got {reported:?} with {described}"
        );

        let local_expected = ec2_zone_names(&router, region).await.join(",");
        let foreign_expected = ec2_zone_names(&router, foreign).await.join(",");

        for (key, expected) in [
            ("LocalZones", &local_expected),
            ("RegionRefZones", &local_expected),
            ("ForeignZones", &foreign_expected),
        ] {
            let actual = output_value(&described, key).unwrap_or_default();
            assert_eq!(
                &actual, expected,
                "case {case}: expected {key} to be the zones EC2 reports, \
                 {expected:?}, got {actual:?}"
            );
        }
    }
}
