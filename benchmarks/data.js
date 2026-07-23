window.BENCHMARK_DATA = {
  "lastUpdate": 1784787574481,
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
      }
    ]
  }
}