// SPDX-License-Identifier: CC0-1.0

//! The Amazon ECR registries AWS ITSELF publishes SageMaker's containers from, by Region.
//!
//! An inference or training container a request names is either the caller's own — pushed to
//! the caller's registry — or one of AWS's: `ModelPackageContainerDefinition$Image` is
//! documented for both, "If you are using your own custom algorithm instead of an algorithm
//! provided by SageMaker, the inference code must meet SageMaker requirements"
//! (`smithy_query(query="doc",shape="ModelPackageContainerDefinition",member="Image")`), and an
//! algorithm provided by SageMaker is published from an AWS-owned account, never from the
//! caller's. Those accounts' repositories carry resource-based policies that let SageMaker pull
//! them for any account, which is why a registration naming one is served for every caller.
//!
//! What makes such a registry addressable is the pair of values its host carries: AWS publishes
//! its containers REGION by REGION, one registry account per Region per family of image, and
//! only the account published for a Region holds that Region's repositories. So the question
//! this module answers is about a (Region, account) pair rather than about an account alone: a
//! path naming an AWS registry account of one Region inside another Region's host addresses no
//! registry AWS publishes from, which is a pull SageMaker cannot make.
//!
//! The pairs below are AWS's published ones, from the SageMaker Developer Guide's registry-path
//! tables ("Docker Registry Paths and Example Code", which gives one registry account per
//! Region for each family, and the Deep Learning Containers' own registry table). Three
//! families of image are published this way, carried by up to four accounts per Region:
//!
//! * the built-in algorithm images, which AWS publishes from two registries per Region (the
//!   algorithms are split across them);
//! * the frameworks SageMaker builds itself (`sagemaker-xgboost`, `sagemaker-scikit-learn`,
//!   `sagemaker-sparkml-serving`);
//! * the Deep Learning Containers (`pytorch-inference`, `tensorflow-inference`,
//!   `huggingface-pytorch-inference` and the rest).
//!
//! A Region AWS publishes none of them in carries no entry, and a reference into such a Region
//! is left to the only registry this emulator serves there: the caller's own.

/// Every Amazon ECR registry account AWS publishes SageMaker containers from, by the Region it
/// publishes them in.
///
/// Read through [`aws_publishes_from`]; a Region appears once, with its accounts deduplicated
/// (two families of a Region are published from one account where AWS lists the same id twice).
const REGISTRIES_BY_REGION: &[(&str, &[&str])] = &[
    ("af-south-1", &["455444449433", "510948584623", "626614931356"]),
    ("ap-east-1", &["286214385809", "651117190479", "871362719292"]),
    (
        "ap-northeast-1",
        &[
            "501404015308",
            "351501993468",
            "354813040037",
            "763104351884",
        ],
    ),
    (
        "ap-northeast-2",
        &[
            "306986355934",
            "835164637446",
            "366743142698",
            "763104351884",
        ],
    ),
    ("ap-northeast-3", &["867004704886", "364406365360"]),
    ("ap-south-1", &["991648021394", "720646828776", "763104351884"]),
    (
        "ap-southeast-1",
        &["475088953585", "121021644041", "763104351884"],
    ),
    (
        "ap-southeast-2",
        &[
            "544295431143",
            "712309505854",
            "783357654285",
            "763104351884",
        ],
    ),
    ("ap-southeast-3", &["951798379941", "907027046896"]),
    ("ca-central-1", &["469771592824", "341280168497", "763104351884"]),
    ("cn-north-1", &["390948362332", "450853457545", "727897471807"]),
    (
        "cn-northwest-1",
        &["387376663083", "451049120500", "727897471807"],
    ),
    (
        "eu-central-1",
        &[
            "813361260812",
            "664544806723",
            "492215442770",
            "763104351884",
        ],
    ),
    ("eu-north-1", &["669576153137", "662702820516", "763104351884"]),
    ("eu-south-1", &["257386234256", "978288397137", "692866216735"]),
    (
        "eu-west-1",
        &[
            "685385470294",
            "438346466558",
            "141502667606",
            "763104351884",
        ],
    ),
    ("eu-west-2", &["644912444149", "764974769150", "763104351884"]),
    ("eu-west-3", &["749696950732", "659782779980", "763104351884"]),
    ("il-central-1", &["898809789911", "780543022126"]),
    ("me-central-1", &["272398656194", "914824155844"]),
    ("me-south-1", &["249704162688", "801668240914", "217643126080"]),
    ("sa-east-1", &["855470959533", "737474898029", "763104351884"]),
    (
        "us-east-1",
        &[
            "811284229777",
            "382416733822",
            "683313688378",
            "763104351884",
        ],
    ),
    (
        "us-east-2",
        &[
            "825641698319",
            "404615174143",
            "257758044811",
            "763104351884",
        ],
    ),
    ("us-gov-east-1", &["237065988967", "446045086412"]),
    ("us-gov-west-1", &["226302683700", "414596584902", "442386744353"]),
    ("us-west-1", &["632365934929", "746614075791", "763104351884"]),
    (
        "us-west-2",
        &[
            "433757028032",
            "174872318107",
            "246618743249",
            "763104351884",
        ],
    ),
];

/// Whether AWS publishes SageMaker containers from the Amazon ECR registry of account
/// `registry_id` IN `region` — the one question a reference into a registry no account here
/// owns is admitted on.
///
/// Both values come out of the reference's own host, so the pair is the one the path addressed:
/// an account AWS publishes from in one Region, written into another Region's host, addresses
/// no registry AWS publishes from and is not one this answers for.
#[must_use]
pub fn aws_publishes_from(region: &str, registry_id: &str) -> bool {
    REGISTRIES_BY_REGION
        .iter()
        .find(|(published_in, _)| *published_in == region)
        .is_some_and(|(_, accounts)| accounts.contains(&registry_id))
}
