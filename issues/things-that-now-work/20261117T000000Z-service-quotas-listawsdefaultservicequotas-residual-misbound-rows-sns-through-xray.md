Issue-Source: fix-residue
Issue-Trace: service-quotas-catalog-list-aws-default-service-quotas-sns
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: service-quotas-catalog-list-aws-default-service-quotas-sns service-quotas-catalog-list-aws-default-service-quotas-sqs service-quotas-catalog-list-aws-default-service-quotas-sso service-quotas-catalog-list-aws-default-service-quotas-storagegateway service-quotas-catalog-list-aws-default-service-quotas-timestream service-quotas-catalog-list-aws-default-service-quotas-vpc service-quotas-catalog-list-aws-default-service-quotas-vpc-lattice service-quotas-catalog-list-aws-default-service-quotas-wafv2 service-quotas-catalog-list-aws-default-service-quotas-workspaces service-quotas-catalog-list-aws-default-service-quotas-xray
Issue-Shape-Id: com.amazonaws.servicequotas#ServiceQuotasV20190624

# ListAWSDefaultServiceQuotas still misbinds the rows no recorded value is quoted for, for sns, sqs, sso, storagegateway, timestream, vpc, vpc-lattice, wafv2, workspaces and xray

## Operation
ListAWSDefaultServiceQuotas

## Description
RESIDUE of the fix for
`20261109T154349Z-service-quotas-listawsdefaultservicequotas-misbinds-quota-metadata-for-sns-sqs-sso.md`
(now under `issues/things-that-now-work/`). That report bundled ten `ServiceCode`s. In every one
of them, each row whose AWS-published facts the report QUOTES is now published under the
`QuotaCode` the recording binds it to, and each quota those codes used to carry is published
under no code at all. What is left is the rows the report lists without quoting their recorded
values: the emulator answers HTTP 200 and the row under those codes still carries a `QuotaName`,
`Value`, `Unit`, `Adjustable`, `Period`, `UsageMetric` or `Description` of another quota, or a
`Description` worded by the emulator rather than by the service.

THE RULE (unchanged, and the one the fix applied to the quoted rows). For a given `ServiceCode`,
the row published under a `QuotaCode` carries the `QuotaName`, `Value`, `Unit`, `Adjustable`,
`GlobalQuota`, `Period`, `UsageMetric`, `QuotaContext` and `Description` of the quota the service
publishes UNDER THAT CODE — the text as the service words it, never a paraphrase and never a
neighbouring quota's — and a quota whose own code no source states is published under no code at
all. Smithy fixes the row SHAPE (`ServiceQuota`: `QuotaCode` @length 1..128, `QuotaName`, `Value`
@range 0..10000000000, `Unit`, `Adjustable`, `GlobalQuota`, `UsageMetric`, `Period`,
`QuotaContext`, `Description` @length 0..350); the per-code CONTENT is recorded behaviour, so the
recorded `ListAWSDefaultServiceQuotas` response of each `ServiceCode` is the reference.

WHAT A FIXER NEEDS, AND WHY THIS IS FILED SEPARATELY. The remaining rows cannot be authored from
anything in this checkout: the replay tool reports these clusters as body divergences without
disclosing the recorded values, and the original report quotes AWS's values only for the rows
already fixed. Closing this needs the recorded rows themselves (the `QuotaName`, `Value`, `Unit`,
`Adjustable`, `Period`, `UsageMetric`, `QuotaContext` and `Description` each of the codes below
publishes). Authoring a plausible sentence or code pairing instead is the defect itself.

## Evidence
Replay of trace set simparity-wire-aws-v1 after the fix: all ten fixtures still diverge on body
content only (expected 200, actual 200). The codes still reported, per `ServiceCode`:

- sns (fixture service-quotas-catalog-list-aws-default-service-quotas-sns, step task#1): every
  code of the catalog except `L-390AADBE`, `L-4738DDCE` and `L-FF07E5EA`, i.e. L-01EACE54,
  L-038DA0E0, L-19F66BE5, L-1A43D3DB, L-21C7FFA6, L-2EE2434F, L-2FE42A3E, L-4126E74A, L-41B81227,
  L-4659BFC2, L-4EE37BC0, L-61103206, L-6521CF3A, L-6635B4C9, L-6A04D1EB, L-7B7E3FB8, L-7E62C088,
  L-820458B0, L-856966E8, L-876E1222, L-93308085, L-9A72FF33, L-9D5EC8F7, L-A05852B7, L-A228E0EB,
  L-A30AD1BE, L-A4340BCD, L-AB928142, L-B3509C67, L-B6107771, L-B96EDA7D, L-B98ECD3E, L-BBD4D2FF,
  L-BCD4AAF3, L-BF824DE9, L-C1E19420, L-CC5551AD, L-E1E48E53, L-F35E8445, L-F514C636, L-F6A3071A,
  L-F8E2BA85. Beside those, `L-154ECCA7`, `L-039289D5` and `L-93CEC191` are now published under no
  row at all, because the three rates they had carried belong to the three codes named above;
  whichever SNS quotas the recording publishes under them are still missing. Several of those
  codes (L-41B81227, L-A05852B7, L-A228E0EB, L-E1E48E53, L-F514C636) carry a `Period` and a
  `UsageMetric` in the recording where the catalog serves a counted row with neither, or the other
  way round, so the row under them is of a different CLASS than the one served.
- sqs: L-2DA3E3B2, L-49DA5CEC, L-4BE1B2BD, L-4FF3DAAF, L-79B6240C, L-9F628B95, L-A01B4DF0,
  L-B2A3B9D5, L-C491D5A4, L-E8A9C91E, L-F115E65A, plus the `Description` of L-98134C75's and
  L-BBEFA6CF's now correctly bound rows. `L-6A03DCE9` and `L-A7816957` are published under no row,
  because the message-attribute count and the queue-policy size they had carried belong to
  L-98134C75 and L-BBEFA6CF.
- sso: L-0299121C, L-3C8D41A0, L-7E1D4E33, L-89954265, L-B44C7A29, L-C4FCA052, plus the
  `Description` of L-2FDFC908's and L-D519A297's now correctly bound rows. The recording binds
  `L-C4FCA052` to the PEM-certificate file-size quota and publishes it as NOT adjustable, but
  states neither its `Value` nor its `Unit`, so no row for it can be authored here.
- storagegateway: L-14F83003, L-2232E8E3, L-2E88EE16, L-2EC26EAB, L-4951D254, L-5308FBCA,
  L-58DCC961, L-59D49F15, L-761F1680, L-7B01AFFB, L-803470D8, L-81A6E497, L-96D17DE9, L-98F44425,
  L-99E991AF, L-BD85E1A6, L-C86341FE, L-CEFD39A0, L-E0AEEBAC, L-F5915598, L-FF1BE522, plus the
  `Description` of the four now correctly bound rows. `L-6F75AC83` is published under no row,
  because the virtual-tape size it had carried belongs to L-311F8856. The recording binds
  `L-F5915598` to the file gateway's cache MAXIMUM in TiB but states no value for it.
- timestream: L-00158752, L-05CF2F29, L-1222731D, L-279D7B08, L-3F031494, L-6044E01C, L-6B1915C6,
  L-6C25862A, L-71D80F70, L-7565C2A8, L-7C9FE50E, L-8892F7F6, L-89E3B36D, L-8BF444C0, L-8DFD99E7,
  L-8FD9593E, L-9F20DF3A, L-A83B13DB, L-AE604CE2, L-BFCD0C9E, L-C0E9F2ED, L-F1AC16A2, L-F98EACE7,
  L-FA296006, L-FD5A0C1A, L-FEB34FC6. The three rates the original report quotes (L-798CB19D,
  L-C49D398A, L-F07A9AB4) were already bound as the recording states.
- vpc: L-085A6257, L-12E49864, L-1B52E74A, L-29B6F2EB, L-2AEEBF1A, L-2AFB9258, L-3248932A,
  L-3B4E38D2, L-407747CB, L-44499CD2, L-45FE3B85, L-589F43AA, L-7E9ECCDB, L-83CA0A9D, L-A272D574,
  L-A4707A72, L-B4A6D682, L-BB24F6E5, L-CD17FD4B, L-DC9F7029, L-DF5E4CA3, L-DFA99DE7, L-E79EC296,
  L-F678F1CE, L-FE5A380F, plus the `Description` of L-0EA8095F and the `Value` of L-93826ACB.
  `L-2C462E13` is published under no row, because the peering-request expiry it had carried
  belongs to L-8312C5BB.
- vpc-lattice: L-0DCA4434, L-3DEC3B9F, L-6095700C, L-620C821E, L-73D0F278, L-75D4A19E, L-89DEA27F,
  L-9BC96FEF, L-9CAD07FB, L-BB11C6B9, L-CA6A1CC5, L-CF78395E, L-D64E952E, L-D71303F3, L-EF6E2D62,
  plus the `Description` of the two now correctly bound rows. The recording binds `L-0DCA4434` to
  the resource gateways of one VPC and publishes it as adjustable, but states no value for it.
- wafv2: L-0224FEE0, L-030B54CC, L-041DD6D3, L-0A8A309C, L-11D00F38, L-291AF103, L-2B217866,
  L-2D9CB303, L-2EC3DE7B, L-3D6A3ADF, L-45C90A8A, L-5B2F9B11, L-5E8DF1EF, L-6F32B880, L-71C2E81B,
  L-74EBD1DD, L-8B48556D, L-91DEFBB6, L-9E6FF091, L-9FD4FE94, L-A85235F6, L-B1635397, L-B24834C1,
  L-BF0029B0, L-C5BCD850, L-C5E4D334, L-C928281E, L-CCCD1D7B, L-D95DED7E, L-D9F31E8A, L-E4E414A8,
  plus the `Description` of the two now correctly bound rows. `L-08788A4E` is published under no
  row, because the account's regional IP sets it had carried belong to L-D6DA96AE. The recording
  carries a `QuotaContext` on L-030B54CC and on L-08788A4E, so the quota under each is a quota OF
  A RESOURCE rather than the account-wide row served there.
- workspaces: L-011B3C20, L-0798A2C9, L-07C6D3A5, L-0B7D5CA2, L-0E312A12, L-158CE743, L-18CE281C,
  L-18F1706C, L-254B485B, L-2F7BEDD3, L-337DA39D, L-34278094, L-34D55C89, L-3AC69A9E, L-465DA8AF,
  L-5782CB4D, L-63F14199, L-6E205E73, L-70A02F17, L-843E43DA, L-8C70C791, L-925915DC, L-9A67B5CB,
  L-9C1F7EB2, L-BCACAEBC, L-BE9A8466, L-C266A5F4, L-C76331EA, L-D34D6523, L-D6CFBE96, L-E2CD2905,
  L-EEB759DE, L-EF6817EC, L-F61D0B79, L-F64A92CF, L-F7854FE2, plus the `Description` of the two
  now correctly bound bundle rows. Most of those codes carry a five-minute `Period` and an
  `AWS/Usage` `ResourceCount` in the recording, so they are bundle planes rather than the API
  rates served there, but the recording's bundle names and `Resource` series for them are not
  available here.
- xray: L-5DB17969, L-7F259013, L-998BFF16, L-D781C0FD, L-E2DD2778, L-E75A2BBF, plus the
  `Description` of the four now correctly bound rows. `L-3B8EFF88` is published under no row,
  because the per-trace annotation count it had carried belongs to L-C6B6F05D.

The rows the fix DID correct, and must stay corrected, are pinned in
`services/service-com-amazonaws-servicequotas-servicequotasv20190624/tests/all/default_catalogs_sns_through_xray.rs`
(`BINDINGS`), beside the codes and the quotas now published under nothing (`withdrawn`,
`UNPUBLISHED`) and the rows whose published `Description` no source states
(`DESCRIPTION_WITHHELD`).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. For each `ServiceCode` above, POST `ListAWSDefaultServiceQuotas` with
   `{"ServiceCode": "<code>"}` to the service-quotas endpoint and compare `$.Quotas`, keyed by
   `QuotaCode`, against the recorded response.
3. A `ServiceCode` is clean when every returned row's `QuotaName`, `Description`, `Value`,
   `Unit`, `Adjustable`, `GlobalQuota`, `Period`, `UsageMetric` and `QuotaContext` match the
   recorded row for the SAME `QuotaCode`, and no recorded `QuotaCode` is absent.
