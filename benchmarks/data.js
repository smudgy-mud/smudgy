window.BENCHMARK_DATA = {
  "lastUpdate": 1784867366169,
  "repoUrl": "https://github.com/smudgy-mud/smudgy",
  "entries": {
    "smudgy / main / m8a.2xlarge / Rust 1.97.1": [
      {
        "commit": {
          "author": {
            "email": "ping@walter.dev",
            "name": "wbk",
            "username": "wbk"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "2ef7495d858b57ea4f893ec4b7a5749c9054f634",
          "message": "Merge pull request #13 from smudgy-mud/fix/criterion-library-harness\n\nFix public Criterion corpus execution",
          "timestamp": "2026-07-22T22:32:56-07:00",
          "tree_id": "238dc28a4c3b8df969c66be018745c4141d8d5a6",
          "url": "https://github.com/smudgy-mud/smudgy/commit/2ef7495d858b57ea4f893ec4b7a5749c9054f634"
        },
        "date": 1784787573559,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "atlas_build/cold/10k",
            "value": 58258415.85555556,
            "range": "5.82155e+07..5.83012e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.82155e+07..5.83012e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "atlas_build/cold/1k",
            "value": 1289920.9164102564,
            "range": "1.28857e+06..1.29126e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.28857e+06..1.29126e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "atlas_build/cold/50k",
            "value": 1340662691.2,
            "range": "1.33968e+09..1.3417e+09",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.33968e+09..1.3417e+09 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "automap_step/create_room/100k",
            "value": 2107295.8120649653,
            "range": "1.93421e+06..2.29328e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.93421e+06..2.29328e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "automap_step/create_room/10k",
            "value": 2499349.1267686426,
            "range": "2.30538e+06..2.6873e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.30538e+06..2.6873e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "build/aho_corasick",
            "value": 6443198.561904762,
            "range": "6.44064e+06..6.44494e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6.44064e+06..6.44494e+06 ns/iter"
          },
          {
            "name": "build/regex_filtered",
            "value": 121093916.84935065,
            "range": "1.20655e+08..1.21324e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.20655e+08..1.21324e+08 ns/iter"
          },
          {
            "name": "build/regex_set",
            "value": 54459293.63636363,
            "range": "5.44118e+07..5.44919e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.44118e+07..5.44919e+07 ns/iter"
          },
          {
            "name": "build/tiered",
            "value": 32532109.24935065,
            "range": "3.24808e+07..3.25611e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.24808e+07..3.25611e+07 ns/iter"
          },
          {
            "name": "catalogue/sample/dynamic/small",
            "value": 95.19685000433107,
            "range": "95.1204..95.2829",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 95.1204..95.2829 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/large",
            "value": 6270.615829307385,
            "range": "6268.52..6272.96",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6268.52..6272.96 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/small",
            "value": 301.2901324566341,
            "range": "301.087..301.504",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 301.087..301.504 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/large",
            "value": 89.45866553596993,
            "range": "89.4276..89.4907",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 89.4276..89.4907 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/small",
            "value": 87.34249658745226,
            "range": "87.3057..87.3833",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 87.3057..87.3833 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_128",
            "value": 72040.38678228378,
            "range": "72007.2..72075.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 72007.2..72075.7 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_512",
            "value": 307083.28976799175,
            "range": "306964..307208",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 306964..307208 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_8",
            "value": 4415.940868750418,
            "range": "4412.84..4419.01",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4412.84..4419.01 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_4096",
            "value": 4617.411249192609,
            "range": "4616.79..4618.07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4616.79..4618.07 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_64",
            "value": 4132.2993861460645,
            "range": "4130.36..4134.35",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4130.36..4134.35 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_65536",
            "value": 4731.616958185111,
            "range": "4730.92..4732.31",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4730.92..4732.31 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "churn_packet/clean",
            "value": 67302.07052248855,
            "range": "67155.8..67427.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 67155.8..67427.6 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20",
            "value": 78342343.07142857,
            "range": "7.82501e+07..7.84507e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7.82501e+07..7.84507e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20_x4pkg",
            "value": 80828369.17142855,
            "range": "8.0689e+07..8.09641e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8.0689e+07..8.09641e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/toggle20",
            "value": 78871.51706931942,
            "range": "78772.1..78957.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 78772.1..78957.7 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_residue/full/10000",
            "value": 334459776.95,
            "range": "3.33927e+08..3.34929e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.33927e+08..3.34929e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/1000",
            "value": 336715485.5,
            "range": "3.35626e+08..3.38391e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.35626e+08..3.38391e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/5000",
            "value": 292877603.5,
            "range": "2.92249e+08..2.93479e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.92249e+08..2.93479e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/1000",
            "value": 335330710.15,
            "range": "3.34691e+08..3.3593e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.34691e+08..3.3593e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/5000",
            "value": 295078271.65,
            "range": "2.94667e+08..2.95553e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.94667e+08..2.95553e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_absent/25",
            "value": 233312598.7,
            "range": "2.28841e+08..2.37628e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.28841e+08..2.37628e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_disabled/25",
            "value": 276537135.45,
            "range": "2.75881e+08..2.77119e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.75881e+08..2.77119e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "creator_parse/package",
            "value": 256.4556880789001,
            "range": "256.377..256.538",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 256.377..256.538 ns/iter"
          },
          {
            "name": "creator_parse/user",
            "value": 50.479342241734024,
            "range": "50.4614..50.4972",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 50.4614..50.4972 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/1000",
            "value": 14246255.069444444,
            "range": "1.42402e+07..1.42543e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.42402e+07..1.42543e+07 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/10000",
            "value": 53477086.86,
            "range": "5.34206e+07..5.35307e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.34206e+07..5.35307e+07 ns/iter"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/bytes",
            "value": 343747561.5,
            "range": "3.42888e+08..3.44653e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.42888e+08..3.44653e+08 ns/iter\nThroughput input: {\"BytesDecimal\": 16269045}"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/lines",
            "value": 328991251.75,
            "range": "3.28355e+08..3.29628e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.28355e+08..3.29628e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "extend_line/at_capacity",
            "value": 119882.26572387344,
            "range": "119832..119933",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 119832..119933 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag16",
            "value": 17107296.24285714,
            "range": "1.71007e+07..1.7114e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.71007e+07..1.7114e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag4",
            "value": 3292008.1092307693,
            "range": "3.29103e+06..3.29311e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.29103e+06..3.29311e+06 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/whole_lines",
            "value": 66194.51425707285,
            "range": "66181.6..66208.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66181.6..66208.7 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "flush_coalesced/J1/W0",
            "value": 143.6801225251592,
            "range": "143.544..143.837",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 143.544..143.837 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W64",
            "value": 6278.493503405038,
            "range": "6276.42..6280.44",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6276.42..6280.44 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W8",
            "value": 793.6226029046402,
            "range": "793.502..793.751",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 793.502..793.751 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J128/W0",
            "value": 6255.044900028978,
            "range": "6253.89..6256.26",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6253.89..6256.26 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W64",
            "value": 150247.5322280179,
            "range": "150186..150297",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 150186..150297 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W8",
            "value": 24641.04178195905,
            "range": "24632.1..24649.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 24632.1..24649.1 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J16/W0",
            "value": 665.3913534338659,
            "range": "665.168..665.624",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 665.168..665.624 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W64",
            "value": 23453.440726080775,
            "range": "23449..23458.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 23449..23458.3 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W8",
            "value": 3608.369908952721,
            "range": "3606.66..3609.79",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3606.66..3609.79 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J1/W64",
            "value": 7084.876862366111,
            "range": "7082.56..7087.62",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7082.56..7087.62 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J1/W8",
            "value": 866.7692880548959,
            "range": "865.893..867.68",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 865.893..867.68 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J128/W64",
            "value": 514092.393918919,
            "range": "513811..514333",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 513811..514333 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J128/W8",
            "value": 67155.69251017639,
            "range": "67131.5..67182.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 67131.5..67182.4 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J16/W64",
            "value": 65072.65642948992,
            "range": "65048.5..65096.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 65048.5..65096.1 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J16/W8",
            "value": 8796.289731486231,
            "range": "8792.3..8800.95",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8792.3..8800.95 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J1/W0",
            "value": 144.3049814321018,
            "range": "144.161..144.515",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 144.161..144.515 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W64",
            "value": 6541.5067648229215,
            "range": "6536.99..6546.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6536.99..6546.1 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W8",
            "value": 841.512090923701,
            "range": "841.165..841.849",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 841.165..841.849 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J128/W0",
            "value": 4532.048112455619,
            "range": "4531.26..4532.83",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4531.26..4532.83 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W64",
            "value": 856096.4899705015,
            "range": "855822..856333",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 855822..856333 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W8",
            "value": 108801.94029147022,
            "range": "108765..108836",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 108765..108836 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J16/W0",
            "value": 754.1633221370287,
            "range": "753.706..754.638",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 753.706..754.638 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W64",
            "value": 104750.6441219158,
            "range": "104729..104771",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 104729..104771 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W8",
            "value": 13408.654252933507,
            "range": "13405.6..13411.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 13405.6..13411.4 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "fold/lower",
            "value": 20.91161972474778,
            "range": "20.9099..20.9133",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.9099..20.9133 ns/iter"
          },
          {
            "name": "fold/mixed",
            "value": 20.93327779957463,
            "range": "20.9316..20.935",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.9316..20.935 ns/iter"
          },
          {
            "name": "follow/find_room_by_external_id/100k",
            "value": 91.67762062596556,
            "range": "91.6657..91.6911",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 91.6657..91.6911 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "follow/find_room_by_external_id/10k",
            "value": 94.68548434009978,
            "range": "94.6646..94.7077",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 94.6646..94.7077 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "frame_proxy/10k",
            "value": 170914.63791322318,
            "range": "170876..170950",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 170876..170950 ns/iter\nThroughput input: {\"Elements\": 32430}"
          },
          {
            "name": "identification/by_title_and_description/10k",
            "value": 15337.944134335226,
            "range": "15335.1..15340.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 15335.1..15340.6 ns/iter\nThroughput input: {\"Elements\": 44}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy",
            "value": 549333374,
            "range": "5.48151e+08..5.50655e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.48151e+08..5.50655e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy/no_raw",
            "value": 470337001,
            "range": "4.60401e+08..4.7981e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.60401e+08..4.7981e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_light",
            "value": 287457775,
            "range": "2.87009e+08..2.87883e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.87009e+08..2.87883e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/ansi_light/no_raw",
            "value": 235659660.7666667,
            "range": "2.35304e+08..2.36026e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.35304e+08..2.36026e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/iac_dense",
            "value": 292041311.1,
            "range": "2.91397e+08..2.92721e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.91397e+08..2.92721e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "ingest_pipeline/iac_dense/no_raw",
            "value": 232683436.3,
            "range": "2.31726e+08..2.33739e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.31726e+08..2.33739e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "interop_delivery/emit_cross_isolate/S1",
            "value": 84450.14802989131,
            "range": "84172.2..84767",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 84172.2..84767 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S1",
            "value": 84476.27725046445,
            "range": "84329.5..84593.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 84329.5..84593.8 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S64",
            "value": 3819401.0045801527,
            "range": "3.81695e+06..3.82233e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.81695e+06..3.82233e+06 ns/iter\nThroughput input: {\"Elements\": 2048}"
          },
          {
            "name": "interop_delivery/emit_fanout/S8",
            "value": 505146.1242424242,
            "range": "504754..505554",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 504754..505554 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P16k",
            "value": 1620893.051132686,
            "range": "1.62032e+06..1.62142e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.62032e+06..1.62142e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P64",
            "value": 423186.8796280642,
            "range": "423103..423279",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 423103..423279 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W64",
            "value": 1838257.0875457874,
            "range": "1.83761e+06..1.83886e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.83761e+06..1.83886e+06 ns/iter\nThroughput input: {\"Elements\": 1024}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W8",
            "value": 285913.8317142857,
            "range": "285824..286015",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 285824..286015 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_delivery/watch_per_write/W8",
            "value": 436671.5572052401,
            "range": "436432..436881",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 436432..436881 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_ops/package/emit128",
            "value": 25750.745685331676,
            "range": "25619..25900.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 25619..25900.9 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/get128",
            "value": 61468.07182402372,
            "range": "61419..61519.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 61419..61519.4 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set128",
            "value": 96221.93407138767,
            "range": "96132.1..96306.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 96132.1..96306.6 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set_per_turn64",
            "value": 465690.775698324,
            "range": "465532..465865",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 465532..465865 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_ops/user/emit128",
            "value": 25525.921986761765,
            "range": "25401.9..25653.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 25401.9..25653.1 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/get128",
            "value": 57530.577238117214,
            "range": "57437.4..57650.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 57437.4..57650.6 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set128",
            "value": 72840.49218614091,
            "range": "72797.1..72884",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 72797.1..72884 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set_per_turn64",
            "value": 418433.15879396984,
            "range": "418224..418648",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 418224..418648 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_read/keys_32k",
            "value": 69102.10045681061,
            "range": "68712.5..69501.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 68712.5..69501.8 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_read/materialize_32k",
            "value": 11537998.211627906,
            "range": "1.14892e+07..1.16282e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.14892e+07..1.16282e+07 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "interop_read/value_leaf/1k",
            "value": 70904.20493965759,
            "range": "70607.2..71146.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 70607.2..71146.3 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/1m",
            "value": 70257.64373765867,
            "range": "70086.4..70456.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 70086.4..70456.7 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth1",
            "value": 71656.82188215104,
            "range": "71224.5..72010.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 71224.5..72010.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth4",
            "value": 59225.413769239945,
            "range": "59024.3..59395.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 59024.3..59395.2 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "line_operations/replace_and_highlight",
            "value": 9501.075815941509,
            "range": "9498.78..9503.43",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 9498.78..9503.43 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "path_parse/bracket",
            "value": 74.8445354431707,
            "range": "74.8375..74.8522",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 74.8375..74.8522 ns/iter"
          },
          {
            "name": "path_parse/depth1",
            "value": 48.71198840142559,
            "range": "48.7061..48.7179",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 48.7061..48.7179 ns/iter"
          },
          {
            "name": "path_parse/depth4",
            "value": 87.8323623669568,
            "range": "87.7747..87.8815",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 87.7747..87.8815 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/10k",
            "value": 533880.0039487726,
            "range": "533816..533942",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 533816..533942 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/50k",
            "value": 460502.3714811407,
            "range": "460441..460565",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 460441..460565 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/10k",
            "value": 3183416.1518987347,
            "range": "3.1804e+06..3.18541e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.1804e+06..3.18541e+06 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/50k",
            "value": 25665641.214999996,
            "range": "2.5526e+07..2.59043e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.5526e+07..2.59043e+07 ns/iter"
          },
          {
            "name": "pathfinding/path_across/10k",
            "value": 2963063.4331360944,
            "range": "2.95875e+06..2.96703e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.95875e+06..2.96703e+06 ns/iter"
          },
          {
            "name": "pathfinding/path_across/50k",
            "value": 22343477.295454543,
            "range": "2.21637e+07..2.25971e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.21637e+07..2.25971e+07 ns/iter"
          },
          {
            "name": "per_emit_composite/package",
            "value": 414.35482155817505,
            "range": "414.315..414.392",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 414.315..414.392 ns/iter"
          },
          {
            "name": "per_set_composite/package",
            "value": 355.84395107486495,
            "range": "355.708..355.965",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 355.708..355.965 ns/iter"
          },
          {
            "name": "per_set_composite/user",
            "value": 139.05026414344366,
            "range": "138.935..139.149",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 138.935..139.149 ns/iter"
          },
          {
            "name": "producer_parse/package",
            "value": 45.89526671525349,
            "range": "45.8908..45.9003",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 45.8908..45.9003 ns/iter"
          },
          {
            "name": "producer_parse/user",
            "value": 3.830853824260285,
            "range": "3.82805..3.83388",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.82805..3.83388 ns/iter"
          },
          {
            "name": "rebuild/room_connections/10k",
            "value": 28518702.75,
            "range": "2.81651e+07..2.88291e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.81651e+07..2.88291e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "rebuild/room_connections/1k",
            "value": 1808407.938275862,
            "range": "1.80215e+06..1.81363e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.80215e+06..1.81363e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "rebuild/room_connections/50k",
            "value": 198286478.3333333,
            "range": "1.95037e+08..2.01244e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.95037e+08..2.01244e+08 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "scan_literals/aho_corasick_leftmost",
            "value": 15129671.836363636,
            "range": "1.51106e+07..1.51433e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.51106e+07..1.51433e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/aho_corasick_overlapping",
            "value": 17190651.95194805,
            "range": "1.71771e+07..1.7203e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.71771e+07..1.7203e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_filtered",
            "value": 398832048.05,
            "range": "3.98748e+08..3.98912e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.98748e+08..3.98912e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_set_current",
            "value": 30588500495.7,
            "range": "3.05775e+10..3.05987e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.05775e+10..3.05987e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_literals/tiered",
            "value": 50323479.27402598,
            "range": "5.03129e+07..5.03362e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.03129e+07..5.03362e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_filtered",
            "value": 498099859.2,
            "range": "4.98021e+08..4.98183e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.98021e+08..4.98183e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_set_current",
            "value": 31646936811.2,
            "range": "3.16235e+10..3.16689e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.16235e+10..3.16689e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_mixed/tiered",
            "value": 167695788.76103896,
            "range": "1.67542e+08..1.67805e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.67542e+08..1.67805e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "script_dispatch/baseline",
            "value": 341890.83909520594,
            "range": "340953..342795",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 340953..342795 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire0",
            "value": 1226378.6655256725,
            "range": "1.2254e+06..1.22761e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.2254e+06..1.22761e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire20",
            "value": 2801797.3513966477,
            "range": "2.79532e+06..2.80705e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.79532e+06..2.80705e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire5",
            "value": 1780932.390747331,
            "range": "1.77827e+06..1.78433e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.77827e+06..1.78433e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "sgr/process/bold_color",
            "value": 32.222101311213144,
            "range": "32.2151..32.2299",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 32.2151..32.2299 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/color_256",
            "value": 50.58677534773439,
            "range": "50.5802..50.5939",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 50.5802..50.5939 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/reset",
            "value": 21.03476341433651,
            "range": "21.0316..21.0384",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.0316..21.0384 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/simple_color",
            "value": 21.267904162109417,
            "range": "21.2652..21.2708",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.2652..21.2708 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/truecolor",
            "value": 93.13472130674953,
            "range": "93.1212..93.149",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 93.1212..93.149 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "spatial_query/connections/viewport_full/10k",
            "value": 81970.30045849027,
            "range": "81923..82014",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 81923..82014 ns/iter\nThroughput input: {\"Elements\": 19802}"
          },
          {
            "name": "spatial_query/connections/viewport_full/50k",
            "value": 437533.375,
            "range": "437468..437601",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 437468..437601 ns/iter\nThroughput input: {\"Elements\": 99557}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/10k",
            "value": 19002.778420496565,
            "range": "18982.5..19023",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 18982.5..19023 ns/iter\nThroughput input: {\"Elements\": 4416}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/50k",
            "value": 91957.79649541284,
            "range": "91926.3..91994.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 91926.3..91994.3 ns/iter\nThroughput input: {\"Elements\": 21021}"
          },
          {
            "name": "spatial_query/connections/viewport_small/10k",
            "value": 2954.6837917589414,
            "range": "2948.67..2960.87",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2948.67..2960.87 ns/iter\nThroughput input: {\"Elements\": 576}"
          },
          {
            "name": "spatial_query/connections/viewport_small/50k",
            "value": 10814.221796268854,
            "range": "10788.1..10836.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 10788.1..10836.7 ns/iter\nThroughput input: {\"Elements\": 2359}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/10k",
            "value": 38553.504423551174,
            "range": "38541.4..38565.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 38541.4..38565.2 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/50k",
            "value": 193262.40247390798,
            "range": "193232..193292",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 193232..193292 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/10k",
            "value": 9429.480671701933,
            "range": "9425.7..9433.37",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 9425.7..9433.37 ns/iter\nThroughput input: {\"Elements\": 2070}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/50k",
            "value": 40608.99290608625,
            "range": "40555.3..40666.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 40555.3..40666.6 ns/iter\nThroughput input: {\"Elements\": 10306}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/10k",
            "value": 1170.4547255308444,
            "range": "1153.84..1183.96",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1153.84..1183.96 ns/iter\nThroughput input: {\"Elements\": 240}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/50k",
            "value": 5028.462393915132,
            "range": "5019.92..5036.36",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5019.92..5036.36 ns/iter\nThroughput input: {\"Elements\": 1146}"
          },
          {
            "name": "styled_line/new_no_raw/long_plain",
            "value": 22.34039324729888,
            "range": "22.3362..22.3449",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 22.3362..22.3449 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/long_styled",
            "value": 23.367500790663353,
            "range": "23.362..23.3736",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 23.362..23.3736 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/short_plain",
            "value": 21.64408403979368,
            "range": "21.6363..21.6531",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.6363..21.6531 ns/iter\nThroughput input: {\"Bytes\": 40}"
          },
          {
            "name": "styled_line/new_with_raw/long_plain",
            "value": 97.15516519034718,
            "range": "97.1047..97.2147",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 97.1047..97.2147 ns/iter\nThroughput input: {\"Bytes\": 400}"
          },
          {
            "name": "styled_line/new_with_raw/long_styled",
            "value": 123.27439456464988,
            "range": "123.226..123.333",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 123.226..123.333 ns/iter\nThroughput input: {\"Bytes\": 464}"
          },
          {
            "name": "styled_line/new_with_raw/short_plain",
            "value": 39.28451921128985,
            "range": "39.278..39.2911",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 39.278..39.2911 ns/iter\nThroughput input: {\"Bytes\": 80}"
          },
          {
            "name": "telnet_receive/ansi_light",
            "value": 287735.68854346575,
            "range": "287516..287966",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 287516..287966 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "telnet_receive/iac_dense",
            "value": 4457680.525663717,
            "range": "4.45543e+06..4.45966e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.45543e+06..4.45966e+06 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "to_spans/by_span_count/1",
            "value": 63.292662523857345,
            "range": "63.2831..63.3025",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 63.2831..63.3025 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "to_spans/by_span_count/32",
            "value": 1304.3932725215204,
            "range": "1304.17..1304.61",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1304.17..1304.61 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "to_spans/by_span_count/8",
            "value": 343.7585762411303,
            "range": "343.594..343.911",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 343.594..343.911 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "trigger_verbs/empty",
            "value": 1096771.945614035,
            "range": "1.09627e+06..1.09731e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.09627e+06..1.09731e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/gag",
            "value": 1094031.408370044,
            "range": "1.09355e+06..1.09447e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.09355e+06..1.09447e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/highlight",
            "value": 1227570.8784313728,
            "range": "1.22731e+06..1.22786e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.22731e+06..1.22786e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/read_echo",
            "value": 1452032.8231884057,
            "range": "1.45035e+06..1.45469e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.45035e+06..1.45469e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "upsert_room/single/10k",
            "value": 982131.1507692307,
            "range": "978761..985383",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 978761..985383 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/1k",
            "value": 887691.7143847486,
            "range": "880731..894463",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 880731..894463 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/50k",
            "value": 1450267.186,
            "range": "1.44934e+06..1.45119e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.44934e+06..1.45119e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/1",
            "value": 1009967.9088888889,
            "range": "1.00923e+06..1.01069e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.00923e+06..1.01069e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/16",
            "value": 1187416.2275534444,
            "range": "1.1851e+06..1.18964e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.1851e+06..1.18964e+06 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "upsert_rooms/batch_10k/256",
            "value": 3348102.1390728476,
            "range": "3.32994e+06..3.36732e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.32994e+06..3.36732e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "write_and_flush/J16/W8_mixed",
            "value": 17576.77033195753,
            "range": "17569.5..17583.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 2ef7495d858b57ea4f893ec4b7a5749c9054f634\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 17569.5..17583.5 ns/iter\nThroughput input: {\"Elements\": 16}"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "ping@walter.dev",
            "name": "wbk",
            "username": "wbk"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "02fbb94e87809e594555cc350d0e8370c09ce529",
          "message": "Merge pull request #2 from GTanger/fix/atomic-json-writes\n\nfix(core): write user data atomically",
          "timestamp": "2026-07-23T16:14:28-07:00",
          "tree_id": "c22f90b8410d38bc06d9980c21fc0399f9ca0fe5",
          "url": "https://github.com/smudgy-mud/smudgy/commit/02fbb94e87809e594555cc350d0e8370c09ce529"
        },
        "date": 1784851262929,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "atlas_build/cold/10k",
            "value": 59349231.83333333,
            "range": "5.93054e+07..5.94018e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.93054e+07..5.94018e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "atlas_build/cold/1k",
            "value": 1303402.1294429707,
            "range": "1.30275e+06..1.304e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.30275e+06..1.304e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "atlas_build/cold/50k",
            "value": 1358219284.5,
            "range": "1.35717e+09..1.3594e+09",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.35717e+09..1.3594e+09 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "automap_step/create_room/100k",
            "value": 2009794.4190751445,
            "range": "1.80862e+06..2.21563e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.80862e+06..2.21563e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "automap_step/create_room/10k",
            "value": 2474372.053904762,
            "range": "2.28466e+06..2.65825e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.28466e+06..2.65825e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "build/aho_corasick",
            "value": 6448637.366233766,
            "range": "6.44588e+06..6.4503e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6.44588e+06..6.4503e+06 ns/iter"
          },
          {
            "name": "build/regex_filtered",
            "value": 120291375.7038961,
            "range": "1.20195e+08..1.20461e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.20195e+08..1.20461e+08 ns/iter"
          },
          {
            "name": "build/regex_set",
            "value": 52721598.23766234,
            "range": "5.2662e+07..5.27721e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.2662e+07..5.27721e+07 ns/iter"
          },
          {
            "name": "build/tiered",
            "value": 32710161.473593075,
            "range": "3.26831e+07..3.27478e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.26831e+07..3.27478e+07 ns/iter"
          },
          {
            "name": "catalogue/sample/dynamic/small",
            "value": 93.37057337842526,
            "range": "93.3469..93.3958",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 93.3469..93.3958 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/large",
            "value": 6412.00136399141,
            "range": "6408.59..6415.48",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6408.59..6415.48 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/small",
            "value": 300.8968395692942,
            "range": "300.728..301.073",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 300.728..301.073 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/large",
            "value": 89.10066159097512,
            "range": "89.072..89.1299",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 89.072..89.1299 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/small",
            "value": 86.36764487280537,
            "range": "86.3242..86.4147",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 86.3242..86.4147 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_128",
            "value": 73444.37230699403,
            "range": "73401.1..73486.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 73401.1..73486.2 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_512",
            "value": 312343.9606856805,
            "range": "312148..312554",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 312148..312554 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_8",
            "value": 4471.216174106721,
            "range": "4469.9..4472.49",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4469.9..4472.49 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_4096",
            "value": 4517.112072703079,
            "range": "4514.08..4520.08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4514.08..4520.08 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_64",
            "value": 4109.747304118658,
            "range": "4107.31..4112.37",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4107.31..4112.37 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_65536",
            "value": 5380.401986875896,
            "range": "5378.24..5382.54",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5378.24..5382.54 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "churn_packet/clean",
            "value": 92987.14231401938,
            "range": "92543.2..93291.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 92543.2..93291.6 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20",
            "value": 76520748.68571429,
            "range": "7.64512e+07..7.65873e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7.64512e+07..7.65873e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20_x4pkg",
            "value": 79119251.67142856,
            "range": "7.89546e+07..7.92843e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7.89546e+07..7.92843e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/toggle20",
            "value": 104130.53253588516,
            "range": "103961..104384",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 103961..104384 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_residue/full/10000",
            "value": 334730086.9,
            "range": "3.33454e+08..3.36097e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.33454e+08..3.36097e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/1000",
            "value": 333327879.15,
            "range": "3.32641e+08..3.33998e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.32641e+08..3.33998e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/5000",
            "value": 292957660.45,
            "range": "2.92499e+08..2.93459e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.92499e+08..2.93459e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/1000",
            "value": 336322069.3,
            "range": "3.3574e+08..3.36898e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.3574e+08..3.36898e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/5000",
            "value": 296425914.95,
            "range": "2.96063e+08..2.96832e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.96063e+08..2.96832e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_absent/25",
            "value": 231638573.19999996,
            "range": "2.27803e+08..2.35737e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.27803e+08..2.35737e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_disabled/25",
            "value": 279790767.8,
            "range": "2.78916e+08..2.80741e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.78916e+08..2.80741e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "creator_parse/package",
            "value": 266.1597706206306,
            "range": "265.957..266.355",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 265.957..266.355 ns/iter"
          },
          {
            "name": "creator_parse/user",
            "value": 53.35082601063589,
            "range": "52.9697..53.7733",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 52.9697..53.7733 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/1000",
            "value": 14221569.619444445,
            "range": "1.42125e+07..1.42305e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.42125e+07..1.42305e+07 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/10000",
            "value": 53563460.470000006,
            "range": "5.34897e+07..5.36323e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.34897e+07..5.36323e+07 ns/iter"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/bytes",
            "value": 341825533.4,
            "range": "3.40796e+08..3.4287e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.40796e+08..3.4287e+08 ns/iter\nThroughput input: {\"BytesDecimal\": 16269045}"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/lines",
            "value": 323655828.85,
            "range": "3.23e+08..3.24247e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.23e+08..3.24247e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "extend_line/at_capacity",
            "value": 119638.73604400859,
            "range": "119606..119673",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 119606..119673 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag16",
            "value": 16761449.244827587,
            "range": "1.67494e+07..1.67738e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.67494e+07..1.67738e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag4",
            "value": 3151662.578358209,
            "range": "3.15083e+06..3.15267e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.15083e+06..3.15267e+06 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/whole_lines",
            "value": 65713.90455148034,
            "range": "65678.8..65753.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 65678.8..65753.5 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "flush_coalesced/J1/W0",
            "value": 143.31863823201346,
            "range": "143.218..143.438",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 143.218..143.438 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W64",
            "value": 6246.097299590246,
            "range": "6243.46..6248.65",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6243.46..6248.65 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W8",
            "value": 768.5654924700481,
            "range": "768.212..768.953",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 768.212..768.953 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J128/W0",
            "value": 6374.509315941599,
            "range": "6370.03..6379.63",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6370.03..6379.63 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W64",
            "value": 155887.45394333842,
            "range": "155753..156007",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 155753..156007 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W8",
            "value": 23107.753259656176,
            "range": "23091.7..23125",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 23091.7..23125 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J16/W0",
            "value": 696.0917485048642,
            "range": "695.884..696.31",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 695.884..696.31 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W64",
            "value": 23341.049886300778,
            "range": "23332.5..23349",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 23332.5..23349 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W8",
            "value": 3632.59945648709,
            "range": "3630.74..3634.44",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3630.74..3634.44 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J1/W64",
            "value": 6708.341075885197,
            "range": "6704.84..6712.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6704.84..6712.1 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J1/W8",
            "value": 851.3972719223605,
            "range": "851.125..851.662",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 851.125..851.662 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J128/W64",
            "value": 517480.25806451606,
            "range": "517178..517777",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 517178..517777 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J128/W8",
            "value": 67249.88472107722,
            "range": "67238.2..67262.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 67238.2..67262.2 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J16/W64",
            "value": 66131.31344319167,
            "range": "66113.5..66144.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66113.5..66144.6 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J16/W8",
            "value": 8644.256177330524,
            "range": "8639.82..8648.89",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8639.82..8648.89 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J1/W0",
            "value": 144.9357676958622,
            "range": "144.9..144.973",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 144.9..144.973 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W64",
            "value": 6462.419605911329,
            "range": "6458.34..6466.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6458.34..6466.3 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W8",
            "value": 879.2261197675992,
            "range": "878.822..879.601",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 878.822..879.601 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J128/W0",
            "value": 4546.728735125606,
            "range": "4544.01..4549.88",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4544.01..4549.88 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W64",
            "value": 857024.1589970499,
            "range": "855943..857760",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 855943..857760 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W8",
            "value": 109805.3795191069,
            "range": "109794..109820",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 109794..109820 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J16/W0",
            "value": 738.3425222875316,
            "range": "738.098..738.643",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 738.098..738.643 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W64",
            "value": 103807.21490436664,
            "range": "103784..103832",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 103784..103832 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W8",
            "value": 13238.733329872828,
            "range": "13228.5..13251.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 13228.5..13251.1 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "fold/lower",
            "value": 20.562373231851858,
            "range": "20.5553..20.5701",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.5553..20.5701 ns/iter"
          },
          {
            "name": "fold/mixed",
            "value": 20.525442511196907,
            "range": "20.5241..20.5268",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.5241..20.5268 ns/iter"
          },
          {
            "name": "follow/find_room_by_external_id/100k",
            "value": 93.08809775805973,
            "range": "93.0344..93.1475",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 93.0344..93.1475 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "follow/find_room_by_external_id/10k",
            "value": 95.5592643524784,
            "range": "95.5234..95.5976",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 95.5234..95.5976 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "frame_proxy/10k",
            "value": 168591.36483479434,
            "range": "168316..168850",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 168316..168850 ns/iter\nThroughput input: {\"Elements\": 32430}"
          },
          {
            "name": "identification/by_title_and_description/10k",
            "value": 15316.372278864703,
            "range": "15311.8..15321.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 15311.8..15321.6 ns/iter\nThroughput input: {\"Elements\": 44}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy",
            "value": 537629166.1,
            "range": "5.34946e+08..5.40636e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.34946e+08..5.40636e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy/no_raw",
            "value": 467517465.6,
            "range": "4.58639e+08..4.75024e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.58639e+08..4.75024e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_light",
            "value": 277994118.4,
            "range": "2.77466e+08..2.78554e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.77466e+08..2.78554e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/ansi_light/no_raw",
            "value": 231093969.5,
            "range": "2.30818e+08..2.31379e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.30818e+08..2.31379e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/iac_dense",
            "value": 280975685.95,
            "range": "2.8033e+08..2.81656e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.8033e+08..2.81656e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "ingest_pipeline/iac_dense/no_raw",
            "value": 232319885.1333333,
            "range": "2.31834e+08..2.3279e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.31834e+08..2.3279e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "interop_delivery/emit_cross_isolate/S1",
            "value": 86357.09877070172,
            "range": "86100..86583.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 86100..86583.5 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S1",
            "value": 84946.41668358716,
            "range": "84601..85314.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 84601..85314.9 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S64",
            "value": 3897535.1790697677,
            "range": "3.88427e+06..3.91053e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.88427e+06..3.91053e+06 ns/iter\nThroughput input: {\"Elements\": 2048}"
          },
          {
            "name": "interop_delivery/emit_fanout/S8",
            "value": 510738.1864615384,
            "range": "510423..511138",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 510423..511138 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P16k",
            "value": 1636866.823856209,
            "range": "1.6361e+06..1.63761e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.6361e+06..1.63761e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P64",
            "value": 431322.49956933677,
            "range": "431180..431502",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 431180..431502 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W64",
            "value": 1871569.4271375462,
            "range": "1.8675e+06..1.8756e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.8675e+06..1.8756e+06 ns/iter\nThroughput input: {\"Elements\": 1024}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W8",
            "value": 289836.1266320046,
            "range": "289262..290630",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 289262..290630 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_delivery/watch_per_write/W8",
            "value": 443997.2957257346,
            "range": "443397..444583",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 443397..444583 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_ops/package/emit128",
            "value": 28578.192178483692,
            "range": "28518..28645.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 28518..28645.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/get128",
            "value": 66811.0831220177,
            "range": "66635.8..66962",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66635.8..66962 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set128",
            "value": 100837.82643931797,
            "range": "100758..100916",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 100758..100916 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set_per_turn64",
            "value": 467480.3789522919,
            "range": "467011..468010",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 467011..468010 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_ops/user/emit128",
            "value": 27865.350220863067,
            "range": "27836..27898.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 27836..27898.5 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/get128",
            "value": 61215.08978093256,
            "range": "60736.9..61734.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 60736.9..61734.6 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set128",
            "value": 74487.96971924152,
            "range": "73934.6..74909.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73934.6..74909.6 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set_per_turn64",
            "value": 422126.20558848436,
            "range": "421770..422451",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 421770..422451 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_read/keys_32k",
            "value": 79282.4710564168,
            "range": "79057.7..79470.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 79057.7..79470.1 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_read/materialize_32k",
            "value": 12760339.787499998,
            "range": "1.27487e+07..1.27714e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.27487e+07..1.27714e+07 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "interop_read/value_leaf/1k",
            "value": 73886.11815916753,
            "range": "73810.4..73979.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73810.4..73979.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/1m",
            "value": 73702.43086510264,
            "range": "73378.5..73981.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73378.5..73981.7 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth1",
            "value": 73770.5761627907,
            "range": "73695.2..73839.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73695.2..73839.3 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth4",
            "value": 73726.20098630944,
            "range": "73314.8..74086.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73314.8..74086.6 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "line_operations/replace_and_highlight",
            "value": 9392.274792062157,
            "range": "9387.9..9397.99",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 9387.9..9397.99 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "path_parse/bracket",
            "value": 74.81072419393452,
            "range": "74.8041..74.8178",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 74.8041..74.8178 ns/iter"
          },
          {
            "name": "path_parse/depth1",
            "value": 49.01468470940911,
            "range": "49.0064..49.0232",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 49.0064..49.0232 ns/iter"
          },
          {
            "name": "path_parse/depth4",
            "value": 86.75175845963152,
            "range": "86.7352..86.7685",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 86.7352..86.7685 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/10k",
            "value": 529740.2767838126,
            "range": "528753..530482",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 528753..530482 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/50k",
            "value": 461005.84525547444,
            "range": "459607..462153",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 459607..462153 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/10k",
            "value": 3155738.0893750004,
            "range": "3.14393e+06..3.16731e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.14393e+06..3.16731e+06 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/50k",
            "value": 27938036.55,
            "range": "2.78603e+07..2.80348e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.78603e+07..2.80348e+07 ns/iter"
          },
          {
            "name": "pathfinding/path_across/10k",
            "value": 2799857.3844444444,
            "range": "2.79914e+06..2.80057e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.79914e+06..2.80057e+06 ns/iter"
          },
          {
            "name": "pathfinding/path_across/50k",
            "value": 22518063.221739132,
            "range": "2.24639e+07..2.2567e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.24639e+07..2.2567e+07 ns/iter"
          },
          {
            "name": "per_emit_composite/package",
            "value": 421.4216732781307,
            "range": "421.083..421.774",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 421.083..421.774 ns/iter"
          },
          {
            "name": "per_set_composite/package",
            "value": 354.4623212117565,
            "range": "354.39..354.541",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 354.39..354.541 ns/iter"
          },
          {
            "name": "per_set_composite/user",
            "value": 131.08949487980584,
            "range": "131.035..131.149",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 131.035..131.149 ns/iter"
          },
          {
            "name": "producer_parse/package",
            "value": 46.472082353305254,
            "range": "46.4276..46.5236",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 46.4276..46.5236 ns/iter"
          },
          {
            "name": "producer_parse/user",
            "value": 3.8928414661235204,
            "range": "3.88699..3.89923",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.88699..3.89923 ns/iter"
          },
          {
            "name": "rebuild/room_connections/10k",
            "value": 27952081.271428574,
            "range": "2.76278e+07..2.8287e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.76278e+07..2.8287e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "rebuild/room_connections/1k",
            "value": 1811291.3796491227,
            "range": "1.80203e+06..1.81923e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.80203e+06..1.81923e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "rebuild/room_connections/50k",
            "value": 192833976.3,
            "range": "1.89629e+08..1.95803e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.89629e+08..1.95803e+08 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "scan_literals/aho_corasick_leftmost",
            "value": 15183547.168398269,
            "range": "1.51675e+07..1.52167e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.51675e+07..1.52167e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/aho_corasick_overlapping",
            "value": 17317406.051948052,
            "range": "1.73099e+07..1.73271e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.73099e+07..1.73271e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_filtered",
            "value": 402760629.8,
            "range": "4.02711e+08..4.02824e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.02711e+08..4.02824e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_set_current",
            "value": 31408883377,
            "range": "3.13577e+10..3.14769e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.13577e+10..3.14769e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_literals/tiered",
            "value": 50221261.88831169,
            "range": "5.02062e+07..5.02361e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.02062e+07..5.02361e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_filtered",
            "value": 496183804.2,
            "range": "4.9591e+08..4.96549e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.9591e+08..4.96549e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_set_current",
            "value": 32780957873.7,
            "range": "3.27477e+10..3.28078e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.27477e+10..3.28078e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_mixed/tiered",
            "value": 170370669.37662336,
            "range": "1.70283e+08..1.70438e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.70283e+08..1.70438e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "script_dispatch/baseline",
            "value": 341695.3297278912,
            "range": "341022..342539",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 341022..342539 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire0",
            "value": 1214021.0652912622,
            "range": "1.21367e+06..1.21437e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.21367e+06..1.21437e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire20",
            "value": 2862420.1125714285,
            "range": "2.85825e+06..2.86622e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.85825e+06..2.86622e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire5",
            "value": 1820372.2578181818,
            "range": "1.81909e+06..1.82163e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.81909e+06..1.82163e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "sgr/process/bold_color",
            "value": 32.16531420026386,
            "range": "32.1544..32.1775",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 32.1544..32.1775 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/color_256",
            "value": 51.32798344592806,
            "range": "51.3003..51.361",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 51.3003..51.361 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/reset",
            "value": 21.377042163848323,
            "range": "21.3629..21.3924",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.3629..21.3924 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/simple_color",
            "value": 21.616665465248598,
            "range": "21.6131..21.6199",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.6131..21.6199 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/truecolor",
            "value": 92.53202130747646,
            "range": "92.5157..92.5479",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 92.5157..92.5479 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "spatial_query/connections/viewport_full/10k",
            "value": 82497.72781435154,
            "range": "82449.4..82548.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 82449.4..82548.2 ns/iter\nThroughput input: {\"Elements\": 19802}"
          },
          {
            "name": "spatial_query/connections/viewport_full/50k",
            "value": 440421.49868073873,
            "range": "440287..440567",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 440287..440567 ns/iter\nThroughput input: {\"Elements\": 99557}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/10k",
            "value": 19201.04593233372,
            "range": "19173.4..19231.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 19173.4..19231.9 ns/iter\nThroughput input: {\"Elements\": 4416}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/50k",
            "value": 92559.81756431612,
            "range": "92528.1..92590.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 92528.1..92590.6 ns/iter\nThroughput input: {\"Elements\": 21021}"
          },
          {
            "name": "spatial_query/connections/viewport_small/10k",
            "value": 2922.1980049446165,
            "range": "2907.31..2935.18",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2907.31..2935.18 ns/iter\nThroughput input: {\"Elements\": 576}"
          },
          {
            "name": "spatial_query/connections/viewport_small/50k",
            "value": 10981.948764601153,
            "range": "10951.1..11014",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 10951.1..11014 ns/iter\nThroughput input: {\"Elements\": 2359}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/10k",
            "value": 38884.90198834951,
            "range": "38875.9..38893.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 38875.9..38893.2 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/50k",
            "value": 199285.0615262321,
            "range": "199247..199320",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 199247..199320 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/10k",
            "value": 9124.957243198982,
            "range": "9120.05..9129.73",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 9120.05..9129.73 ns/iter\nThroughput input: {\"Elements\": 2070}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/50k",
            "value": 39675.53904487641,
            "range": "39614.5..39731.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 39614.5..39731.3 ns/iter\nThroughput input: {\"Elements\": 10306}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/10k",
            "value": 1079.7380520700779,
            "range": "1074.81..1085.15",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1074.81..1085.15 ns/iter\nThroughput input: {\"Elements\": 240}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/50k",
            "value": 4996.777396513081,
            "range": "4981.94..5010.33",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4981.94..5010.33 ns/iter\nThroughput input: {\"Elements\": 1146}"
          },
          {
            "name": "styled_line/new_no_raw/long_plain",
            "value": 20.680365026241486,
            "range": "20.673..20.6881",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.673..20.6881 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/long_styled",
            "value": 22.45136651253486,
            "range": "22.445..22.457",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 22.445..22.457 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/short_plain",
            "value": 19.749055094102157,
            "range": "19.747..19.7511",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 19.747..19.7511 ns/iter\nThroughput input: {\"Bytes\": 40}"
          },
          {
            "name": "styled_line/new_with_raw/long_plain",
            "value": 114.85466363226469,
            "range": "114.838..114.876",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 114.838..114.876 ns/iter\nThroughput input: {\"Bytes\": 400}"
          },
          {
            "name": "styled_line/new_with_raw/long_styled",
            "value": 123.13436019153116,
            "range": "123.091..123.185",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 123.091..123.185 ns/iter\nThroughput input: {\"Bytes\": 464}"
          },
          {
            "name": "styled_line/new_with_raw/short_plain",
            "value": 39.36150492050489,
            "range": "39.3557..39.3679",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 39.3557..39.3679 ns/iter\nThroughput input: {\"Bytes\": 80}"
          },
          {
            "name": "telnet_receive/ansi_light",
            "value": 287905.65738081565,
            "range": "287573..288254",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 287573..288254 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "telnet_receive/iac_dense",
            "value": 4403051.411403508,
            "range": "4.40174e+06..4.40471e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.40174e+06..4.40471e+06 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "to_spans/by_span_count/1",
            "value": 63.50746951351806,
            "range": "63.492..63.5256",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 63.492..63.5256 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "to_spans/by_span_count/32",
            "value": 1315.6261408542084,
            "range": "1315.41..1315.85",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1315.41..1315.85 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "to_spans/by_span_count/8",
            "value": 362.9502337759306,
            "range": "362.905..362.993",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 362.905..362.993 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "trigger_verbs/empty",
            "value": 1095041.1780701755,
            "range": "1.09447e+06..1.09569e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.09447e+06..1.09569e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/gag",
            "value": 1112130.8679372198,
            "range": "1.11086e+06..1.11341e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.11086e+06..1.11341e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/highlight",
            "value": 1254515.8742499999,
            "range": "1.25294e+06..1.25571e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.25294e+06..1.25571e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/read_echo",
            "value": 1451686.0484057972,
            "range": "1.45095e+06..1.45252e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.45095e+06..1.45252e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "upsert_room/single/10k",
            "value": 997848.9834307993,
            "range": "995560..999438",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 995560..999438 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/1k",
            "value": 906773.443286219,
            "range": "902045..911591",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 902045..911591 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/50k",
            "value": 1383502.651081081,
            "range": "1.37966e+06..1.38682e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.37966e+06..1.38682e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/1",
            "value": 1010753.267827869,
            "range": "1.01016e+06..1.01141e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.01016e+06..1.01141e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/16",
            "value": 1192103.477672209,
            "range": "1.18664e+06..1.19729e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.18664e+06..1.19729e+06 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "upsert_rooms/batch_10k/256",
            "value": 3375314.089261745,
            "range": "3.35383e+06..3.3963e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.35383e+06..3.3963e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "write_and_flush/J16/W8_mixed",
            "value": 17561.908487681438,
            "range": "17557.9..17566",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 02fbb94e87809e594555cc350d0e8370c09ce529\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 17557.9..17566 ns/iter\nThroughput input: {\"Elements\": 16}"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "ping@walter.dev",
            "name": "wbk",
            "username": "wbk"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "03195a2c1c89887fe25d21eda821eadbe2efa494",
          "message": "Merge pull request #1 from GTanger/fix/connection-io\n\nfix(core): harden socket I/O handling",
          "timestamp": "2026-07-23T17:12:56-07:00",
          "tree_id": "77dd0c7672f6982b5cff13f57dee670581ccf34d",
          "url": "https://github.com/smudgy-mud/smudgy/commit/03195a2c1c89887fe25d21eda821eadbe2efa494"
        },
        "date": 1784854638161,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "atlas_build/cold/10k",
            "value": 58758066.311111115,
            "range": "5.86727e+07..5.88487e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.86727e+07..5.88487e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "atlas_build/cold/1k",
            "value": 1324984.395778364,
            "range": "1.32285e+06..1.32717e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.32285e+06..1.32717e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "atlas_build/cold/50k",
            "value": 1350571817.5,
            "range": "1.34857e+09..1.35317e+09",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.34857e+09..1.35317e+09 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "automap_step/create_room/100k",
            "value": 2030822.9282094594,
            "range": "1.78408e+06..2.28258e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.78408e+06..2.28258e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "automap_step/create_room/10k",
            "value": 2424577.393978102,
            "range": "2.23148e+06..2.61018e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.23148e+06..2.61018e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "build/aho_corasick",
            "value": 6381995.643982684,
            "range": "6.37307e+06..6.39363e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6.37307e+06..6.39363e+06 ns/iter"
          },
          {
            "name": "build/regex_filtered",
            "value": 120163001.57402597,
            "range": "1.19977e+08..1.20435e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.19977e+08..1.20435e+08 ns/iter"
          },
          {
            "name": "build/regex_set",
            "value": 53406059.38441558,
            "range": "5.33182e+07..5.34904e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.33182e+07..5.34904e+07 ns/iter"
          },
          {
            "name": "build/tiered",
            "value": 32672392.494372293,
            "range": "3.26418e+07..3.26983e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.26418e+07..3.26983e+07 ns/iter"
          },
          {
            "name": "catalogue/sample/dynamic/small",
            "value": 93.62263574515912,
            "range": "93.5797..93.671",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 93.5797..93.671 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/large",
            "value": 6425.847792680936,
            "range": "6422.05..6429.63",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6422.05..6429.63 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/small",
            "value": 297.2944257644703,
            "range": "297.097..297.494",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 297.097..297.494 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/large",
            "value": 87.76363091154802,
            "range": "87.739..87.7862",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 87.739..87.7862 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/small",
            "value": 85.13249182657412,
            "range": "85.0945..85.1688",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 85.0945..85.1688 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_128",
            "value": 72735.59215752919,
            "range": "72707..72765.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 72707..72765.4 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_512",
            "value": 305445.71361090586,
            "range": "305355..305524",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 305355..305524 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_8",
            "value": 4407.956818383331,
            "range": "4405.74..4410.21",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4405.74..4410.21 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_4096",
            "value": 4606.943491097866,
            "range": "4605.33..4608.37",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4605.33..4608.37 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_64",
            "value": 4125.518226369488,
            "range": "4123.34..4127.83",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4123.34..4127.83 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_65536",
            "value": 4840.746793706178,
            "range": "4837.01..4844.47",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4837.01..4844.47 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "churn_packet/clean",
            "value": 83881.05206735579,
            "range": "83744.9..84020.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 83744.9..84020.3 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20",
            "value": 75133355.3,
            "range": "7.4756e+07..7.58219e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7.4756e+07..7.58219e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20_x4pkg",
            "value": 77532928.04285714,
            "range": "7.74302e+07..7.76312e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7.74302e+07..7.76312e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/toggle20",
            "value": 94039.68000376578,
            "range": "93840.6..94316.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 93840.6..94316.9 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_residue/full/10000",
            "value": 339458429.9,
            "range": "3.38659e+08..3.40207e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.38659e+08..3.40207e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/1000",
            "value": 333995418.05,
            "range": "3.33517e+08..3.34428e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.33517e+08..3.34428e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/5000",
            "value": 291167110.6,
            "range": "2.90693e+08..2.91592e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.90693e+08..2.91592e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/1000",
            "value": 337022477.65,
            "range": "3.36493e+08..3.37578e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.36493e+08..3.37578e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/5000",
            "value": 296849605.9,
            "range": "2.96103e+08..2.9761e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.96103e+08..2.9761e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_absent/25",
            "value": 235591001.06666666,
            "range": "2.32534e+08..2.38653e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.32534e+08..2.38653e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_disabled/25",
            "value": 279127703.15,
            "range": "2.78615e+08..2.79649e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.78615e+08..2.79649e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "creator_parse/package",
            "value": 261.2706893576691,
            "range": "261.114..261.418",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 261.114..261.418 ns/iter"
          },
          {
            "name": "creator_parse/user",
            "value": 44.6211470875108,
            "range": "44.599..44.6423",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 44.599..44.6423 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/1000",
            "value": 14180948.327777779,
            "range": "1.41721e+07..1.41884e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.41721e+07..1.41884e+07 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/10000",
            "value": 53392810,
            "range": "5.33075e+07..5.349e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.33075e+07..5.349e+07 ns/iter"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/bytes",
            "value": 342272099.75,
            "range": "3.41826e+08..3.42702e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.41826e+08..3.42702e+08 ns/iter\nThroughput input: {\"BytesDecimal\": 16269045}"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/lines",
            "value": 331530054.75,
            "range": "3.30869e+08..3.32127e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.30869e+08..3.32127e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "extend_line/at_capacity",
            "value": 120179.80048053821,
            "range": "120136..120218",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 120136..120218 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag16",
            "value": 18015974.218518518,
            "range": "1.79962e+07..1.80347e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.79962e+07..1.80347e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag4",
            "value": 3359486.954330709,
            "range": "3.35824e+06..3.3607e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.35824e+06..3.3607e+06 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/whole_lines",
            "value": 66076.7989786856,
            "range": "66027.9..66126.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66027.9..66126.1 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "flush_coalesced/J1/W0",
            "value": 142.41820616854682,
            "range": "142.253..142.601",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 142.253..142.601 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W64",
            "value": 6233.341916372542,
            "range": "6230.44..6236.28",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6230.44..6236.28 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W8",
            "value": 792.085114121229,
            "range": "791.623..792.544",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 791.623..792.544 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J128/W0",
            "value": 6304.09790194182,
            "range": "6302.05..6305.99",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6302.05..6305.99 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W64",
            "value": 152712.99712556732,
            "range": "152682..152743",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 152682..152743 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W8",
            "value": 23070.076779489485,
            "range": "23064.6..23076.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 23064.6..23076.3 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J16/W0",
            "value": 639.5875848117582,
            "range": "638.862..640.464",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 638.862..640.464 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W64",
            "value": 23657.754268634413,
            "range": "23628.3..23687.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 23628.3..23687.6 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W8",
            "value": 3730.8510485012216,
            "range": "3726.88..3734.72",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3726.88..3734.72 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J1/W64",
            "value": 6963.026086349864,
            "range": "6956.37..6968.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6956.37..6968.8 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J1/W8",
            "value": 956.1881751128691,
            "range": "954.916..957.545",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 954.916..957.545 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J128/W64",
            "value": 526636.1463247862,
            "range": "526323..526906",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 526323..526906 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J128/W8",
            "value": 67651.7246628131,
            "range": "67604.5..67696.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 67604.5..67696.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J16/W64",
            "value": 67260.2450197455,
            "range": "67189.9..67320.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 67189.9..67320.7 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J16/W8",
            "value": 8939.485062784603,
            "range": "8932.75..8944.36",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8932.75..8944.36 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J1/W0",
            "value": 143.75864848346617,
            "range": "143.544..143.955",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 143.544..143.955 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W64",
            "value": 6762.064333325851,
            "range": "6758.6..6765.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6758.6..6765.1 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W8",
            "value": 910.805822107289,
            "range": "910.149..911.471",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 910.149..911.471 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J128/W0",
            "value": 4518.570240928018,
            "range": "4517.89..4519.24",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4517.89..4519.24 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W64",
            "value": 871963.7495522387,
            "range": "871801..872152",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 871801..872152 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W8",
            "value": 111078.06831382748,
            "range": "110929..111223",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 110929..111223 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J16/W0",
            "value": 657.9459588858238,
            "range": "657.281..658.458",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 657.281..658.458 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W64",
            "value": 106001.46368286444,
            "range": "105921..106068",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 105921..106068 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W8",
            "value": 13602.813537782913,
            "range": "13594.6..13612.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 13594.6..13612.1 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "fold/lower",
            "value": 20.58170628514214,
            "range": "20.5779..20.5858",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.5779..20.5858 ns/iter"
          },
          {
            "name": "fold/mixed",
            "value": 20.585535002573558,
            "range": "20.5817..20.5891",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.5817..20.5891 ns/iter"
          },
          {
            "name": "follow/find_room_by_external_id/100k",
            "value": 92.1621866660753,
            "range": "92.0635..92.2546",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 92.0635..92.2546 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "follow/find_room_by_external_id/10k",
            "value": 94.92750801326949,
            "range": "94.8552..95.004",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 94.8552..95.004 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "frame_proxy/10k",
            "value": 164033.07364290385,
            "range": "163984..164078",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 163984..164078 ns/iter\nThroughput input: {\"Elements\": 32430}"
          },
          {
            "name": "identification/by_title_and_description/10k",
            "value": 15448.01975981524,
            "range": "15445.1..15450.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 15445.1..15450.9 ns/iter\nThroughput input: {\"Elements\": 44}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy",
            "value": 531690972.2,
            "range": "5.30716e+08..5.32607e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.30716e+08..5.32607e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy/no_raw",
            "value": 472477482.65,
            "range": "4.6217e+08..4.82157e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.6217e+08..4.82157e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_light",
            "value": 282726402.15,
            "range": "2.81929e+08..2.83488e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.81929e+08..2.83488e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/ansi_light/no_raw",
            "value": 234728117.9,
            "range": "2.34325e+08..2.35127e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.34325e+08..2.35127e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/iac_dense",
            "value": 284466235.8,
            "range": "2.84073e+08..2.84848e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.84073e+08..2.84848e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "ingest_pipeline/iac_dense/no_raw",
            "value": 233796809.0666667,
            "range": "2.33301e+08..2.34293e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.33301e+08..2.34293e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "interop_delivery/emit_cross_isolate/S1",
            "value": 83665.70385846672,
            "range": "83567.2..83749.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 83567.2..83749.3 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S1",
            "value": 84995.92973701956,
            "range": "84791.4..85188.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 84791.4..85188.5 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S64",
            "value": 3849085.379230769,
            "range": "3.84512e+06..3.85214e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.84512e+06..3.85214e+06 ns/iter\nThroughput input: {\"Elements\": 2048}"
          },
          {
            "name": "interop_delivery/emit_fanout/S8",
            "value": 503484.01097683795,
            "range": "502451..504628",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 502451..504628 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P16k",
            "value": 1630886.958957655,
            "range": "1.63036e+06..1.63146e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.63036e+06..1.63146e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P64",
            "value": 426914.55387894297,
            "range": "426843..426991",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 426843..426991 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W64",
            "value": 1831692.0212454211,
            "range": "1.82938e+06..1.83406e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.82938e+06..1.83406e+06 ns/iter\nThroughput input: {\"Elements\": 1024}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W8",
            "value": 286932.0285140563,
            "range": "286830..287036",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 286830..287036 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_delivery/watch_per_write/W8",
            "value": 437194.0754155731,
            "range": "436793..437590",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 436793..437590 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_ops/package/emit128",
            "value": 28235.88739095956,
            "range": "28173.7..28296.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 28173.7..28296.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/get128",
            "value": 70466.49723934979,
            "range": "70099.8..70940.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 70099.8..70940.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set128",
            "value": 99624.02834723877,
            "range": "99362.7..99872.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 99362.7..99872.7 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set_per_turn64",
            "value": 465052.5133953489,
            "range": "464668..465435",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 464668..465435 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_ops/user/emit128",
            "value": 27772.854056018336,
            "range": "27708.9..27847",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 27708.9..27847 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/get128",
            "value": 65608.44283097854,
            "range": "65464.3..65745.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 65464.3..65745.1 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set128",
            "value": 74532.73677123411,
            "range": "74450.7..74638.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 74450.7..74638.3 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set_per_turn64",
            "value": 418230.10016708437,
            "range": "418076..418383",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 418076..418383 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_read/keys_32k",
            "value": 78779.4943518959,
            "range": "78432.9..79119.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 78432.9..79119.9 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_read/materialize_32k",
            "value": 12629701.565,
            "range": "1.24588e+07..1.27888e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.24588e+07..1.27888e+07 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "interop_read/value_leaf/1k",
            "value": 74975.52216054013,
            "range": "74841..75128.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 74841..75128.2 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/1m",
            "value": 74581.33117481062,
            "range": "74439.6..74713.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 74439.6..74713.7 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth1",
            "value": 74314.85731835206,
            "range": "73954.9..74705",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73954.9..74705 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth4",
            "value": 73950.38576923076,
            "range": "73825.7..74056.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73825.7..74056.7 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "line_operations/replace_and_highlight",
            "value": 9452.861111665658,
            "range": "9451.13..9454.64",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 9451.13..9454.64 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "path_parse/bracket",
            "value": 75.22975891388187,
            "range": "75.2167..75.2422",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 75.2167..75.2422 ns/iter"
          },
          {
            "name": "path_parse/depth1",
            "value": 48.60621427387784,
            "range": "48.5928..48.6192",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 48.5928..48.6192 ns/iter"
          },
          {
            "name": "path_parse/depth4",
            "value": 86.87061055366341,
            "range": "86.8487..86.8916",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 86.8487..86.8916 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/10k",
            "value": 512398.5537037036,
            "range": "512321..512471",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 512321..512471 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/50k",
            "value": 443829.18232682056,
            "range": "443749..443910",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 443749..443910 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/10k",
            "value": 3064008.7902439027,
            "range": "3.06144e+06..3.06612e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.06144e+06..3.06612e+06 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/50k",
            "value": 27145979.578947373,
            "range": "2.68736e+07..2.7367e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.68736e+07..2.7367e+07 ns/iter"
          },
          {
            "name": "pathfinding/path_across/10k",
            "value": 2774964.720555556,
            "range": "2.77092e+06..2.77874e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.77092e+06..2.77874e+06 ns/iter"
          },
          {
            "name": "pathfinding/path_across/50k",
            "value": 20882232.545833334,
            "range": "2.08349e+07..2.09316e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.08349e+07..2.09316e+07 ns/iter"
          },
          {
            "name": "per_emit_composite/package",
            "value": 412.74377933023834,
            "range": "412.623..412.849",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 412.623..412.849 ns/iter"
          },
          {
            "name": "per_set_composite/package",
            "value": 355.28100897964896,
            "range": "355.101..355.456",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 355.101..355.456 ns/iter"
          },
          {
            "name": "per_set_composite/user",
            "value": 137.6162551077965,
            "range": "137.586..137.645",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 137.586..137.645 ns/iter"
          },
          {
            "name": "producer_parse/package",
            "value": 46.15476548523877,
            "range": "46.1474..46.1618",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 46.1474..46.1618 ns/iter"
          },
          {
            "name": "producer_parse/user",
            "value": 3.8386746502309483,
            "range": "3.83717..3.84022",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.83717..3.84022 ns/iter"
          },
          {
            "name": "rebuild/room_connections/10k",
            "value": 28104341.304999996,
            "range": "2.77816e+07..2.84277e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.77816e+07..2.84277e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "rebuild/room_connections/1k",
            "value": 1793381.4926573425,
            "range": "1.78311e+06..1.8028e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.78311e+06..1.8028e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "rebuild/room_connections/50k",
            "value": 195992872.6,
            "range": "1.92299e+08..1.99422e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.92299e+08..1.99422e+08 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "scan_literals/aho_corasick_leftmost",
            "value": 15198277.058008658,
            "range": "1.51815e+07..1.52259e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.51815e+07..1.52259e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/aho_corasick_overlapping",
            "value": 17831113.192640692,
            "range": "1.78039e+07..1.78449e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.78039e+07..1.78449e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_filtered",
            "value": 400530984.5,
            "range": "4.00426e+08..4.00649e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.00426e+08..4.00649e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_set_current",
            "value": 32156161248.8,
            "range": "3.21456e+10..3.21667e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.21456e+10..3.21667e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_literals/tiered",
            "value": 50476985.07272727,
            "range": "5.04279e+07..5.05241e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.04279e+07..5.05241e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_filtered",
            "value": 496141263.5,
            "range": "4.95593e+08..4.96619e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.95593e+08..4.96619e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_set_current",
            "value": 32848139147,
            "range": "3.2827e+10..3.28667e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.2827e+10..3.28667e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_mixed/tiered",
            "value": 168423121.92987013,
            "range": "1.68268e+08..1.68691e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.68268e+08..1.68691e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "script_dispatch/baseline",
            "value": 346378.9056010929,
            "range": "345719..346925",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 345719..346925 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire0",
            "value": 1203089.526923077,
            "range": "1.20219e+06..1.20397e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.20219e+06..1.20397e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire20",
            "value": 2854276.361714286,
            "range": "2.85144e+06..2.8568e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.85144e+06..2.8568e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire5",
            "value": 1820637.3599999999,
            "range": "1.81756e+06..1.82559e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.81756e+06..1.82559e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "sgr/process/bold_color",
            "value": 32.34659223143125,
            "range": "32.3391..32.3537",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 32.3391..32.3537 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/color_256",
            "value": 51.36991912769227,
            "range": "51.3583..51.3814",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 51.3583..51.3814 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/reset",
            "value": 21.320574219324385,
            "range": "21.3165..21.3246",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.3165..21.3246 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/simple_color",
            "value": 21.627777690636503,
            "range": "21.6231..21.6324",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.6231..21.6324 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/truecolor",
            "value": 93.06403403971058,
            "range": "93.0412..93.0849",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 93.0412..93.0849 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "spatial_query/connections/viewport_full/10k",
            "value": 81898.7809422542,
            "range": "81884.9..81912.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 81884.9..81912.1 ns/iter\nThroughput input: {\"Elements\": 19802}"
          },
          {
            "name": "spatial_query/connections/viewport_full/50k",
            "value": 441310.66390114743,
            "range": "441205..441419",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 441205..441419 ns/iter\nThroughput input: {\"Elements\": 99557}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/10k",
            "value": 19100.625065771914,
            "range": "19086..19118",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 19086..19118 ns/iter\nThroughput input: {\"Elements\": 4416}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/50k",
            "value": 92443.61767424663,
            "range": "92377.1..92488.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 92377.1..92488.7 ns/iter\nThroughput input: {\"Elements\": 21021}"
          },
          {
            "name": "spatial_query/connections/viewport_small/10k",
            "value": 3121.149503098986,
            "range": "3117.29..3124.71",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3117.29..3124.71 ns/iter\nThroughput input: {\"Elements\": 576}"
          },
          {
            "name": "spatial_query/connections/viewport_small/50k",
            "value": 11490.345835979315,
            "range": "11475.7..11504.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 11475.7..11504.9 ns/iter\nThroughput input: {\"Elements\": 2359}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/10k",
            "value": 38677.16036043005,
            "range": "38668.6..38684.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 38668.6..38684.2 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/50k",
            "value": 194047.04014740107,
            "range": "194012..194082",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 194012..194082 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/10k",
            "value": 9167.89513939571,
            "range": "9144.34..9190.91",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 9144.34..9190.91 ns/iter\nThroughput input: {\"Elements\": 2070}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/50k",
            "value": 39637.58294974166,
            "range": "39413.6..39846",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 39413.6..39846 ns/iter\nThroughput input: {\"Elements\": 10306}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/10k",
            "value": 1110.3417776323297,
            "range": "1104.76..1115.89",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1104.76..1115.89 ns/iter\nThroughput input: {\"Elements\": 240}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/50k",
            "value": 4873.5246856463855,
            "range": "4863.37..4882.77",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4863.37..4882.77 ns/iter\nThroughput input: {\"Elements\": 1146}"
          },
          {
            "name": "styled_line/new_no_raw/long_plain",
            "value": 20.68187456422013,
            "range": "20.6761..20.6873",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.6761..20.6873 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/long_styled",
            "value": 22.683564554045986,
            "range": "22.6786..22.6886",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 22.6786..22.6886 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/short_plain",
            "value": 19.9935619681943,
            "range": "19.9874..19.9996",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 19.9874..19.9996 ns/iter\nThroughput input: {\"Bytes\": 40}"
          },
          {
            "name": "styled_line/new_with_raw/long_plain",
            "value": 104.9705707492625,
            "range": "104.762..105.195",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 104.762..105.195 ns/iter\nThroughput input: {\"Bytes\": 400}"
          },
          {
            "name": "styled_line/new_with_raw/long_styled",
            "value": 125.34054210592242,
            "range": "125.259..125.428",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 125.259..125.428 ns/iter\nThroughput input: {\"Bytes\": 464}"
          },
          {
            "name": "styled_line/new_with_raw/short_plain",
            "value": 38.87117808512676,
            "range": "38.8563..38.8865",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 38.8563..38.8865 ns/iter\nThroughput input: {\"Bytes\": 80}"
          },
          {
            "name": "telnet_receive/ansi_light",
            "value": 288174.85640138405,
            "range": "288031..288297",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 288031..288297 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "telnet_receive/iac_dense",
            "value": 4493987.9321428565,
            "range": "4.49334e+06..4.49463e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.49334e+06..4.49463e+06 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "to_spans/by_span_count/1",
            "value": 63.33616444924333,
            "range": "63.323..63.3485",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 63.323..63.3485 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "to_spans/by_span_count/32",
            "value": 1317.9422075153354,
            "range": "1317.67..1318.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1317.67..1318.2 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "to_spans/by_span_count/8",
            "value": 364.7200434754652,
            "range": "364.639..364.801",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 364.639..364.801 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "trigger_verbs/empty",
            "value": 1047705.7368972745,
            "range": "1.04701e+06..1.0485e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.04701e+06..1.0485e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/gag",
            "value": 1067682.0922746782,
            "range": "1.06624e+06..1.06883e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.06624e+06..1.06883e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/highlight",
            "value": 1204394.3152173914,
            "range": "1.20272e+06..1.20618e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.20272e+06..1.20618e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/read_echo",
            "value": 1405294.378873239,
            "range": "1.40203e+06..1.40935e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.40203e+06..1.40935e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "upsert_room/single/10k",
            "value": 1013411.1163385827,
            "range": "1.00612e+06..1.02021e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.00612e+06..1.02021e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/1k",
            "value": 898666.4813471502,
            "range": "895489..901853",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 895489..901853 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/50k",
            "value": 1307918.91713555,
            "range": "1.30705e+06..1.30881e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.30705e+06..1.30881e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/1",
            "value": 1029446.1265010353,
            "range": "1.02814e+06..1.031e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.02814e+06..1.031e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/16",
            "value": 1219426.566828087,
            "range": "1.21579e+06..1.22285e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.21579e+06..1.22285e+06 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "upsert_rooms/batch_10k/256",
            "value": 3372923.304,
            "range": "3.35886e+06..3.38516e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.35886e+06..3.38516e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "write_and_flush/J16/W8_mixed",
            "value": 17828.627271433652,
            "range": "17810.4..17845",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 03195a2c1c89887fe25d21eda821eadbe2efa494\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 17810.4..17845 ns/iter\nThroughput input: {\"Elements\": 16}"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "ping@walter.dev",
            "name": "wbk",
            "username": "wbk"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "ce813f4506945837c4e4c66531c556912556bc71",
          "message": "Merge pull request #3 from GTanger/perf/box-large-ui-messages\n\nperf(ui): box oversized message payloads",
          "timestamp": "2026-07-23T19:12:17-07:00",
          "tree_id": "6a9be95e85aa8c813c624646bdd567c3e0df5d91",
          "url": "https://github.com/smudgy-mud/smudgy/commit/ce813f4506945837c4e4c66531c556912556bc71"
        },
        "date": 1784861944233,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "atlas_build/cold/10k",
            "value": 58333329.755555555,
            "range": "5.81993e+07..5.84609e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.81993e+07..5.84609e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "atlas_build/cold/1k",
            "value": 1293765.995854922,
            "range": "1.2933e+06..1.29421e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.2933e+06..1.29421e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "atlas_build/cold/50k",
            "value": 1342653341.5,
            "range": "1.34005e+09..1.34555e+09",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.34005e+09..1.34555e+09 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "automap_step/create_room/100k",
            "value": 2013431.3761996166,
            "range": "1.79025e+06..2.24037e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.79025e+06..2.24037e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "automap_step/create_room/10k",
            "value": 2487606.572180451,
            "range": "2.29603e+06..2.67206e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.29603e+06..2.67206e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "build/aho_corasick",
            "value": 6308331.923290043,
            "range": "6.30575e+06..6.31034e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6.30575e+06..6.31034e+06 ns/iter"
          },
          {
            "name": "build/regex_filtered",
            "value": 120483564.30909091,
            "range": "1.20323e+08..1.20684e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.20323e+08..1.20684e+08 ns/iter"
          },
          {
            "name": "build/regex_set",
            "value": 52162529.62337662,
            "range": "5.21175e+07..5.21876e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.21175e+07..5.21876e+07 ns/iter"
          },
          {
            "name": "build/tiered",
            "value": 32850752.64242424,
            "range": "3.28115e+07..3.288e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.28115e+07..3.288e+07 ns/iter"
          },
          {
            "name": "catalogue/sample/dynamic/small",
            "value": 92.85014489424309,
            "range": "92.8309..92.871",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 92.8309..92.871 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/large",
            "value": 6473.843048541528,
            "range": "6472.63..6474.97",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6472.63..6474.97 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/small",
            "value": 300.6662786350745,
            "range": "300.578..300.76",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 300.578..300.76 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/large",
            "value": 89.375139348686,
            "range": "89.3603..89.3904",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 89.3603..89.3904 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/small",
            "value": 85.41337592507217,
            "range": "85.3484..85.4795",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 85.3484..85.4795 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_128",
            "value": 71801.94701471427,
            "range": "71773.4..71830.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 71773.4..71830.9 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_512",
            "value": 302882.82749076403,
            "range": "302807..302953",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 302807..302953 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_8",
            "value": 4383.490761723289,
            "range": "4381.27..4385.71",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4381.27..4385.71 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_4096",
            "value": 4716.283131463885,
            "range": "4714.94..4717.52",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4714.94..4717.52 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_64",
            "value": 4198.917102081755,
            "range": "4195.57..4202.33",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4195.57..4202.33 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_65536",
            "value": 4417.98289991626,
            "range": "4414.76..4421.41",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4414.76..4421.41 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "churn_packet/clean",
            "value": 79545.39127098322,
            "range": "79387.7..79771.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 79387.7..79771.9 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20",
            "value": 77436931.62857142,
            "range": "7.73742e+07..7.74981e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7.73742e+07..7.74981e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20_x4pkg",
            "value": 80261030.2,
            "range": "8.01357e+07..8.03883e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8.01357e+07..8.03883e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/toggle20",
            "value": 93322.72031454783,
            "range": "93107.4..93494.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 93107.4..93494.4 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_residue/full/10000",
            "value": 325013854.8,
            "range": "3.24161e+08..3.25949e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.24161e+08..3.25949e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/1000",
            "value": 326054731.65,
            "range": "3.25667e+08..3.26426e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.25667e+08..3.26426e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/5000",
            "value": 285869554.5,
            "range": "2.85349e+08..2.86372e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.85349e+08..2.86372e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/1000",
            "value": 331224479.1,
            "range": "3.30614e+08..3.31853e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.30614e+08..3.31853e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/5000",
            "value": 292918147.1,
            "range": "2.92385e+08..2.93443e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.92385e+08..2.93443e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_absent/25",
            "value": 230735621.8333333,
            "range": "2.27682e+08..2.33813e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.27682e+08..2.33813e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_disabled/25",
            "value": 271426718.45,
            "range": "2.71164e+08..2.71692e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.71164e+08..2.71692e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "creator_parse/package",
            "value": 261.45403441809634,
            "range": "261.396..261.512",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 261.396..261.512 ns/iter"
          },
          {
            "name": "creator_parse/user",
            "value": 48.646638304549,
            "range": "48.6343..48.6583",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 48.6343..48.6583 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/1000",
            "value": 14133685.424999997,
            "range": "1.41246e+07..1.41438e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.41246e+07..1.41438e+07 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/10000",
            "value": 52983739.55,
            "range": "5.29438e+07..5.30272e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.29438e+07..5.30272e+07 ns/iter"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/bytes",
            "value": 340881793.7,
            "range": "3.40006e+08..3.41744e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.40006e+08..3.41744e+08 ns/iter\nThroughput input: {\"BytesDecimal\": 16269045}"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/lines",
            "value": 327305014.9,
            "range": "3.26191e+08..3.28488e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.26191e+08..3.28488e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "extend_line/at_capacity",
            "value": 120386.76000963621,
            "range": "120348..120425",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 120348..120425 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag16",
            "value": 17439130.40714286,
            "range": "1.74307e+07..1.74472e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.74307e+07..1.74472e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag4",
            "value": 3299199.8938461537,
            "range": "3.29752e+06..3.30059e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.29752e+06..3.30059e+06 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/whole_lines",
            "value": 65514.03317180617,
            "range": "65507.8..65521.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 65507.8..65521.5 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "flush_coalesced/J1/W0",
            "value": 144.19674181046273,
            "range": "144.012..144.354",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 144.012..144.354 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W64",
            "value": 6177.209616623712,
            "range": "6175.66..6178.67",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6175.66..6178.67 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W8",
            "value": 736.8453688517251,
            "range": "736.657..737.048",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 736.657..737.048 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J128/W0",
            "value": 6131.898444687842,
            "range": "6129.2..6134.62",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6129.2..6134.62 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W64",
            "value": 154203.4835047619,
            "range": "154173..154231",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 154173..154231 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W8",
            "value": 22884.675962493104,
            "range": "22861..22906",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 22861..22906 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J16/W0",
            "value": 792.7051849469283,
            "range": "791.133..794.195",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 791.133..794.195 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W64",
            "value": 22925.510765829396,
            "range": "22904.6..22950.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 22904.6..22950.5 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W8",
            "value": 3606.9341831901993,
            "range": "3602.26..3613.41",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3602.26..3613.41 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J1/W64",
            "value": 6586.10668933587,
            "range": "6585.15..6587.36",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6585.15..6587.36 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J1/W8",
            "value": 867.0811181379617,
            "range": "866.776..867.408",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 866.776..867.408 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J128/W64",
            "value": 519861.45959252975,
            "range": "519768..519983",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 519768..519983 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J128/W8",
            "value": 66717.32184281843,
            "range": "66706.7..66729.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66706.7..66729.5 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J16/W64",
            "value": 66415.35575586447,
            "range": "66400.1..66431.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66400.1..66431.7 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J16/W8",
            "value": 9007.908093506492,
            "range": "8985.42..9028.21",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8985.42..9028.21 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J1/W0",
            "value": 144.2205942010092,
            "range": "144.097..144.343",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 144.097..144.343 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W64",
            "value": 6459.226355444911,
            "range": "6457.28..6461.23",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6457.28..6461.23 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W8",
            "value": 840.7076363885747,
            "range": "840.48..840.933",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 840.48..840.933 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J128/W0",
            "value": 4494.366426822936,
            "range": "4492.78..4495.84",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4492.78..4495.84 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W64",
            "value": 849066.8226470588,
            "range": "848934..849187",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 848934..849187 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W8",
            "value": 108655.38450404428,
            "range": "108436..108898",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 108436..108898 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J16/W0",
            "value": 773.8049951616372,
            "range": "773.502..774.112",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 773.502..774.112 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W64",
            "value": 103787.05610108303,
            "range": "103781..103793",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 103781..103793 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W8",
            "value": 13244.214747864138,
            "range": "13241.8..13246.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 13241.8..13246.4 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "fold/lower",
            "value": 20.938767911801698,
            "range": "20.9353..20.9421",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.9353..20.9421 ns/iter"
          },
          {
            "name": "fold/mixed",
            "value": 20.943274658010196,
            "range": "20.9403..20.9461",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.9403..20.9461 ns/iter"
          },
          {
            "name": "follow/find_room_by_external_id/100k",
            "value": 92.10088328273577,
            "range": "92.0088..92.2465",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 92.0088..92.2465 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "follow/find_room_by_external_id/10k",
            "value": 94.32065355448731,
            "range": "94.294..94.35",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 94.294..94.35 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "frame_proxy/10k",
            "value": 163120.4035167698,
            "range": "162963..163305",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 162963..163305 ns/iter\nThroughput input: {\"Elements\": 32430}"
          },
          {
            "name": "identification/by_title_and_description/10k",
            "value": 15348.974489141392,
            "range": "15324.4..15368.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 15324.4..15368.4 ns/iter\nThroughput input: {\"Elements\": 44}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy",
            "value": 533115639,
            "range": "5.32522e+08..5.33627e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.32522e+08..5.33627e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy/no_raw",
            "value": 469721761.4,
            "range": "4.67338e+08..4.73408e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.67338e+08..4.73408e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_light",
            "value": 274940106.55,
            "range": "2.74457e+08..2.75372e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.74457e+08..2.75372e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/ansi_light/no_raw",
            "value": 229380579.7333333,
            "range": "2.29057e+08..2.2976e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.29057e+08..2.2976e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/iac_dense",
            "value": 283387035.05,
            "range": "2.82625e+08..2.84276e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.82625e+08..2.84276e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "ingest_pipeline/iac_dense/no_raw",
            "value": 228090688.6,
            "range": "2.27706e+08..2.28471e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.27706e+08..2.28471e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "interop_delivery/emit_cross_isolate/S1",
            "value": 85171.40835298081,
            "range": "84909.5..85350.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 84909.5..85350.3 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S1",
            "value": 83356.69624350159,
            "range": "82914.6..83775.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 82914.6..83775.8 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S64",
            "value": 3790790.536090226,
            "range": "3.78433e+06..3.79771e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.78433e+06..3.79771e+06 ns/iter\nThroughput input: {\"Elements\": 2048}"
          },
          {
            "name": "interop_delivery/emit_fanout/S8",
            "value": 497135.820974155,
            "range": "496759..497493",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 496759..497493 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P16k",
            "value": 1610292.461414791,
            "range": "1.60968e+06..1.61098e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.60968e+06..1.61098e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P64",
            "value": 420880.6805227656,
            "range": "420695..421060",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 420695..421060 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W64",
            "value": 1815616.1992727271,
            "range": "1.81395e+06..1.81722e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.81395e+06..1.81722e+06 ns/iter\nThroughput input: {\"Elements\": 1024}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W8",
            "value": 283520.4594763802,
            "range": "283398..283642",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 283398..283642 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_delivery/watch_per_write/W8",
            "value": 434857.1931304348,
            "range": "434311..435619",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 434311..435619 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_ops/package/emit128",
            "value": 28025.576250845166,
            "range": "27933.3..28130.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 27933.3..28130.9 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/get128",
            "value": 70544.19507091702,
            "range": "70249.9..70918.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 70249.9..70918.3 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set128",
            "value": 98460.14832838773,
            "range": "98108..98773.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 98108..98773.1 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set_per_turn64",
            "value": 463655.89897959185,
            "range": "463273..464012",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 463273..464012 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_ops/user/emit128",
            "value": 27798.850983477576,
            "range": "27749..27865.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 27749..27865.5 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/get128",
            "value": 66102.37562130178,
            "range": "65897.9..66320.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 65897.9..66320.1 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set128",
            "value": 73606.05438442394,
            "range": "73305.5..73909.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73305.5..73909.7 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set_per_turn64",
            "value": 420008.0052808047,
            "range": "419668..420310",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 419668..420310 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_read/keys_32k",
            "value": 71854.36973012399,
            "range": "71270..72518",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 71270..72518 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_read/materialize_32k",
            "value": 12259588.14878049,
            "range": "1.20828e+07..1.24375e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.20828e+07..1.24375e+07 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "interop_read/value_leaf/1k",
            "value": 75501.13024932412,
            "range": "75229.3..75766.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 75229.3..75766.5 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/1m",
            "value": 75922.61082198072,
            "range": "75649.3..76206.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 75649.3..76206.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth1",
            "value": 75971.96568220273,
            "range": "75739.2..76198.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 75739.2..76198.7 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth4",
            "value": 76668.99930260764,
            "range": "76214.3..77103.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 76214.3..77103.9 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "line_operations/replace_and_highlight",
            "value": 9167.338281243434,
            "range": "9165.99..9168.93",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 9165.99..9168.93 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "path_parse/bracket",
            "value": 75.1436849186856,
            "range": "75.1288..75.1573",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 75.1288..75.1573 ns/iter"
          },
          {
            "name": "path_parse/depth1",
            "value": 49.33470174991458,
            "range": "49.3245..49.3451",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 49.3245..49.3451 ns/iter"
          },
          {
            "name": "path_parse/depth4",
            "value": 86.43217320413461,
            "range": "86.4191..86.4437",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 86.4191..86.4437 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/10k",
            "value": 516753.7009297521,
            "range": "516646..516831",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 516646..516831 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/50k",
            "value": 443561.3857648099,
            "range": "443491..443632",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 443491..443632 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/10k",
            "value": 3053327.0957317073,
            "range": "3.05216e+06..3.05461e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.05216e+06..3.05461e+06 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/50k",
            "value": 25372472.57,
            "range": "2.51887e+07..2.55995e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.51887e+07..2.55995e+07 ns/iter"
          },
          {
            "name": "pathfinding/path_across/10k",
            "value": 2835443.6728813555,
            "range": "2.83279e+06..2.83904e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.83279e+06..2.83904e+06 ns/iter"
          },
          {
            "name": "pathfinding/path_across/50k",
            "value": 21102794.770833336,
            "range": "2.09278e+07..2.13252e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.09278e+07..2.13252e+07 ns/iter"
          },
          {
            "name": "per_emit_composite/package",
            "value": 405.3254459167491,
            "range": "405.259..405.392",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 405.259..405.392 ns/iter"
          },
          {
            "name": "per_set_composite/package",
            "value": 342.52011932673395,
            "range": "342.396..342.641",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 342.396..342.641 ns/iter"
          },
          {
            "name": "per_set_composite/user",
            "value": 134.62426100863425,
            "range": "134.579..134.672",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 134.579..134.672 ns/iter"
          },
          {
            "name": "producer_parse/package",
            "value": 46.122390819154525,
            "range": "46.1151..46.13",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 46.1151..46.13 ns/iter"
          },
          {
            "name": "producer_parse/user",
            "value": 3.8266803048350018,
            "range": "3.82604..3.82737",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.82604..3.82737 ns/iter"
          },
          {
            "name": "rebuild/room_connections/10k",
            "value": 28561552.415,
            "range": "2.82101e+07..2.8926e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.82101e+07..2.8926e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "rebuild/room_connections/1k",
            "value": 1766211.366438356,
            "range": "1.75753e+06..1.77488e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.75753e+06..1.77488e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "rebuild/room_connections/50k",
            "value": 202142084.96666667,
            "range": "1.97745e+08..2.0624e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.97745e+08..2.0624e+08 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "scan_literals/aho_corasick_leftmost",
            "value": 15207103.134632034,
            "range": "1.52008e+07..1.52174e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.52008e+07..1.52174e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/aho_corasick_overlapping",
            "value": 17164400.214718614,
            "range": "1.71374e+07..1.71897e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.71374e+07..1.71897e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_filtered",
            "value": 404663498.45,
            "range": "4.03833e+08..4.056e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.03833e+08..4.056e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_set_current",
            "value": 32918122045,
            "range": "3.28961e+10..3.29419e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.28961e+10..3.29419e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_literals/tiered",
            "value": 50747239.385714285,
            "range": "5.07307e+07..5.07765e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.07307e+07..5.07765e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_filtered",
            "value": 494309072.65,
            "range": "4.93448e+08..4.95185e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.93448e+08..4.95185e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_set_current",
            "value": 32844760846.8,
            "range": "3.27945e+10..3.28954e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.27945e+10..3.28954e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_mixed/tiered",
            "value": 167745833.16103896,
            "range": "1.67612e+08..1.67861e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.67612e+08..1.67861e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "script_dispatch/baseline",
            "value": 338796.8276653171,
            "range": "337588..340003",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 337588..340003 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire0",
            "value": 1195575.7066985648,
            "range": "1.19528e+06..1.19584e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.19528e+06..1.19584e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire20",
            "value": 2848429.9587570624,
            "range": "2.84271e+06..2.85443e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.84271e+06..2.85443e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire5",
            "value": 1808621.738628159,
            "range": "1.8059e+06..1.81189e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.8059e+06..1.81189e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "sgr/process/bold_color",
            "value": 31.99921726044525,
            "range": "31.992..32.0069",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 31.992..32.0069 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/color_256",
            "value": 51.547012964263324,
            "range": "51.5345..51.5586",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 51.5345..51.5586 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/reset",
            "value": 21.21186288537874,
            "range": "21.2073..21.2164",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.2073..21.2164 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/simple_color",
            "value": 21.440635696426124,
            "range": "21.4362..21.4453",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.4362..21.4453 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/truecolor",
            "value": 93.36651464921539,
            "range": "93.3519..93.3809",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 93.3519..93.3809 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "spatial_query/connections/viewport_full/10k",
            "value": 81467.80931180692,
            "range": "81448.9..81488.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 81448.9..81488.5 ns/iter\nThroughput input: {\"Elements\": 19802}"
          },
          {
            "name": "spatial_query/connections/viewport_full/50k",
            "value": 437124.89659685857,
            "range": "437025..437222",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 437025..437222 ns/iter\nThroughput input: {\"Elements\": 99557}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/10k",
            "value": 19152.474052655605,
            "range": "19144.4..19160",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 19144.4..19160 ns/iter\nThroughput input: {\"Elements\": 4416}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/50k",
            "value": 91348.53296663,
            "range": "91284.9..91418.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 91284.9..91418.9 ns/iter\nThroughput input: {\"Elements\": 21021}"
          },
          {
            "name": "spatial_query/connections/viewport_small/10k",
            "value": 2951.50868308695,
            "range": "2942.43..2960.35",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2942.43..2960.35 ns/iter\nThroughput input: {\"Elements\": 576}"
          },
          {
            "name": "spatial_query/connections/viewport_small/50k",
            "value": 10928.694649180328,
            "range": "10910.8..10948",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 10910.8..10948 ns/iter\nThroughput input: {\"Elements\": 2359}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/10k",
            "value": 38435.52688868332,
            "range": "38428..38443.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 38428..38443.3 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/50k",
            "value": 197170.136496063,
            "range": "197116..197235",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 197116..197235 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/10k",
            "value": 9099.215014697022,
            "range": "9086.21..9118.12",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 9086.21..9118.12 ns/iter\nThroughput input: {\"Elements\": 2070}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/50k",
            "value": 38849.695698674725,
            "range": "38794.4..38905.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 38794.4..38905.6 ns/iter\nThroughput input: {\"Elements\": 10306}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/10k",
            "value": 1112.7248163585932,
            "range": "1107.18..1117.67",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1107.18..1117.67 ns/iter\nThroughput input: {\"Elements\": 240}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/50k",
            "value": 4896.465083971411,
            "range": "4891.76..4901.22",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4891.76..4901.22 ns/iter\nThroughput input: {\"Elements\": 1146}"
          },
          {
            "name": "styled_line/new_no_raw/long_plain",
            "value": 21.085243427839647,
            "range": "21.0758..21.0954",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.0758..21.0954 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/long_styled",
            "value": 36.3670926751586,
            "range": "36.3486..36.3853",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 36.3486..36.3853 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/short_plain",
            "value": 20.333217970104904,
            "range": "20.329..20.3372",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.329..20.3372 ns/iter\nThroughput input: {\"Bytes\": 40}"
          },
          {
            "name": "styled_line/new_with_raw/long_plain",
            "value": 109.8233550334664,
            "range": "109.564..110.021",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 109.564..110.021 ns/iter\nThroughput input: {\"Bytes\": 400}"
          },
          {
            "name": "styled_line/new_with_raw/long_styled",
            "value": 132.84791274695993,
            "range": "132.828..132.869",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 132.828..132.869 ns/iter\nThroughput input: {\"Bytes\": 464}"
          },
          {
            "name": "styled_line/new_with_raw/short_plain",
            "value": 37.74804824850362,
            "range": "37.743..37.7532",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 37.743..37.7532 ns/iter\nThroughput input: {\"Bytes\": 80}"
          },
          {
            "name": "telnet_receive/ansi_light",
            "value": 290513.24387695873,
            "range": "290224..290794",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 290224..290794 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "telnet_receive/iac_dense",
            "value": 4461981.880530973,
            "range": "4.46013e+06..4.46405e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.46013e+06..4.46405e+06 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "to_spans/by_span_count/1",
            "value": 62.72995554629144,
            "range": "62.7185..62.743",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 62.7185..62.743 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "to_spans/by_span_count/32",
            "value": 1313.8037457319965,
            "range": "1313.58..1314.02",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1313.58..1314.02 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "to_spans/by_span_count/8",
            "value": 361.2398617549304,
            "range": "361.154..361.32",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 361.154..361.32 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "trigger_verbs/empty",
            "value": 1057258.1050955413,
            "range": "1.05553e+06..1.05909e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.05553e+06..1.05909e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/gag",
            "value": 1078160.5095652174,
            "range": "1.07791e+06..1.0784e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.07791e+06..1.0784e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/highlight",
            "value": 1193843.276076555,
            "range": "1.19304e+06..1.19473e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.19304e+06..1.19473e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/read_echo",
            "value": 1405514.6483146066,
            "range": "1.40435e+06..1.40733e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.40435e+06..1.40733e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "upsert_room/single/10k",
            "value": 985550.031477927,
            "range": "984753..986373",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 984753..986373 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/1k",
            "value": 889406.5499136442,
            "range": "884255..894536",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 884255..894536 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/50k",
            "value": 1312323.2054404146,
            "range": "1.31063e+06..1.31408e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.31063e+06..1.31408e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/1",
            "value": 1005060.6164658635,
            "range": "1.00269e+06..1.00751e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.00269e+06..1.00751e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/16",
            "value": 1210266.9227272726,
            "range": "1.20364e+06..1.21697e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.20364e+06..1.21697e+06 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "upsert_rooms/batch_10k/256",
            "value": 3391325.2828947357,
            "range": "3.36738e+06..3.41157e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.36738e+06..3.41157e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "write_and_flush/J16/W8_mixed",
            "value": 17357.289486544032,
            "range": "17347.6..17367.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: ce813f4506945837c4e4c66531c556912556bc71\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 17347.6..17367.6 ns/iter\nThroughput input: {\"Elements\": 16}"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "ping@walter.dev",
            "name": "Walter Kalata",
            "username": "wbk"
          },
          "committer": {
            "email": "ping@walter.dev",
            "name": "Walter Kalata",
            "username": "wbk"
          },
          "distinct": true,
          "id": "849793d5bc1e70fe809c9b38d7d884eddcadbd1b",
          "message": "docs: add contributing guide",
          "timestamp": "2026-07-23T19:14:40-07:00",
          "tree_id": "e3019665d971a8a7918e22512662adfc29b4234a",
          "url": "https://github.com/smudgy-mud/smudgy/commit/849793d5bc1e70fe809c9b38d7d884eddcadbd1b"
        },
        "date": 1784864583150,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "atlas_build/cold/10k",
            "value": 58572797.755555555,
            "range": "5.8538e+07..5.86087e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.8538e+07..5.86087e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "atlas_build/cold/1k",
            "value": 1308614.1785900784,
            "range": "1.30769e+06..1.30961e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.30769e+06..1.30961e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "atlas_build/cold/50k",
            "value": 1350694128.3,
            "range": "1.34964e+09..1.35173e+09",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.34964e+09..1.35173e+09 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "automap_step/create_room/100k",
            "value": 1980059.0214028775,
            "range": "1.74922e+06..2.21462e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.74922e+06..2.21462e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "automap_step/create_room/10k",
            "value": 2469687.8739543725,
            "range": "2.28138e+06..2.65373e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.28138e+06..2.65373e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "build/aho_corasick",
            "value": 6475196.129350649,
            "range": "6.46827e+06..6.47844e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6.46827e+06..6.47844e+06 ns/iter"
          },
          {
            "name": "build/regex_filtered",
            "value": 121361255.61818182,
            "range": "1.21249e+08..1.21599e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.21249e+08..1.21599e+08 ns/iter"
          },
          {
            "name": "build/regex_set",
            "value": 53667165.66883117,
            "range": "5.35437e+07..5.37596e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.35437e+07..5.37596e+07 ns/iter"
          },
          {
            "name": "build/tiered",
            "value": 32820631.85974026,
            "range": "3.27995e+07..3.28469e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.27995e+07..3.28469e+07 ns/iter"
          },
          {
            "name": "catalogue/sample/dynamic/small",
            "value": 93.59607436518168,
            "range": "93.5415..93.6626",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 93.5415..93.6626 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/large",
            "value": 6563.055882804847,
            "range": "6561.15..6564.97",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6561.15..6564.97 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/small",
            "value": 303.54878528151323,
            "range": "303.386..303.712",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 303.386..303.712 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/large",
            "value": 91.03882192095865,
            "range": "91.0175..91.0596",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 91.0175..91.0596 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/small",
            "value": 86.06669690033894,
            "range": "86.0417..86.0943",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 86.0417..86.0943 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_128",
            "value": 72637.22333129261,
            "range": "72607..72669.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 72607..72669.1 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_512",
            "value": 307152.5708866558,
            "range": "307061..307244",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 307061..307244 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_8",
            "value": 4443.734289476709,
            "range": "4441.5..4445.77",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4441.5..4445.77 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_4096",
            "value": 4602.882668806682,
            "range": "4601.98..4603.77",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4601.98..4603.77 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_64",
            "value": 4125.967915626026,
            "range": "4123.9..4127.91",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4123.9..4127.91 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_65536",
            "value": 4551.397197280922,
            "range": "4550.25..4552.59",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4550.25..4552.59 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "churn_packet/clean",
            "value": 68298.40928414701,
            "range": "67840.8..68744.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 67840.8..68744.4 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20",
            "value": 78764456.92857142,
            "range": "7.8681e+07..7.88464e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7.8681e+07..7.88464e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20_x4pkg",
            "value": 81271663.4,
            "range": "8.11611e+07..8.13814e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8.11611e+07..8.13814e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/toggle20",
            "value": 81253.08861394145,
            "range": "81140.6..81388.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 81140.6..81388.4 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_residue/full/10000",
            "value": 330814237.45,
            "range": "3.30465e+08..3.31164e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.30465e+08..3.31164e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/1000",
            "value": 337924051.6,
            "range": "3.37194e+08..3.38646e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.37194e+08..3.38646e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/5000",
            "value": 296717430.25,
            "range": "2.96275e+08..2.97162e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.96275e+08..2.97162e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/1000",
            "value": 338071414.6,
            "range": "3.37459e+08..3.38768e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.37459e+08..3.38768e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/5000",
            "value": 299789008.35,
            "range": "2.99377e+08..3.00225e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.99377e+08..3.00225e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_absent/25",
            "value": 235680955.8,
            "range": "2.31262e+08..2.40078e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.31262e+08..2.40078e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_disabled/25",
            "value": 280564283.65,
            "range": "2.79902e+08..2.81238e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.79902e+08..2.81238e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "creator_parse/package",
            "value": 254.93173304608598,
            "range": "254.75..255.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 254.75..255.1 ns/iter"
          },
          {
            "name": "creator_parse/user",
            "value": 48.17248703450097,
            "range": "48.1499..48.1935",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 48.1499..48.1935 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/1000",
            "value": 14284802.525714284,
            "range": "1.42698e+07..1.43011e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.42698e+07..1.43011e+07 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/10000",
            "value": 53926420.42,
            "range": "5.38875e+07..5.39617e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.38875e+07..5.39617e+07 ns/iter"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/bytes",
            "value": 347982932.65,
            "range": "3.4749e+08..3.48458e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.4749e+08..3.48458e+08 ns/iter\nThroughput input: {\"BytesDecimal\": 16269045}"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/lines",
            "value": 332392502.2,
            "range": "3.32004e+08..3.32769e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.32004e+08..3.32769e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "extend_line/at_capacity",
            "value": 120388.19078440809,
            "range": "120344..120441",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 120344..120441 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag16",
            "value": 17427091.98214286,
            "range": "1.74198e+07..1.74346e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.74198e+07..1.74346e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag4",
            "value": 3385915.2128,
            "range": "3.38416e+06..3.38741e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.38416e+06..3.38741e+06 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/whole_lines",
            "value": 66249.6078518849,
            "range": "66227.3..66273.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66227.3..66273.6 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "flush_coalesced/J1/W0",
            "value": 146.93873861479022,
            "range": "146.888..146.991",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 146.888..146.991 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W64",
            "value": 6274.418446514151,
            "range": "6270.18..6278.02",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6270.18..6278.02 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W8",
            "value": 770.244067787467,
            "range": "770.029..770.443",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 770.029..770.443 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J128/W0",
            "value": 6270.950214624038,
            "range": "6264.89..6276.99",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6264.89..6276.99 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W64",
            "value": 153236.10561883065,
            "range": "153199..153286",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 153199..153286 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W8",
            "value": 22616.084029784393,
            "range": "22608.7..22622.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 22608.7..22622.6 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J16/W0",
            "value": 661.2002600450619,
            "range": "661.047..661.394",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 661.047..661.394 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W64",
            "value": 23512.988470978034,
            "range": "23448.3..23584.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 23448.3..23584.5 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W8",
            "value": 3566.9953115983844,
            "range": "3551.32..3582.79",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3551.32..3582.79 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J1/W64",
            "value": 6722.632460446288,
            "range": "6718.93..6726.38",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6718.93..6726.38 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J1/W8",
            "value": 865.3305607253081,
            "range": "864.519..866.174",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 864.519..866.174 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J128/W64",
            "value": 517640.3447278912,
            "range": "517490..517800",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 517490..517800 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J128/W8",
            "value": 66983.53992923243,
            "range": "66974.4..66991.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66974.4..66991.2 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J16/W64",
            "value": 65851.53692675507,
            "range": "65831.7..65870.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 65831.7..65870.8 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J16/W8",
            "value": 8424.639640370446,
            "range": "8418.15..8431.09",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8418.15..8431.09 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J1/W0",
            "value": 146.21356032473244,
            "range": "146.159..146.284",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 146.159..146.284 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W64",
            "value": 6523.616335168088,
            "range": "6518.69..6528.09",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6518.69..6528.09 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W8",
            "value": 823.421709008225,
            "range": "823.139..823.676",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 823.139..823.676 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J128/W0",
            "value": 4538.567766060878,
            "range": "4536.4..4540.43",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4536.4..4540.43 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W64",
            "value": 856245.5887240358,
            "range": "856042..856443",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 856042..856443 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W8",
            "value": 109413.34928664073,
            "range": "109381..109447",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 109381..109447 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J16/W0",
            "value": 765.9635297063678,
            "range": "765.531..766.346",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 765.531..766.346 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W64",
            "value": 104542.00622270744,
            "range": "104509..104575",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 104509..104575 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W8",
            "value": 13658.110412124433,
            "range": "13654.4..13661.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 13654.4..13661.9 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "fold/lower",
            "value": 21.0728376845598,
            "range": "21.0686..21.0773",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.0686..21.0773 ns/iter"
          },
          {
            "name": "fold/mixed",
            "value": 21.077683961911987,
            "range": "21.0743..21.0809",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.0743..21.0809 ns/iter"
          },
          {
            "name": "follow/find_room_by_external_id/100k",
            "value": 93.55279964772998,
            "range": "93.522..93.5851",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 93.522..93.5851 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "follow/find_room_by_external_id/10k",
            "value": 95.37726791428577,
            "range": "95.3108..95.4472",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 95.3108..95.4472 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "frame_proxy/10k",
            "value": 168283.46226921788,
            "range": "168087..168526",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 168087..168526 ns/iter\nThroughput input: {\"Elements\": 32430}"
          },
          {
            "name": "identification/by_title_and_description/10k",
            "value": 15449.12982515965,
            "range": "15440..15458.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 15440..15458.5 ns/iter\nThroughput input: {\"Elements\": 44}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy",
            "value": 534792601.9,
            "range": "5.33457e+08..5.36221e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.33457e+08..5.36221e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy/no_raw",
            "value": 472100830.7,
            "range": "4.65501e+08..4.77341e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.65501e+08..4.77341e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_light",
            "value": 278374983.7,
            "range": "2.77834e+08..2.78868e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.77834e+08..2.78868e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/ansi_light/no_raw",
            "value": 231540164.46666664,
            "range": "2.31145e+08..2.31983e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.31145e+08..2.31983e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/iac_dense",
            "value": 285933253.4,
            "range": "2.85405e+08..2.86481e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.85405e+08..2.86481e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "ingest_pipeline/iac_dense/no_raw",
            "value": 232490340.3,
            "range": "2.31964e+08..2.33096e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.31964e+08..2.33096e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "interop_delivery/emit_cross_isolate/S1",
            "value": 84801.92900632802,
            "range": "84722.7..84879.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 84722.7..84879.6 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S1",
            "value": 85034.9383788396,
            "range": "84737.6..85307.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 84737.6..85307.3 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S64",
            "value": 3881320.1015503877,
            "range": "3.87694e+06..3.88602e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.87694e+06..3.88602e+06 ns/iter\nThroughput input: {\"Elements\": 2048}"
          },
          {
            "name": "interop_delivery/emit_fanout/S8",
            "value": 506780.98133874236,
            "range": "506077..507468",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 506077..507468 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P16k",
            "value": 1630788.2025974027,
            "range": "1.62968e+06..1.63177e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.62968e+06..1.63177e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P64",
            "value": 428038.13929794525,
            "range": "427894..428181",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 427894..428181 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W64",
            "value": 1832441.60989011,
            "range": "1.83102e+06..1.83382e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.83102e+06..1.83382e+06 ns/iter\nThroughput input: {\"Elements\": 1024}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W8",
            "value": 284810.39982905984,
            "range": "284542..285069",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 284542..285069 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_delivery/watch_per_write/W8",
            "value": 443400.6244464127,
            "range": "442986..443824",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 442986..443824 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_ops/package/emit128",
            "value": 28146.573181250707,
            "range": "28069.1..28231",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 28069.1..28231 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/get128",
            "value": 68846.94541969054,
            "range": "68538.9..69191.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 68538.9..69191.2 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set128",
            "value": 99379.8664020639,
            "range": "99036..99686.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 99036..99686.5 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set_per_turn64",
            "value": 467832.0467726847,
            "range": "467567..468102",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 467567..468102 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_ops/user/emit128",
            "value": 27848.75086753888,
            "range": "27756.5..27963.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 27756.5..27963.7 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/get128",
            "value": 66057.1868473232,
            "range": "65973.4..66149.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 65973.4..66149.2 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set128",
            "value": 73882.36893246831,
            "range": "73746.8..73968.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73746.8..73968.3 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set_per_turn64",
            "value": 420625.0155592935,
            "range": "420296..420923",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 420296..420923 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_read/keys_32k",
            "value": 78111.96089116597,
            "range": "77798.7..78433.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 77798.7..78433.5 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_read/materialize_32k",
            "value": 12377832.209756099,
            "range": "1.20836e+07..1.25882e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.20836e+07..1.25882e+07 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "interop_read/value_leaf/1k",
            "value": 72541.15758455508,
            "range": "72347.3..72747.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 72347.3..72747.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/1m",
            "value": 73354.32370456867,
            "range": "73009.7..73715.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73009.7..73715.6 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth1",
            "value": 73436.08963862648,
            "range": "73015.3..73874.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73015.3..73874.1 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth4",
            "value": 74312.1401102996,
            "range": "74036.6..74576.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 74036.6..74576.7 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "line_operations/replace_and_highlight",
            "value": 9107.644531434931,
            "range": "9105.58..9109.66",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 9105.58..9109.66 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "path_parse/bracket",
            "value": 78.36452853131794,
            "range": "78.3415..78.3881",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 78.3415..78.3881 ns/iter"
          },
          {
            "name": "path_parse/depth1",
            "value": 49.28775686682249,
            "range": "49.2742..49.3019",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 49.2742..49.3019 ns/iter"
          },
          {
            "name": "path_parse/depth4",
            "value": 87.05814038057231,
            "range": "87.043..87.0736",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 87.043..87.0736 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/10k",
            "value": 504536.65558912384,
            "range": "504448..504624",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 504448..504624 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/50k",
            "value": 436625.2462946818,
            "range": "436516..436730",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 436516..436730 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/10k",
            "value": 2988702.7535714284,
            "range": "2.98809e+06..2.98941e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.98809e+06..2.98941e+06 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/50k",
            "value": 28583695.54444444,
            "range": "2.85464e+07..2.8622e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.85464e+07..2.8622e+07 ns/iter"
          },
          {
            "name": "pathfinding/path_across/10k",
            "value": 2910612.447398844,
            "range": "2.9102e+06..2.91102e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.9102e+06..2.91102e+06 ns/iter"
          },
          {
            "name": "pathfinding/path_across/50k",
            "value": 23451901.61904762,
            "range": "2.333e+07..2.35735e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.333e+07..2.35735e+07 ns/iter"
          },
          {
            "name": "per_emit_composite/package",
            "value": 412.3442899584769,
            "range": "412.237..412.45",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 412.237..412.45 ns/iter"
          },
          {
            "name": "per_set_composite/package",
            "value": 358.1456089987761,
            "range": "358.049..358.242",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 358.049..358.242 ns/iter"
          },
          {
            "name": "per_set_composite/user",
            "value": 133.45557382999488,
            "range": "133.415..133.496",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 133.415..133.496 ns/iter"
          },
          {
            "name": "producer_parse/package",
            "value": 46.486926703670505,
            "range": "46.4776..46.496",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 46.4776..46.496 ns/iter"
          },
          {
            "name": "producer_parse/user",
            "value": 3.8581299250606143,
            "range": "3.85741..3.85887",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.85741..3.85887 ns/iter"
          },
          {
            "name": "rebuild/room_connections/10k",
            "value": 28605958.859999996,
            "range": "2.81259e+07..2.9097e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.81259e+07..2.9097e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "rebuild/room_connections/1k",
            "value": 1782239.3799307956,
            "range": "1.77255e+06..1.79016e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.77255e+06..1.79016e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "rebuild/room_connections/50k",
            "value": 201558561.13333336,
            "range": "1.98275e+08..2.04955e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.98275e+08..2.04955e+08 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "scan_literals/aho_corasick_leftmost",
            "value": 15307609.826839827,
            "range": "1.5252e+07..1.54248e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.5252e+07..1.54248e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/aho_corasick_overlapping",
            "value": 18169074.824242424,
            "range": "1.81621e+07..1.81747e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.81621e+07..1.81747e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_filtered",
            "value": 407383494.75,
            "range": "4.07255e+08..4.07517e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.07255e+08..4.07517e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_set_current",
            "value": 30910794926.8,
            "range": "3.09046e+10..3.09168e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.09046e+10..3.09168e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_literals/tiered",
            "value": 51837021.505194806,
            "range": "5.17343e+07..5.19403e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.17343e+07..5.19403e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_filtered",
            "value": 498187175.7,
            "range": "4.97913e+08..4.98509e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.97913e+08..4.98509e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_set_current",
            "value": 33030990553.1,
            "range": "3.30072e+10..3.30503e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.30072e+10..3.30503e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_mixed/tiered",
            "value": 170949877.2857143,
            "range": "1.70734e+08..1.71065e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.70734e+08..1.71065e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "script_dispatch/baseline",
            "value": 341622.91414965986,
            "range": "341006..342205",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 341006..342205 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire0",
            "value": 1204880.5392344499,
            "range": "1.20395e+06..1.20578e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.20395e+06..1.20578e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire20",
            "value": 2863589.8977011493,
            "range": "2.86288e+06..2.86435e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.86288e+06..2.86435e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire5",
            "value": 1823102.4827838826,
            "range": "1.82171e+06..1.82451e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.82171e+06..1.82451e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "sgr/process/bold_color",
            "value": 32.3690256948308,
            "range": "32.3567..32.3812",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 32.3567..32.3812 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/color_256",
            "value": 52.05769627902822,
            "range": "52.0374..52.0786",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 52.0374..52.0786 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/reset",
            "value": 21.424961516764597,
            "range": "21.4201..21.43",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.4201..21.43 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/simple_color",
            "value": 21.711190259850497,
            "range": "21.7044..21.7181",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.7044..21.7181 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/truecolor",
            "value": 84.21363802186578,
            "range": "84.1925..84.235",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 84.1925..84.235 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "spatial_query/connections/viewport_full/10k",
            "value": 81909.43373730757,
            "range": "81748.8..82096.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 81748.8..82096.2 ns/iter\nThroughput input: {\"Elements\": 19802}"
          },
          {
            "name": "spatial_query/connections/viewport_full/50k",
            "value": 449938.94025157235,
            "range": "449408..450484",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 449408..450484 ns/iter\nThroughput input: {\"Elements\": 99557}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/10k",
            "value": 19083.470096303907,
            "range": "19075.1..19091.7",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 19075.1..19091.7 ns/iter\nThroughput input: {\"Elements\": 4416}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/50k",
            "value": 93174.43019395748,
            "range": "93057.1..93302.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 93057.1..93302.4 ns/iter\nThroughput input: {\"Elements\": 21021}"
          },
          {
            "name": "spatial_query/connections/viewport_small/10k",
            "value": 2993.971825111031,
            "range": "2986.33..3000.95",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2986.33..3000.95 ns/iter\nThroughput input: {\"Elements\": 576}"
          },
          {
            "name": "spatial_query/connections/viewport_small/50k",
            "value": 10956.981640899507,
            "range": "10950.3..10963.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 10950.3..10963.8 ns/iter\nThroughput input: {\"Elements\": 2359}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/10k",
            "value": 38783.07072124757,
            "range": "38775.6..38790.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 38775.6..38790.2 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/50k",
            "value": 197969.95053465344,
            "range": "197758..198254",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 197758..198254 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/10k",
            "value": 9098.627174899286,
            "range": "9093.23..9105.12",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 9093.23..9105.12 ns/iter\nThroughput input: {\"Elements\": 2070}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/50k",
            "value": 40211.67639756464,
            "range": "40186.7..40237.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 40186.7..40237.4 ns/iter\nThroughput input: {\"Elements\": 10306}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/10k",
            "value": 1119.681894308537,
            "range": "1112.44..1126.81",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1112.44..1126.81 ns/iter\nThroughput input: {\"Elements\": 240}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/50k",
            "value": 4913.815189997261,
            "range": "4908.59..4918.94",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4908.59..4918.94 ns/iter\nThroughput input: {\"Elements\": 1146}"
          },
          {
            "name": "styled_line/new_no_raw/long_plain",
            "value": 21.396562458406166,
            "range": "21.3905..21.4022",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.3905..21.4022 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/long_styled",
            "value": 23.91559466245346,
            "range": "23.9075..23.9234",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 23.9075..23.9234 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/short_plain",
            "value": 20.63202749234502,
            "range": "20.6268..20.637",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.6268..20.637 ns/iter\nThroughput input: {\"Bytes\": 40}"
          },
          {
            "name": "styled_line/new_with_raw/long_plain",
            "value": 101.25484282924081,
            "range": "101.202..101.311",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 101.202..101.311 ns/iter\nThroughput input: {\"Bytes\": 400}"
          },
          {
            "name": "styled_line/new_with_raw/long_styled",
            "value": 123.93983340411899,
            "range": "123.845..124.021",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 123.845..124.021 ns/iter\nThroughput input: {\"Bytes\": 464}"
          },
          {
            "name": "styled_line/new_with_raw/short_plain",
            "value": 37.938176692291755,
            "range": "37.9068..37.9835",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 37.9068..37.9835 ns/iter\nThroughput input: {\"Bytes\": 80}"
          },
          {
            "name": "telnet_receive/ansi_light",
            "value": 288228.5250865052,
            "range": "288103..288347",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 288103..288347 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "telnet_receive/iac_dense",
            "value": 4453910.869026549,
            "range": "4.45221e+06..4.45538e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.45221e+06..4.45538e+06 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "to_spans/by_span_count/1",
            "value": 63.761695153199405,
            "range": "63.7491..63.7743",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 63.7491..63.7743 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "to_spans/by_span_count/32",
            "value": 1339.6977745762922,
            "range": "1339.34..1340.05",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1339.34..1340.05 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "to_spans/by_span_count/8",
            "value": 355.6025917669718,
            "range": "355.525..355.677",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 355.525..355.677 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "trigger_verbs/empty",
            "value": 1053619.5464135022,
            "range": "1.05326e+06..1.054e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.05326e+06..1.054e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/gag",
            "value": 1083204.1681917212,
            "range": "1.08272e+06..1.08371e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.08272e+06..1.08371e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/highlight",
            "value": 1206088.8309178748,
            "range": "1.2059e+06..1.20626e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.2059e+06..1.20626e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/read_echo",
            "value": 1401917.6352941175,
            "range": "1.40064e+06..1.40317e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.40064e+06..1.40317e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "upsert_room/single/10k",
            "value": 981093.9814176245,
            "range": "979297..982200",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 979297..982200 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/1k",
            "value": 891574.8005190311,
            "range": "885728..897671",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 885728..897671 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/50k",
            "value": 1281051.6551637277,
            "range": "1.27816e+06..1.28348e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.27816e+06..1.28348e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/1",
            "value": 997146.3669338677,
            "range": "995992..998194",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 995992..998194 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/16",
            "value": 1189943.0688836104,
            "range": "1.18704e+06..1.19256e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.18704e+06..1.19256e+06 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "upsert_rooms/batch_10k/256",
            "value": 3409561.1289473684,
            "range": "3.38811e+06..3.42869e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.38811e+06..3.42869e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "write_and_flush/J16/W8_mixed",
            "value": 17091.591292336027,
            "range": "17088.3..17094.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: 849793d5bc1e70fe809c9b38d7d884eddcadbd1b\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 17088.3..17094.8 ns/iter\nThroughput input: {\"Elements\": 16}"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "ping@walter.dev",
            "name": "wbk",
            "username": "wbk"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "cb13a2ddf32ae1cdee6b22517461ba917f60dcd3",
          "message": "Merge pull request #15 from smudgy-mud/feat/benchmark-signal\n\nci: make PR benchmark signals reproducible",
          "timestamp": "2026-07-23T20:18:03-07:00",
          "tree_id": "d116818fcfad4e7ab65592247496616c26760b81",
          "url": "https://github.com/smudgy-mud/smudgy/commit/cb13a2ddf32ae1cdee6b22517461ba917f60dcd3"
        },
        "date": 1784867365706,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "atlas_build/cold/10k",
            "value": 59361040.67777777,
            "range": "5.92653e+07..5.94616e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.92653e+07..5.94616e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "atlas_build/cold/1k",
            "value": 1330696.032722513,
            "range": "1.32909e+06..1.33216e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.32909e+06..1.33216e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "atlas_build/cold/50k",
            "value": 1355983716.6,
            "range": "1.3553e+09..1.35677e+09",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.3553e+09..1.35677e+09 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "automap_step/create_room/100k",
            "value": 2066965.0375796177,
            "range": "1.87404e+06..2.26859e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.87404e+06..2.26859e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "automap_step/create_room/10k",
            "value": 2476371.7209829865,
            "range": "2.28513e+06..2.66061e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.28513e+06..2.66061e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "build/aho_corasick",
            "value": 6445030.089004329,
            "range": "6.44271e+06..6.44672e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6.44271e+06..6.44672e+06 ns/iter"
          },
          {
            "name": "build/regex_filtered",
            "value": 120785250.63896103,
            "range": "1.20604e+08..1.20997e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.20604e+08..1.20997e+08 ns/iter"
          },
          {
            "name": "build/regex_set",
            "value": 54162428.67272727,
            "range": "5.40932e+07..5.42237e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.40932e+07..5.42237e+07 ns/iter"
          },
          {
            "name": "build/tiered",
            "value": 32753424.74199134,
            "range": "3.27215e+07..3.28009e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.27215e+07..3.28009e+07 ns/iter"
          },
          {
            "name": "catalogue/sample/dynamic/small",
            "value": 93.84798488796763,
            "range": "93.8171..93.8813",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 93.8171..93.8813 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/large",
            "value": 6466.9864296173755,
            "range": "6465.76..6468.25",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 6465.76..6468.25 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/subscribed/small",
            "value": 293.8638739958902,
            "range": "293.751..293.982",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 293.751..293.982 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/large",
            "value": 88.17924999002786,
            "range": "88.1637..88.1944",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 88.1637..88.1944 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/sample/unsubscribed/small",
            "value": 85.25090093301274,
            "range": "85.2169..85.2865",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 85.2169..85.2865 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_128",
            "value": 70782.58195103689,
            "range": "70756.5..70810.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 70756.5..70810.6 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_512",
            "value": 304648.7613890941,
            "range": "304560..304736",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 304560..304736 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/entries_8",
            "value": 4425.071949913798,
            "range": "4422.11..4428.26",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4422.11..4428.26 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_4096",
            "value": 4477.518232786607,
            "range": "4474.61..4480.67",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4474.61..4480.67 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_64",
            "value": 4171.860094938268,
            "range": "4169.93..4173.88",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 4169.93..4173.88 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "catalogue/snapshot/leaves_65536",
            "value": 5326.363091220448,
            "range": "5325.37..5327.37",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5325.37..5327.37 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "churn_packet/clean",
            "value": 93168.60738207962,
            "range": "91030.8..94723.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 91030.8..94723.8 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20",
            "value": 78903020.77142857,
            "range": "7.87932e+07..7.90162e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 7.87932e+07..7.90162e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/create_delete20_x4pkg",
            "value": 81581436.97142856,
            "range": "8.14593e+07..8.1704e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8.14593e+07..8.1704e+07 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_packet/toggle20",
            "value": 107426.60249087393,
            "range": "107315..107542",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 107315..107542 ns/iter\nThroughput input: {\"Elements\": 50}"
          },
          {
            "name": "churn_residue/full/10000",
            "value": 336949106.65,
            "range": "3.3597e+08..3.37774e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.3597e+08..3.37774e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/1000",
            "value": 334565302.15,
            "range": "3.33943e+08..3.35185e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.33943e+08..3.35185e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_absent/5000",
            "value": 293641411.45,
            "range": "2.9295e+08..2.94342e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.9295e+08..2.94342e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/1000",
            "value": 335549407.65,
            "range": "3.34853e+08..3.36205e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.34853e+08..3.36205e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/literal_disabled/5000",
            "value": 297688200.65,
            "range": "2.97414e+08..2.97978e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.97414e+08..2.97978e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_absent/25",
            "value": 232247705.53333336,
            "range": "2.28216e+08..2.36306e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.28216e+08..2.36306e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "churn_residue/regex_disabled/25",
            "value": 281325277.15,
            "range": "2.80845e+08..2.81786e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.80845e+08..2.81786e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "creator_parse/package",
            "value": 261.68235705858086,
            "range": "261.339..261.999",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 261.339..261.999 ns/iter"
          },
          {
            "name": "creator_parse/user",
            "value": 47.48324799915808,
            "range": "47.4523..47.5125",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 47.4523..47.5125 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/1000",
            "value": 14288277.09722222,
            "range": "1.42681e+07..1.4308e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.42681e+07..1.4308e+07 ns/iter"
          },
          {
            "name": "engine_build/dirty_rebuild/10000",
            "value": 53712130.52,
            "range": "5.36592e+07..5.3784e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.36592e+07..5.3784e+07 ns/iter"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/bytes",
            "value": 349455307.3,
            "range": "3.48873e+08..3.50064e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.48873e+08..3.50064e+08 ns/iter\nThroughput input: {\"BytesDecimal\": 16269045}"
          },
          {
            "name": "engine_scan/synthetic-long-session.log/lines",
            "value": 334155372.6,
            "range": "3.33581e+08..3.34641e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.33581e+08..3.34641e+08 ns/iter\nThroughput input: {\"Elements\": 300000}"
          },
          {
            "name": "extend_line/at_capacity",
            "value": 120348.32398746384,
            "range": "120245..120453",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 120245..120453 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag16",
            "value": 17744806.964285713,
            "range": "1.77316e+07..1.77582e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.77316e+07..1.77582e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/frag4",
            "value": 3320510.1356589147,
            "range": "3.31975e+06..3.32134e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.31975e+06..3.32134e+06 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "extend_line/whole_lines",
            "value": 66144.16825044405,
            "range": "66006.4..66270.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66006.4..66270.6 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "flush_coalesced/J1/W0",
            "value": 148.45291051251735,
            "range": "148.329..148.586",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 148.329..148.586 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W64",
            "value": 6343.080692726012,
            "range": "6337.99..6348.11",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6337.99..6348.11 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J1/W8",
            "value": 769.2212967127015,
            "range": "768.312..770.093",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 768.312..770.093 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_coalesced/J128/W0",
            "value": 6366.745123384253,
            "range": "6362.17..6371.38",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6362.17..6371.38 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W64",
            "value": 153585.39840546699,
            "range": "153509..153665",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 153509..153665 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J128/W8",
            "value": 22590.04445059201,
            "range": "22564..22629.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 22564..22629.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_coalesced/J16/W0",
            "value": 763.3092879587678,
            "range": "762.633..764.039",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 762.633..764.039 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W64",
            "value": 22984.506419557398,
            "range": "22968.2..23002.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 22968.2..23002.1 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_coalesced/J16/W8",
            "value": 3581.1256613466567,
            "range": "3579.16..3583.49",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3579.16..3583.49 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J1/W64",
            "value": 6892.643852394142,
            "range": "6885.13..6897.99",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6885.13..6897.99 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J1/W8",
            "value": 844.666938603396,
            "range": "844.056..845.279",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 844.056..845.279 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_mixed/J128/W64",
            "value": 526983.2327022374,
            "range": "526659..527328",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 526659..527328 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J128/W8",
            "value": 67610.91152999451,
            "range": "67571.3..67647.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 67571.3..67647.2 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_mixed/J16/W64",
            "value": 66940.29538091067,
            "range": "66880.6..66999.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 66880.6..66999.5 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_mixed/J16/W8",
            "value": 8592.042003847062,
            "range": "8581.87..8603.57",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8581.87..8603.57 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J1/W0",
            "value": 146.9854308034441,
            "range": "146.844..147.128",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 146.844..147.128 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W64",
            "value": 6678.885004011409,
            "range": "6675.45..6682.36",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 6675.45..6682.36 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J1/W8",
            "value": 836.9756381203636,
            "range": "836.373..837.635",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 836.373..837.635 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "flush_per_write/J128/W0",
            "value": 4540.563030746705,
            "range": "4536.32..4544.69",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4536.32..4544.69 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W64",
            "value": 874605.6904191617,
            "range": "873902..875185",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 873902..875185 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J128/W8",
            "value": 110939.63544743702,
            "range": "110903..110979",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 110903..110979 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "flush_per_write/J16/W0",
            "value": 770.1937408770851,
            "range": "769.617..770.784",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 769.617..770.784 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W64",
            "value": 106598.55834558823,
            "range": "106542..106659",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 106542..106659 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "flush_per_write/J16/W8",
            "value": 13383.038575310964,
            "range": "13375.2..13391.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 13375.2..13391.4 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "fold/lower",
            "value": 20.988743458480126,
            "range": "20.9851..20.9925",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.9851..20.9925 ns/iter"
          },
          {
            "name": "fold/mixed",
            "value": 20.98484550832842,
            "range": "20.9822..20.9874",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 20.9822..20.9874 ns/iter"
          },
          {
            "name": "follow/find_room_by_external_id/100k",
            "value": 93.88786324290012,
            "range": "93.8594..93.9186",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 93.8594..93.9186 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "follow/find_room_by_external_id/10k",
            "value": 95.23852423933288,
            "range": "95.2071..95.2716",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 95.2071..95.2716 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "frame_proxy/10k",
            "value": 166341.47525807522,
            "range": "166134..166539",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 166134..166539 ns/iter\nThroughput input: {\"Elements\": 32430}"
          },
          {
            "name": "identification/by_title_and_description/10k",
            "value": 15424.97843161546,
            "range": "15402.9..15446.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 15402.9..15446.2 ns/iter\nThroughput input: {\"Elements\": 44}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy",
            "value": 533676125,
            "range": "5.32149e+08..5.35222e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 5.32149e+08..5.35222e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_heavy/no_raw",
            "value": 470060984.35,
            "range": "4.66529e+08..4.72675e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.66529e+08..4.72675e+08 ns/iter\nThroughput input: {\"Bytes\": 35014271}"
          },
          {
            "name": "ingest_pipeline/ansi_light",
            "value": 277831301.05,
            "range": "2.77639e+08..2.78067e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.77639e+08..2.78067e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/ansi_light/no_raw",
            "value": 230419453.26666665,
            "range": "2.30028e+08..2.3085e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.30028e+08..2.3085e+08 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "ingest_pipeline/iac_dense",
            "value": 281668847.45,
            "range": "2.81122e+08..2.82235e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.81122e+08..2.82235e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "ingest_pipeline/iac_dense/no_raw",
            "value": 229953920.9666667,
            "range": "2.29329e+08..2.30562e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.29329e+08..2.30562e+08 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "interop_delivery/emit_cross_isolate/S1",
            "value": 85887.43762833676,
            "range": "85806.5..85969.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 85806.5..85969.1 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S1",
            "value": 85854.45341160457,
            "range": "85646.5..86051.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 85646.5..86051.6 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_delivery/emit_fanout/S64",
            "value": 3837400.3824427486,
            "range": "3.83463e+06..3.8414e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.83463e+06..3.8414e+06 ns/iter\nThroughput input: {\"Elements\": 2048}"
          },
          {
            "name": "interop_delivery/emit_fanout/S8",
            "value": 513589.84050761414,
            "range": "512256..514865",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 512256..514865 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P16k",
            "value": 1621710.6454545457,
            "range": "1.62044e+06..1.62296e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.62044e+06..1.62296e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/emit_payload/P64",
            "value": 423233.392216582,
            "range": "422804..423841",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 422804..423841 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W64",
            "value": 1819031.9014492754,
            "range": "1.81648e+06..1.82159e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.81648e+06..1.82159e+06 ns/iter\nThroughput input: {\"Elements\": 1024}"
          },
          {
            "name": "interop_delivery/watch_coalesced/W8",
            "value": 283324.747740113,
            "range": "283203..283421",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 283203..283421 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_delivery/watch_per_write/W8",
            "value": 434284.9981754996,
            "range": "433935..434688",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 433935..434688 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "interop_ops/package/emit128",
            "value": 28100.682582852016,
            "range": "27978.3..28238.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 27978.3..28238.9 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/get128",
            "value": 68102.41676213857,
            "range": "67926.8..68310.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 67926.8..68310.1 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set128",
            "value": 99847.65742811502,
            "range": "99669.4..100008",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 99669.4..100008 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/package/set_per_turn64",
            "value": 466058.83744164323,
            "range": "465576..466550",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 465576..466550 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_ops/user/emit128",
            "value": 27795.726167601486,
            "range": "27689..27905.5",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 27689..27905.5 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/get128",
            "value": 65103.47314211212,
            "range": "65008.3..65212.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 65008.3..65212.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set128",
            "value": 74594.3939009842,
            "range": "74477.1..74727",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 74477.1..74727 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_ops/user/set_per_turn64",
            "value": 420217.77533557045,
            "range": "419773..420771",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 419773..420771 ns/iter\nThroughput input: {\"Elements\": 64}"
          },
          {
            "name": "interop_read/keys_32k",
            "value": 78151.49161817894,
            "range": "77997.5..78282.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 77997.5..78282.1 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "interop_read/materialize_32k",
            "value": 11998563.56744186,
            "range": "1.16305e+07..1.23706e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.16305e+07..1.23706e+07 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "interop_read/value_leaf/1k",
            "value": 73736.05934537915,
            "range": "73699.2..73787.6",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73699.2..73787.6 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/1m",
            "value": 72742.9851829988,
            "range": "72567.9..72929.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 72567.9..72929.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth1",
            "value": 73283.46481125092,
            "range": "73249.3..73310.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 73249.3..73310.8 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "interop_read/value_leaf/depth4",
            "value": 72487.32684807914,
            "range": "71944.1..73068.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 71944.1..73068.9 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "line_operations/replace_and_highlight",
            "value": 9277.981526005835,
            "range": "9274.72..9281.15",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 9274.72..9281.15 ns/iter\nThroughput input: {\"Elements\": 128}"
          },
          {
            "name": "path_parse/bracket",
            "value": 74.73281330656135,
            "range": "74.72..74.746",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 74.72..74.746 ns/iter"
          },
          {
            "name": "path_parse/depth1",
            "value": 49.08730666323963,
            "range": "49.0779..49.0967",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 49.0779..49.0967 ns/iter"
          },
          {
            "name": "path_parse/depth4",
            "value": 86.50890786403811,
            "range": "86.4987..86.5195",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 86.4987..86.5195 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/10k",
            "value": 516209.67215320905,
            "range": "515325..517164",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 515325..517164 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_hit/50k",
            "value": 445609.378782453,
            "range": "445323..445938",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 445323..445938 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/10k",
            "value": 3049558.919512195,
            "range": "3.04782e+06..3.05129e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.04782e+06..3.05129e+06 ns/iter"
          },
          {
            "name": "pathfinding/nearest_tag_miss/50k",
            "value": 25780269.840000004,
            "range": "2.57398e+07..2.5823e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.57398e+07..2.5823e+07 ns/iter"
          },
          {
            "name": "pathfinding/path_across/10k",
            "value": 2861398.615340909,
            "range": "2.85418e+06..2.8658e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.85418e+06..2.8658e+06 ns/iter"
          },
          {
            "name": "pathfinding/path_across/50k",
            "value": 21664402.713043477,
            "range": "2.15977e+07..2.17258e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.15977e+07..2.17258e+07 ns/iter"
          },
          {
            "name": "per_emit_composite/package",
            "value": 405.6816441041875,
            "range": "405.591..405.78",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 405.591..405.78 ns/iter"
          },
          {
            "name": "per_set_composite/package",
            "value": 358.31212430719626,
            "range": "358.1..358.52",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 358.1..358.52 ns/iter"
          },
          {
            "name": "per_set_composite/user",
            "value": 134.25942805929327,
            "range": "134.194..134.32",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 134.194..134.32 ns/iter"
          },
          {
            "name": "producer_parse/package",
            "value": 46.14858628712079,
            "range": "46.1416..46.156",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 46.1416..46.156 ns/iter"
          },
          {
            "name": "producer_parse/user",
            "value": 3.830529203426272,
            "range": "3.82985..3.83119",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 3.82985..3.83119 ns/iter"
          },
          {
            "name": "rebuild/room_connections/10k",
            "value": 28386952.905,
            "range": "2.80947e+07..2.86547e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.80947e+07..2.86547e+07 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "rebuild/room_connections/1k",
            "value": 1779684.6224913492,
            "range": "1.7729e+06..1.78543e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.7729e+06..1.78543e+06 ns/iter\nThroughput input: {\"Elements\": 1000}"
          },
          {
            "name": "rebuild/room_connections/50k",
            "value": 202741034.46666667,
            "range": "1.97971e+08..2.07398e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.97971e+08..2.07398e+08 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "scan_literals/aho_corasick_leftmost",
            "value": 15174212.773160173,
            "range": "1.5167e+07..1.51861e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.5167e+07..1.51861e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/aho_corasick_overlapping",
            "value": 17326686.385714285,
            "range": "1.73113e+07..1.73416e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.73113e+07..1.73416e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_filtered",
            "value": 400366314.35,
            "range": "3.99841e+08..4.0094e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.99841e+08..4.0094e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_literals/regex_set_current",
            "value": 30920998550.6,
            "range": "3.09136e+10..3.09284e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.09136e+10..3.09284e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_literals/tiered",
            "value": 50328994.266233765,
            "range": "5.03005e+07..5.03476e+07",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 5.03005e+07..5.03476e+07 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_filtered",
            "value": 494963612,
            "range": "4.94699e+08..4.95235e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.94699e+08..4.95235e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "scan_mixed/regex_set_current",
            "value": 31783984949.6,
            "range": "3.17613e+10..3.18079e+10",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.17613e+10..3.18079e+10 ns/iter\nThroughput input: {\"Bytes\": 1084294}"
          },
          {
            "name": "scan_mixed/tiered",
            "value": 169839313.11688313,
            "range": "1.69312e+08..1.70112e+08",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1.69312e+08..1.70112e+08 ns/iter\nThroughput input: {\"Bytes\": 16269045}"
          },
          {
            "name": "script_dispatch/baseline",
            "value": 344596.70820793434,
            "range": "344296..344881",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 344296..344881 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire0",
            "value": 1222255.6765281172,
            "range": "1.22187e+06..1.22267e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.22187e+06..1.22267e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire20",
            "value": 2972401.2213017745,
            "range": "2.96758e+06..2.97807e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2.96758e+06..2.97807e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "script_dispatch/fire5",
            "value": 1854949.0762962964,
            "range": "1.85277e+06..1.85751e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.85277e+06..1.85751e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "sgr/process/bold_color",
            "value": 32.32172809919902,
            "range": "32.3148..32.3287",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 32.3148..32.3287 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/color_256",
            "value": 51.292808740398684,
            "range": "51.2865..51.2999",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 51.2865..51.2999 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/reset",
            "value": 21.272384473684784,
            "range": "21.2688..21.276",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.2688..21.276 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/simple_color",
            "value": 21.578726794913212,
            "range": "21.5738..21.5835",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.5738..21.5835 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "sgr/process/truecolor",
            "value": 92.75436685508775,
            "range": "92.7292..92.7805",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 92.7292..92.7805 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "spatial_query/connections/viewport_full/10k",
            "value": 81995.4006736773,
            "range": "81957.8..82032.3",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 81957.8..82032.3 ns/iter\nThroughput input: {\"Elements\": 19802}"
          },
          {
            "name": "spatial_query/connections/viewport_full/50k",
            "value": 442376.6107773852,
            "range": "442059..442684",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 442059..442684 ns/iter\nThroughput input: {\"Elements\": 99557}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/10k",
            "value": 18955.46202924907,
            "range": "18947..18964.4",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 18947..18964.4 ns/iter\nThroughput input: {\"Elements\": 4416}"
          },
          {
            "name": "spatial_query/connections/viewport_medium/50k",
            "value": 93017.92479734709,
            "range": "92964.3..93085.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 92964.3..93085.9 ns/iter\nThroughput input: {\"Elements\": 21021}"
          },
          {
            "name": "spatial_query/connections/viewport_small/10k",
            "value": 2933.335988186844,
            "range": "2929.18..2938.05",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 2929.18..2938.05 ns/iter\nThroughput input: {\"Elements\": 576}"
          },
          {
            "name": "spatial_query/connections/viewport_small/50k",
            "value": 10789.17620996403,
            "range": "10763..10815.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 10763..10815.2 ns/iter\nThroughput input: {\"Elements\": 2359}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/10k",
            "value": 38557.330929981494,
            "range": "38545.3..38568.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 38545.3..38568.8 ns/iter\nThroughput input: {\"Elements\": 10000}"
          },
          {
            "name": "spatial_query/rooms/viewport_full/50k",
            "value": 195600.88414062498,
            "range": "195496..195707",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 195496..195707 ns/iter\nThroughput input: {\"Elements\": 50000}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/10k",
            "value": 8964.931702200145,
            "range": "8959.92..8969.87",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 8959.92..8969.87 ns/iter\nThroughput input: {\"Elements\": 2070}"
          },
          {
            "name": "spatial_query/rooms/viewport_medium/50k",
            "value": 40055.54860067168,
            "range": "40020.7..40092.9",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 40020.7..40092.9 ns/iter\nThroughput input: {\"Elements\": 10306}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/10k",
            "value": 1093.6925712591033,
            "range": "1087.7..1099.2",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1087.7..1099.2 ns/iter\nThroughput input: {\"Elements\": 240}"
          },
          {
            "name": "spatial_query/rooms/viewport_small/50k",
            "value": 4859.968560286371,
            "range": "4848.99..4872.45",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4848.99..4872.45 ns/iter\nThroughput input: {\"Elements\": 1146}"
          },
          {
            "name": "styled_line/new_no_raw/long_plain",
            "value": 22.107445232067494,
            "range": "21.9834..22.2166",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 21.9834..22.2166 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/long_styled",
            "value": 36.654835007193356,
            "range": "36.6341..36.6785",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 36.6341..36.6785 ns/iter\nThroughput input: {\"Bytes\": 200}"
          },
          {
            "name": "styled_line/new_no_raw/short_plain",
            "value": 19.91045418490198,
            "range": "19.9073..19.9139",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 19.9073..19.9139 ns/iter\nThroughput input: {\"Bytes\": 40}"
          },
          {
            "name": "styled_line/new_with_raw/long_plain",
            "value": 105.06423785141554,
            "range": "104.843..105.253",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 104.843..105.253 ns/iter\nThroughput input: {\"Bytes\": 400}"
          },
          {
            "name": "styled_line/new_with_raw/long_styled",
            "value": 130.7494937402728,
            "range": "130.7..130.808",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 130.7..130.808 ns/iter\nThroughput input: {\"Bytes\": 464}"
          },
          {
            "name": "styled_line/new_with_raw/short_plain",
            "value": 37.71774712144428,
            "range": "37.6819..37.7485",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 37.6819..37.7485 ns/iter\nThroughput input: {\"Bytes\": 80}"
          },
          {
            "name": "telnet_receive/ansi_light",
            "value": 292800.45987111895,
            "range": "292604..292995",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 292604..292995 ns/iter\nThroughput input: {\"Bytes\": 19876170}"
          },
          {
            "name": "telnet_receive/iac_dense",
            "value": 4397796.46140351,
            "range": "4.39614e+06..4.39957e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 4.39614e+06..4.39957e+06 ns/iter\nThroughput input: {\"Bytes\": 20722866}"
          },
          {
            "name": "to_spans/by_span_count/1",
            "value": 62.156315648378445,
            "range": "62.1366..62.1777",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 62.1366..62.1777 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "to_spans/by_span_count/32",
            "value": 1299.3530059941013,
            "range": "1298.93..1299.8",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 1298.93..1299.8 ns/iter\nThroughput input: {\"Elements\": 32}"
          },
          {
            "name": "to_spans/by_span_count/8",
            "value": 371.0401807654121,
            "range": "370.884..371.201",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: slope\nSampling: Linear\n95% CI: 370.884..371.201 ns/iter\nThroughput input: {\"Elements\": 8}"
          },
          {
            "name": "trigger_verbs/empty",
            "value": 1055838.2489451475,
            "range": "1.05505e+06..1.05662e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.05505e+06..1.05662e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/gag",
            "value": 1075632.9142548598,
            "range": "1.07516e+06..1.076e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.07516e+06..1.076e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/highlight",
            "value": 1192398.7088729017,
            "range": "1.19131e+06..1.19338e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.19131e+06..1.19338e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "trigger_verbs/read_echo",
            "value": 1445276.1242074927,
            "range": "1.44263e+06..1.44928e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.44263e+06..1.44928e+06 ns/iter\nThroughput input: {\"Elements\": 500}"
          },
          {
            "name": "upsert_room/single/10k",
            "value": 968650.8715909092,
            "range": "960199..976748",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 960199..976748 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/1k",
            "value": 908749.6756183745,
            "range": "902352..916021",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 902352..916021 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_room/single/50k",
            "value": 1402294.3961956524,
            "range": "1.39432e+06..1.41013e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.39432e+06..1.41013e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/1",
            "value": 1014991.279435484,
            "range": "1.0123e+06..1.01738e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.0123e+06..1.01738e+06 ns/iter\nThroughput input: {\"Elements\": 1}"
          },
          {
            "name": "upsert_rooms/batch_10k/16",
            "value": 1178603.3653301888,
            "range": "1.17554e+06..1.18203e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 1.17554e+06..1.18203e+06 ns/iter\nThroughput input: {\"Elements\": 16}"
          },
          {
            "name": "upsert_rooms/batch_10k/256",
            "value": 3377999.359060403,
            "range": "3.36244e+06..3.39153e+06",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 3.36244e+06..3.39153e+06 ns/iter\nThroughput input: {\"Elements\": 256}"
          },
          {
            "name": "write_and_flush/J16/W8_mixed",
            "value": 17627.134120725626,
            "range": "17619..17636.1",
            "unit": "ns/iter",
            "extra": "Run: main push\nSource: cb13a2ddf32ae1cdee6b22517461ba917f60dcd3\nAMI: ami-0cd54adbad90ecaa2\nCriterion statistic: mean\nSampling: Flat\n95% CI: 17619..17636.1 ns/iter\nThroughput input: {\"Elements\": 16}"
          }
        ]
      }
    ]
  }
}