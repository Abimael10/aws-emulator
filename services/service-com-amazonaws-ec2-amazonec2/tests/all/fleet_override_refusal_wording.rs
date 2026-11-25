// SPDX-License-Identifier: CC0-1.0

//! How each FLEET API words its refusal of a launch template override that says what its
//! pool launches TWICE.
//!
//! The rule under test: an override carrying both `InstanceRequirements` and
//! `InstanceType` describes its pool twice — "if you specify `InstanceRequirements`, you
//! can't specify `InstanceType`" (`LaunchTemplateOverrides$InstanceRequirements`, and the
//! same note on `FleetLaunchTemplateOverridesRequest$InstanceRequirements`) — and is
//! refused HTTP 400 with the error code AND the sentence the API it was sent to answers
//! that refusal with. Both are facts of the API, recorded per API: the Spot Fleet surface
//! answers `InvalidParameterCombinationException` and words the second mention of the
//! instance-type member as two words, while the EC2 Fleet surface answers EC2's own
//! `InvalidParameterValue` and words it as the member name. The sentences are spelled out
//! below rather than read from the service, so a change to either one is visible here.
//!
//! The rows differ along what the rule generalises over: which API the override was sent
//! to, where in the overrides list the offending override sits, which instance type it
//! names, and which requirement members it carries — and every row is checked to have
//! stored nothing, because a refused fleet request places no capacity.

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The ARN of the IAM role a Spot Fleet request grants the fleet.
const FLEET_ROLE: &str = "arn:aws:iam::111122223333:role/aws-ec2-spot-fleet-tagging-role";

/// The SPOT FLEET surface's recorded sentence, with the second mention of the
/// instance-type member as the two words "Instance Type".
const SPOT_FLEET_SENTENCE: &str = "You can specify either InstanceRequirements or InstanceType, \
     but not both. Remove either InstanceRequirements or Instance Type, and try again.";

/// The EC2 FLEET surface's recorded sentence, which spells that member as the API spells
/// it.
const CREATE_FLEET_SENTENCE: &str = "You can specify either InstanceRequirements or InstanceType, \
     but not both. Remove either InstanceRequirements or InstanceType, and try again.";

/// One request's form parameters.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// An AMI of the published catalog a fresh account already sees, read out of the service's
/// own state: a launch template must name an image the account can resolve.
fn image_id() -> String {
    Ec2State::default()
        .images
        .keys()
        .next()
        .cloned()
        .expect("a fresh account must publish at least one image")
}

/// Create the launch template every row launches from, and answer its id.
async fn launch_template(case: &str, router: &MultiServiceRouter) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateLaunchTemplate",
        &[
            p("LaunchTemplateName", "override-wording"),
            p("LaunchTemplateData.ImageId", image_id()),
            p("LaunchTemplateData.InstanceType", "m5.xlarge"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate must succeed, got {status}: {body}"
    );
    element(&body, "launchTemplateId")
        .unwrap_or_else(|| panic!("case {case:?}: no launchTemplateId in {body}"))
}

/// Which fleet API a row sends its overrides to.
#[derive(Debug, Clone, Copy)]
enum Api {
    SpotFleet,
    Fleet,
}

impl Api {
    fn action(self) -> &'static str {
        match self {
            Api::SpotFleet => "RequestSpotFleet",
            Api::Fleet => "CreateFleet",
        }
    }

    /// Everything that makes a request of this API valid apart from its overrides.
    fn base(self, template_id: &str) -> Params {
        match self {
            Api::SpotFleet => vec![
                p("SpotFleetRequestConfig.IamFleetRole", FLEET_ROLE),
                p("SpotFleetRequestConfig.TargetCapacity", "1"),
                p(
                    "SpotFleetRequestConfig.LaunchTemplateConfigs.1.\
                     LaunchTemplateSpecification.LaunchTemplateId",
                    template_id,
                ),
            ],
            Api::Fleet => vec![
                p("TargetCapacitySpecification.TotalTargetCapacity", "1"),
                p(
                    "LaunchTemplateConfigs.1.LaunchTemplateSpecification.LaunchTemplateId",
                    template_id,
                ),
            ],
        }
    }

    /// The parameter prefix of the `position`th override of this API's first launch
    /// template config.
    fn override_prefix(self, position: usize) -> String {
        match self {
            Api::SpotFleet => {
                format!("SpotFleetRequestConfig.LaunchTemplateConfigs.1.Overrides.{position}")
            }
            Api::Fleet => format!("LaunchTemplateConfigs.1.Overrides.{position}"),
        }
    }

    /// The error code this API answers the refusal with.
    fn code(self) -> &'static str {
        match self {
            Api::SpotFleet => "InvalidParameterCombinationException",
            Api::Fleet => "InvalidParameterValue",
        }
    }

    /// The sentence this API words the refusal with.
    fn sentence(self) -> &'static str {
        match self {
            Api::SpotFleet => SPOT_FLEET_SENTENCE,
            Api::Fleet => CREATE_FLEET_SENTENCE,
        }
    }
}

/// One row: the API, which override of the list carries both members, the instance type it
/// names, and the requirement members beside it.
struct Row {
    case: &'static str,
    api: Api,
    /// How many overrides the request carries; the LAST of them carries both members and
    /// every earlier one names only an instance type.
    overrides: usize,
    instance_type: &'static str,
    requirements: &'static [(&'static str, &'static str)],
}

/// The modelled requirement members the rows carry (`smithy_query(query="shape",
/// shape="VCpuCountRange,MemoryMiB")`: `InstanceRequirementsRequest` makes `VCpuCount` and
/// `MemoryMiB` required, so an EC2 Fleet row names both).
const VCPU_AND_MEMORY: &[(&str, &str)] = &[("VCpuCount.Min", "2"), ("MemoryMiB.Min", "1024")];
const VCPU_RANGE_AND_MEMORY: &[(&str, &str)] = &[
    ("VCpuCount.Min", "4"),
    ("VCpuCount.Max", "8"),
    ("MemoryMiB.Min", "2048"),
];

const ROWS: &[Row] = &[
    Row {
        case: "the only Spot Fleet override carries both members",
        api: Api::SpotFleet,
        overrides: 1,
        instance_type: "t3.micro",
        requirements: VCPU_AND_MEMORY,
    },
    Row {
        case: "the third Spot Fleet override carries both members",
        api: Api::SpotFleet,
        overrides: 3,
        instance_type: "c6g.medium",
        requirements: VCPU_RANGE_AND_MEMORY,
    },
    Row {
        case: "the only EC2 Fleet override carries both members",
        api: Api::Fleet,
        overrides: 1,
        instance_type: "t4g.small",
        requirements: VCPU_RANGE_AND_MEMORY,
    },
    Row {
        case: "the second EC2 Fleet override carries both members",
        api: Api::Fleet,
        overrides: 2,
        instance_type: "m6i.large",
        requirements: VCPU_AND_MEMORY,
    },
];

/// How much this account holds of everything a fleet request writes.
fn store_counts(engine: &engine::InMemoryEngine) -> (usize, usize, usize, usize) {
    let state = engine.state::<Ec2State>();
    let store = state.lock();
    (
        store.fleets.len(),
        store.spot_fleet_requests.len(),
        store.spot_instance_requests.len(),
        store.instances.len(),
    )
}

#[tokio::test]
async fn each_fleet_api_words_the_double_pool_refusal_its_own_way() {
    for row in ROWS {
        let case = row.case;
        // One emulator per row, so what a row stored (or did not) is read against an
        // account only that row wrote to.
        let (router, engine) = super::ec2_query::emulator();
        let template_id = launch_template(case, &router).await;

        let mut params = row.api.base(&template_id);
        for position in 1..=row.overrides {
            let prefix = row.api.override_prefix(position);
            params.push(p(
                &format!("{prefix}.InstanceType"),
                if position == row.overrides {
                    row.instance_type
                } else {
                    "c5.large"
                },
            ));
            if position == row.overrides {
                for (member, value) in row.requirements {
                    params.push(p(&format!("{prefix}.InstanceRequirements.{member}"), *value));
                }
            }
        }

        let before = store_counts(&engine);
        let (status, body) = call(&router, REGION, row.api.action(), &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from {}, got {status}: {body}",
            row.api.action()
        );
        let code = error_code(&body);
        let expected_code = row.api.code();
        assert_eq!(
            code, expected_code,
            "case {case:?}: expected the error code {expected_code:?}, got {code:?} in {body}"
        );
        let message = error_message(&body);
        let expected = row.api.sentence();
        assert_eq!(
            message, expected,
            "case {case:?}: expected the sentence {expected:?}, got {message:?}"
        );
        let after = store_counts(&engine);
        assert_eq!(
            after, before,
            "case {case:?}: a refused fleet request stores nothing, got {after:?}"
        );
    }
}

#[tokio::test]
async fn the_two_surfaces_word_the_same_refusal_differently() {
    // The two sentences differ in exactly one place — the second mention of the
    // instance-type member — so neither surface may be served the other's wording.
    assert_ne!(
        SPOT_FLEET_SENTENCE, CREATE_FLEET_SENTENCE,
        "the recorded sentences of the two surfaces differ"
    );
    for (case, api, forbidden) in [
        ("RequestSpotFleet", Api::SpotFleet, CREATE_FLEET_SENTENCE),
        ("CreateFleet", Api::Fleet, SPOT_FLEET_SENTENCE),
    ] {
        let (router, _engine) = super::ec2_query::emulator();
        let template_id = launch_template(case, &router).await;
        let mut params = api.base(&template_id);
        let prefix = api.override_prefix(1);
        params.push(p(&format!("{prefix}.InstanceType"), "t3.micro"));
        for (member, value) in VCPU_AND_MEMORY {
            params.push(p(&format!("{prefix}.InstanceRequirements.{member}"), *value));
        }
        let (status, body) = call(&router, REGION, api.action(), &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status}: {body}"
        );
        let message = error_message(&body);
        assert_ne!(
            message, forbidden,
            "case {case:?}: expected this surface's own sentence, got the other surface's \
             {message:?}"
        );
        assert_eq!(
            message,
            api.sentence(),
            "case {case:?}: expected {:?}, got {message:?}",
            api.sentence()
        );
    }
}
