//! Catalogue inputs and preset classifications shared by the GPU scenarios.
pub struct Case {
    pub name: &'static str,
    pub source: &'static str,
    pub params: serde_json::Value,
    pub outset: u16,
    pub pane: bool,
}
pub fn kinetic_signal(name: &str) -> bool {
    matches!(
        name,
        "unarmed"
            | "knockback"
            | "crosscut"
            | "aftershock"
            | "overdrive"
            | "ricochet"
            | "rebound"
            | "focusLock"
            | "victoryStamp"
            | "lastStand"
    )
}
pub fn attention_burst(name: &str) -> bool {
    matches!(
        name,
        "sonar"
            | "converge"
            | "beacon"
            | "cometCall"
            | "prismSweep"
            | "alarm"
            | "echoFrame"
            | "vitalSign"
    )
}
pub fn transformation_batch(name: &str) -> bool {
    matches!(
        name,
        "eventHorizon"
            | "dragonBreath"
            | "vanish"
            | "psionic"
            | "weightPulse"
            | "decay"
            | "lava"
            | "crystal"
    )
}
pub fn material_batch(name: &str) -> bool {
    transformation_batch(name)
        || matches!(
            name,
            "phoenixRebirth"
                | "astralConjunction"
                | "interruptNow"
                | "spatialStitch"
                | "poisoned"
                | "charm"
                | "geas"
                | "verdant"
                | "stoneSkin"
                | "gilding"
        )
}
pub fn anchored_intro(name: &str) -> bool {
    material_batch(name)
        || matches!(
            name,
            "blackTide"
                | "eldritchEcho"
                | "aberration"
                | "rootbind"
                | "autumn"
                | "spellSeal"
                | "leyWeave"
                | "elderSign"
                | "voidBloom"
                | "watchers"
                | "mycelium"
                | "barkMend"
                | "ritualKnot"
                | "counterSeal"
                | "soulDrain"
                | "silence"
                | "possession"
                | "trueSight"
                | "forgetting"
        )
}
pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "lastStand",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":9,"amplitude":1.0,"base":[0.572549,0.105882,0.235294,1],"bright":[1.0,0.254902,0.4,1],"accent":[1.0,0.901961,0.819608,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "victoryStamp",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":8,"amplitude":1.0,"base":[0.647059,0.431373,0.137255,1],"bright":[1.0,0.803922,0.396078,1],"accent":[1.0,1.0,0.87451,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "focusLock",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":7,"amplitude":1.0,"base":[0.443137,0.32549,0.631373,1],"bright":[0.772549,0.631373,1.0,1],"accent":[0.980392,0.941176,1.0,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "rebound",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":6,"amplitude":1.0,"base":[0.207843,0.419608,0.458824,1],"bright":[0.490196,0.882353,0.811765,1],"accent":[0.941176,1.0,0.905882,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "ricochet",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":5,"amplitude":1.0,"base":[0.619608,0.396078,0.12549,1],"bright":[1.0,0.788235,0.329412,1],"accent":[1.0,0.964706,0.85098,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "overdrive",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":4,"amplitude":1.0,"base":[0.090196,0.368627,0.592157,1],"bright":[0.223529,0.858824,0.980392,1],"accent":[0.945098,1.0,1.0,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "aftershock",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":3,"amplitude":1.0,"base":[0.266667,0.403922,0.643137,1],"bright":[0.513725,0.788235,1.0,1],"accent":[0.945098,0.984314,1.0,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "crosscut",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":2,"amplitude":1.0,"base":[0.596078,0.168627,0.25098,1],"bright":[1.0,0.443137,0.509804,1],"accent":[1.0,0.941176,0.858824,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "knockback",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":1,"amplitude":1.0,"base":[0.568627,0.301961,0.137255,1],"bright":[1.0,0.768627,0.419608,1],"accent":[1.0,0.956863,0.862745,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "unarmed",
            source: include_str!("../../packages/text-effects/kinetic-signals.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":0,"amplitude":1.0,"base":[0.709804,0.121569,0.196078,1],"bright":[1.0,0.333333,0.231373,1],"accent":[1.0,0.941176,0.792157,1]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "eventHorizon",
            source: include_str!("../../packages/text-effects/event-horizon.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.317647,0.270588,0.470588,1.0],"bright":[0.874510,0.654902,0.419608,1.0],"accent":[1.000000,0.949020,0.854902,1.0]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "dragonBreath",
            source: include_str!("../../packages/text-effects/dragon-breath.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.490196,0.141176,0.113725,1.0],"bright":[1.000000,0.458824,0.109804,1.0],"accent":[1.000000,0.949020,0.756863,1.0]}),
            outset: 512,
            pane: true,
        },
        Case {
            name: "vanish",
            source: include_str!("../../packages/text-effects/pressure-ink.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"amplitude":1.0,"variant":4,"base":[0.258824,0.352941,0.482353,1.0],"bright":[0.678431,0.819608,0.937255,1.0],"accent":[0.913725,0.964706,1.000000,1.0]}),
            outset: 6,
            pane: false,
        },
        Case {
            name: "psionic",
            source: include_str!("../../packages/text-effects/pressure-ink.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"amplitude":1.0,"variant":1,"base":[0.258824,0.352941,0.482353,1.0],"bright":[0.678431,0.819608,0.937255,1.0],"accent":[1.000000,1.000000,1.000000,1.0]}),
            outset: 12,
            pane: false,
        },
        Case {
            name: "weightPulse",
            source: include_str!("../../packages/text-effects/pressure-ink.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"amplitude":1.0,"variant":0,"base":[0.258824,0.352941,0.482353,1.0],"bright":[0.678431,0.819608,0.937255,1.0],"accent":[1.000000,1.000000,1.000000,1.0]}),
            outset: 6,
            pane: false,
        },
        Case {
            name: "decay",
            source: include_str!("../../packages/text-effects/transmuted-ink.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":2,"base":[0.384314,0.345098,0.274510,1.0],"bright":[0.788235,0.603922,0.380392,1.0],"accent":[0.874510,0.768627,0.603922,1.0]}),
            outset: 14,
            pane: false,
        },
        Case {
            name: "lava",
            source: include_str!("../../packages/text-effects/transmuted-ink.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":1,"base":[0.298039,0.270588,0.270588,1.0],"bright":[1.000000,0.301961,0.078431,1.0],"accent":[1.000000,0.941176,0.631373,1.0]}),
            outset: 4,
            pane: false,
        },
        Case {
            name: "crystal",
            source: include_str!("../../packages/text-effects/transmuted-ink.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":0,"base":[0.160784,0.309804,0.537255,1.0],"bright":[0.564706,0.850980,0.929412,1.0],"accent":[1.000000,0.968627,1.000000,1.0]}),
            outset: 4,
            pane: false,
        },
        Case {
            name: "vitalSign",
            source: include_str!("../../packages/text-effects/attention-bursts.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":7.0,"base":[0.074510,0.450980,0.427451,1.0],"bright":[0.337255,0.941176,0.768627,1.0],"accent":[0.937255,1.000000,1.000000,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "echoFrame",
            source: include_str!("../../packages/text-effects/attention-bursts.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":6.0,"base":[0.137255,0.423529,0.337255,1.0],"bright":[0.450980,0.909804,0.690196,1.0],"accent":[0.937255,1.000000,0.956863,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "alarm",
            source: include_str!("../../packages/text-effects/attention-bursts.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":5.0,"base":[0.662745,0.129412,0.192157,1.0],"bright":[1.000000,0.325490,0.368627,1.0],"accent":[1.000000,0.882353,0.823529,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "prismSweep",
            source: include_str!("../../packages/text-effects/attention-bursts.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":4.0,"base":[0.454902,0.258824,0.749020,1.0],"bright":[0.333333,0.850980,0.909804,1.0],"accent":[1.000000,0.960784,0.929412,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "cometCall",
            source: include_str!("../../packages/text-effects/attention-bursts.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":3.0,"base":[0.101961,0.360784,0.568627,1.0],"bright":[0.396078,0.839216,1.000000,1.0],"accent":[1.000000,0.956863,0.862745,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "beacon",
            source: include_str!("../../packages/text-effects/attention-bursts.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":2.0,"base":[0.545098,0.345098,0.113725,1.0],"bright":[1.000000,0.827451,0.419608,1.0],"accent":[1.000000,0.972549,0.874510,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "converge",
            source: include_str!("../../packages/text-effects/attention-bursts.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":1.0,"base":[0.403922,0.305882,0.690196,1.0],"bright":[0.717647,0.627451,1.000000,1.0],"accent":[1.000000,0.960784,1.000000,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "sonar",
            source: include_str!("../../packages/text-effects/attention-bursts.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":0.0,"base":[0.066667,0.341176,0.474510,1.0],"bright":[0.278431,0.807843,0.898039,1.0],"accent":[0.894118,1.000000,1.000000,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "phoenixRebirth",
            source: include_str!("../../packages/text-effects/phoenix-rebirth.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.501961,0.235294,0.192157,1.0],"bright":[0.952941,0.639216,0.231373,1.0],"accent":[1.000000,0.956863,0.807843,1.0]}),
            outset: 256,
            pane: true,
        },
        Case {
            name: "astralConjunction",
            source: include_str!("../../packages/text-effects/astral-conjunction.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.533333,0.529412,0.741176,1.0],"bright":[0.749020,0.850980,0.921569,1.0],"accent":[1.000000,0.972549,0.890196,1.0]}),
            outset: 256,
            pane: true,
        },
        Case {
            name: "interruptNow",
            source: include_str!("../../packages/text-effects/interrupt-now.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.705882,0.278431,0.258824,1.0],"bright":[0.925490,0.666667,0.447059,1.0],"accent":[1.000000,0.941176,0.811765,1.0]}),
            outset: 8,
            pane: false,
        },
        Case {
            name: "spatialStitch",
            source: include_str!("../../packages/text-effects/spatial-stitch.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.266667,0.415686,0.443137,1.0],"bright":[0.701961,0.843137,0.803922,1.0],"accent":[0.949020,0.941176,0.843137,1.0]}),
            outset: 16,
            pane: false,
        },
        Case {
            name: "poisoned",
            source: include_str!("../../packages/text-effects/poisoned.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.439216,0.321569,0.462745,1.0],"bright":[0.733333,0.807843,0.423529,1.0],"accent":[0.933333,0.882353,0.709804,1.0]}),
            outset: 2,
            pane: false,
        },
        Case {
            name: "charm",
            source: include_str!("../../packages/text-effects/charm.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.607843,0.392157,0.439216,1.0],"bright":[0.917647,0.721569,0.639216,1.0],"accent":[1.000000,0.941176,0.807843,1.0]}),
            outset: 12,
            pane: false,
        },
        Case {
            name: "geas",
            source: include_str!("../../packages/text-effects/geas.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.349020,0.270588,0.223529,1.0],"bright":[0.721569,0.635294,0.509804,1.0],"accent":[0.956863,0.858824,0.670588,1.0]}),
            outset: 16,
            pane: false,
        },
        Case {
            name: "verdant",
            source: include_str!("../../packages/text-effects/verdant.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.145098,0.329412,0.235294,1.0],"bright":[0.541176,0.776471,0.419608,1.0],"accent":[0.913725,0.929412,0.690196,1.0]}),
            outset: 24,
            pane: false,
        },
        Case {
            name: "stoneSkin",
            source: include_str!("../../packages/text-effects/stone-skin.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.243137,0.266667,0.282353,1.0],"bright":[0.698039,0.725490,0.694118,1.0],"accent":[0.921569,0.905882,0.807843,1.0]}),
            outset: 2,
            pane: false,
        },
        Case {
            name: "gilding",
            source: include_str!("../../packages/text-effects/gilding.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.462745,0.313725,0.113725,1.0],"bright":[1.0,0.847059,0.470588,1.0],"accent":[1.0,0.972549,0.862745,1.0]}),
            outset: 2,
            pane: false,
        },
        Case {
            name: "silence",
            source: include_str!("../../packages/text-effects/silence.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.278431,0.325490,0.388235,1.0],"bright":[0.603922,0.666667,0.733333,1.0],"accent":[0.858824,0.886275,0.921569,1.0]}),
            outset: 24,
            pane: false,
        },
        Case {
            name: "possession",
            source: include_str!("../../packages/text-effects/possession.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.349020,0.176471,0.207843,1.0],"bright":[0.800000,0.396078,0.345098,1.0],"accent":[1.000000,0.886275,0.725490,1.0]}),
            outset: 24,
            pane: false,
        },
        Case {
            name: "trueSight",
            source: include_str!("../../packages/text-effects/true-sight.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.400000,0.333333,0.486275,1.0],"bright":[0.545098,0.749020,0.792157,1.0],"accent":[0.937255,1.000000,1.000000,1.0]}),
            outset: 32,
            pane: false,
        },
        Case {
            name: "forgetting",
            source: include_str!("../../packages/text-effects/forgetting.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.294118,0.325490,0.380392,1.0],"bright":[0.603922,0.658824,0.729412,1.0],"accent":[0.874510,0.898039,0.937255,1.0]}),
            outset: 24,
            pane: false,
        },
        Case {
            name: "soulDrain",
            source: include_str!("../../packages/text-effects/soul-drain.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.129,0.306,0.329,1.0],"bright":[0.467,0.784,0.792,1.0],"accent":[0.851,1.0,0.941,1.0]}),
            outset: 96,
            pane: false,
        },
        Case {
            name: "mycelium",
            source: include_str!("../../packages/text-effects/living-ink.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":0.0,"base":[0.188,0.306,0.251,1.0],"bright":[0.518,0.729,0.596,1.0],"accent":[0.929,0.910,0.784,1.0]}),
            outset: 36,
            pane: false,
        },
        Case {
            name: "barkMend",
            source: include_str!("../../packages/text-effects/living-ink.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":1.0,"base":[0.345,0.251,0.165,1.0],"bright":[0.608,0.733,0.420,1.0],"accent":[0.878,0.945,0.678,1.0]}),
            outset: 12,
            pane: false,
        },
        Case {
            name: "ritualKnot",
            source: include_str!("../../packages/text-effects/ritual-nets.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":0.0,"base":[0.294,0.216,0.431,1.0],"bright":[0.671,0.525,0.855,1.0],"accent":[0.945,0.851,1.0,1.0]}),
            outset: 64,
            pane: false,
        },
        Case {
            name: "counterSeal",
            source: include_str!("../../packages/text-effects/ritual-nets.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":1.0,"base":[0.788,0.278,0.267,1.0],"bright":[0.447,0.800,0.847,1.0],"accent":[0.929,1.0,1.0,1.0]}),
            outset: 64,
            pane: false,
        },
        Case {
            name: "voidBloom",
            source: include_str!("../../packages/text-effects/void-bloom.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.020,0.012,0.043,1.0],"bright":[0.349,0.200,0.471,1.0],"accent":[0.863,0.788,0.925,1.0]}),
            outset: 96,
            pane: false,
        },
        Case {
            name: "watchers",
            source: include_str!("../../packages/text-effects/watchers.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.047,0.063,0.082,1.0],"bright":[0.416,0.569,0.584,1.0],"accent":[0.847,0.906,0.859,1.0]}),
            outset: 24,
            pane: false,
        },
        Case {
            name: "spellSeal",
            source: include_str!("../../packages/text-effects/ritual.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":0.0,"base":[0.459,0.376,0.220,1.0],"bright":[0.867,0.761,0.478,1.0],"accent":[1.0,0.957,0.820,1.0]}),
            outset: 64,
            pane: false,
        },
        Case {
            name: "leyWeave",
            source: include_str!("../../packages/text-effects/ritual.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":1.0,"base":[0.192,0.345,0.529,1.0],"bright":[0.510,0.835,0.835,1.0],"accent":[0.914,1.0,1.0,1.0]}),
            outset: 48,
            pane: false,
        },
        Case {
            name: "elderSign",
            source: include_str!("../../packages/text-effects/elder-sign.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.0275,0.0431,0.0431,1.0],"bright":[0.396,0.529,0.475,1.0],"accent":[0.835,0.918,0.847,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "autumn",
            source: include_str!("../../packages/text-effects/autumn.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"amplitude":1.0,"mode":2.0,"base":[0.596,0.275,0.145,1.0],"bright":[0.859,0.620,0.224,1.0],"accent":[0.969,0.816,0.553,1.0]}),
            outset: 160,
            pane: false,
        },
        Case {
            name: "rootbind",
            source: include_str!("../../packages/text-effects/rootbind.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.286,0.231,0.145,1.0],"bright":[0.545,0.60,0.325,1.0],"accent":[0.867,0.820,0.631,1.0]}),
            outset: 28,
            pane: false,
        },
        Case {
            name: "eldritchEcho",
            source: include_str!("../../packages/text-effects/eldritch.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":0.0,"amplitude":1.0,"base":[0.153,0.157,0.239,1.0],"bright":[0.435,0.494,0.671,1.0],"accent":[0.835,0.851,0.914,1.0]}),
            outset: 40,
            pane: false,
        },
        Case {
            name: "aberration",
            source: include_str!("../../packages/text-effects/eldritch.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"variant":1.0,"amplitude":1.0,"base":[0.208,0.141,0.255,1.0],"bright":[0.60,0.498,0.682,1.0],"accent":[0.906,0.831,0.914,1.0]}),
            outset: 40,
            pane: false,
        },
        Case {
            name: "blackTide",
            source: include_str!("../../packages/text-effects/black-tide.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.0196,0.0275,0.0627,1.0],"bright":[0.2824,0.2667,0.3490,1.0],"accent":[0.7882,0.7255,0.8588,1.0]}),
            outset: 28,
            pane: false,
        },
        Case {
            name: "emphasis",
            source: include_str!("../../packages/text-effects/emphasis.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"peak":3.0,"yaw":0.35,"weight":0.5,"pivot":0.5,"fit":1.0,"base":[0.443,0.572,0.659,1.0],"bright":[0.902,0.949,1.0,1.0],"accent":[1.0,1.0,1.0,1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "tide",
            source: include_str!("../../packages/text-effects/motion.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.44,0.57,0.66,1.0],"bright":[0.9,0.95,1.0,1.0],"accent":[1.0,1.0,1.0,1.0],"variant":0.0,"amplitude":1.0}),
            outset: 96,
            pane: true,
        },
        Case {
            name: "elastic",
            source: include_str!("../../packages/text-effects/motion.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.44,0.57,0.66,1.0],"bright":[0.9,0.95,1.0,1.0],"accent":[1.0,1.0,1.0,1.0],"variant":1.0,"amplitude":1.0}),
            outset: 96,
            pane: true,
        },
        Case {
            name: "quake",
            source: include_str!("../../packages/text-effects/motion.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.44,0.57,0.66,1.0],"bright":[0.9,0.95,1.0,1.0],"accent":[1.0,1.0,1.0,1.0],"variant":2.0,"amplitude":1.0}),
            outset: 96,
            pane: true,
        },
        Case {
            name: "explode",
            source: include_str!("../../packages/text-effects/explode.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.44,0.57,0.66,1.0],"bright":[0.9,0.95,1.0,1.0],"accent":[1.0,1.0,1.0,1.0],"pieces":3.0,"depth":1.0,"spin":1.0,"spread":1.0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "implode",
            source: include_str!("../../packages/text-effects/implode.wgsl"),
            params: serde_json::json!({"intensity":1.0,"speed":1.0,"base":[0.275,0.424,0.769,1.0],"bright":[0.67,0.875,1.0,1.0],"accent":[1.0,1.0,1.0,1.0],"pieces":3.0,"depth":1.0,"spin":1.0,"spread":1.0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "wind",
            source: include_str!("../../packages/text-effects/grains.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 0.0, "amplitude": 1.0, "mode": 2.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1], "direction": 1.0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "sand",
            source: include_str!("../../packages/text-effects/grains.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 1.0, "amplitude": 1.0, "mode": 2.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1], "direction": 1.0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "recoil",
            source: include_str!("../../packages/text-effects/kinetic.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 0.0, "amplitude": 1.0, "mode": 2.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "accordion",
            source: include_str!("../../packages/text-effects/kinetic.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 1.0, "amplitude": 1.0, "mode": 0.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "pendulum",
            source: include_str!("../../packages/text-effects/kinetic.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 2.0, "amplitude": 1.0, "mode": 2.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "domino",
            source: include_str!("../../packages/text-effects/kinetic.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 3.0, "amplitude": 1.0, "mode": 2.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "tumble",
            source: include_str!("../../packages/text-effects/kinetic.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 4.0, "amplitude": 1.0, "mode": 0.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "magnetic",
            source: include_str!("../../packages/text-effects/kinetic.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 5.0, "amplitude": 1.0, "mode": 0.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "vortex",
            source: include_str!("../../packages/text-effects/kinetic.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 6.0, "amplitude": 1.0, "mode": 0.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "shatter",
            source: include_str!("../../packages/text-effects/ribbons.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 0.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1], "mode": 2.0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "unravel",
            source: include_str!("../../packages/text-effects/ribbons.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 1.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1], "mode": 2.0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "liquefy",
            source: include_str!("../../packages/text-effects/ink.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 0.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1], "mode": 2.0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "inkBloom",
            source: include_str!("../../packages/text-effects/ink.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 1.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1], "mode": 0.0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "duel",
            source: include_str!("../../packages/text-effects/hero.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 0.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "eclipse",
            source: include_str!("../../packages/text-effects/hero.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 1.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "dimensionalRift",
            source: include_str!("../../packages/text-effects/hero.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 2.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "shockfront",
            source: include_str!("../../packages/text-effects/hero.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 3.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "judgement",
            source: include_str!("../../packages/text-effects/hero.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 4.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "ascension",
            source: include_str!("../../packages/text-effects/hero.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "variant": 5.0, "amplitude": 1.0, "base": [0.44, 0.57, 0.66, 1], "bright": [0.9, 0.95, 1, 1], "accent": [1, 1, 1, 1]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "fire",
            source: include_str!("../../packages/text-effects/fire.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.9294117647058824, 0.21176470588235294, 0.0392156862745098, 1.0], "bright": [1.0, 0.7019607843137254, 0.10196078431372549, 1.0], "accent": [1.0, 0.9529411764705882, 0.7490196078431373, 1.0], "embers": 1}),
            outset: 88,
            pane: false,
        },
        Case {
            name: "smoke",
            source: include_str!("../../packages/text-effects/smoke.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.3215686274509804, 0.3764705882352941, 0.43137254901960786, 1.0], "bright": [0.6941176470588235, 0.7411764705882353, 0.8, 1.0], "accent": [0.8862745098039215, 0.9098039215686274, 0.9294117647058824, 1.0]}),
            outset: 56,
            pane: false,
        },
        Case {
            name: "reverseSmoke",
            source: include_str!("../../packages/text-effects/reverse-smoke.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.32157,0.37647,0.43137,1.0], "bright": [0.69412,0.74118,0.8,1.0], "accent": [0.88627,0.9098,0.92941,1.0]}),
            outset: 144,
            pane: false,
        },
        Case {
            name: "electricity",
            source: include_str!("../../packages/text-effects/electricity.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.19607843137254902, 0.38823529411764707, 1.0, 1.0], "bright": [0.5019607843137255, 0.9176470588235294, 1.0, 1.0], "accent": [0.9529411764705882, 1.0, 1.0, 1.0]}),
            outset: 12,
            pane: false,
        },
        Case {
            name: "lightning",
            source: include_str!("../../packages/text-effects/lightning.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.4392156862745098, 0.29411764705882354, 1.0, 1.0], "bright": [0.611764705882353, 0.6784313725490196, 1.0, 1.0], "accent": [1.0, 1.0, 1.0, 1.0]}),
            outset: 40,
            pane: false,
        },
        Case {
            name: "frost",
            source: include_str!("../../packages/text-effects/frost.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.19607843137254902, 0.43137254901960786, 0.6823529411764706, 1.0], "bright": [0.5686274509803921, 0.9019607843137255, 1.0, 1.0], "accent": [0.9490196078431372, 0.9882352941176471, 1.0, 1.0]}),
            outset: 32,
            pane: false,
        },
        Case {
            name: "ward",
            source: include_str!("../../packages/text-effects/ward.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.2627450980392157, 0.3568627450980392, 0.7411764705882353, 1.0], "bright": [0.6352941176470588, 0.5490196078431373, 1.0, 1.0], "accent": [0.9098039215686274, 0.8784313725490196, 1.0, 1.0]}),
            outset: 14,
            pane: false,
        },
        Case {
            name: "impact",
            source: include_str!("../../packages/text-effects/impact.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.8627450980392157, 0.3411764705882353, 0.1411764705882353, 1.0], "bright": [1.0, 0.8274509803921568, 0.3607843137254902, 1.0], "accent": [1.0, 0.9686274509803922, 0.8745098039215686, 1.0]}),
            outset: 260,
            pane: false,
        },
        Case {
            name: "animeImpact",
            source: include_str!("../../packages/text-effects/anime-impact.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.8627, 0.9020, 0.9373, 1.0], "bright": [1.0, 1.0, 1.0, 1.0], "accent": [1.0, 1.0, 1.0, 1.0]}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "holyGleam",
            source: include_str!("../../packages/text-effects/holy-gleam.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.8392156862745098, 0.5372549019607843, 0.07058823529411765, 1.0], "bright": [1.0, 0.9098039215686274, 0.6274509803921569, 1.0], "accent": [1.0, 1.0, 1.0, 1.0]}),
            outset: 320,
            pane: false,
        },
        Case {
            name: "poisonCloud",
            source: include_str!("../../packages/text-effects/cloud.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.20392156862745098, 0.396078431372549, 0.13725490196078433, 1.0], "bright": [0.5450980392156862, 0.8901960784313725, 0.2549019607843137, 1.0], "accent": [0.8509803921568627, 1.0, 0.6078431372549019, 1.0], "particles": 1, "swirl": 1, "symbol": 0}),
            outset: 112,
            pane: false,
        },
        Case {
            name: "healingCloud",
            source: include_str!("../../packages/text-effects/cloud.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.12549019607843137, 0.34509803921568627, 0.2980392156862745, 1.0], "bright": [0.403921568627451, 0.9215686274509803, 0.7411764705882353, 1.0], "accent": [1.0, 0.7490196078431373, 0.807843137254902, 1.0], "particles": 1, "swirl": 1, "symbol": 1}),
            outset: 112,
            pane: false,
        },
        Case {
            name: "radiate",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [1.0, 0.18823529411764706, 0.18823529411764706, 1.0], "bright": [1.0, 0.3137254901960784, 0.3137254901960784, 1.0], "accent": [1.0, 0.5019607843137255, 0.5019607843137255, 1.0], "variant": 0}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "greeting",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.15294117647058825, 0.611764705882353, 0.4627450980392157, 1.0], "bright": [0.4588235294117647, 0.8745098039215686, 0.6784313725490196, 1.0], "accent": [1.0, 0.8, 0.8352941176470589, 1.0], "variant": 1}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "hostile",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.6980392156862745, 0.10980392156862745, 0.18823529411764706, 1.0], "bright": [1.0, 0.23921568627450981, 0.2823529411764706, 1.0], "accent": [1.0, 0.8156862745098039, 0.7372549019607844, 1.0], "variant": 2}),
            outset: 36,
            pane: false,
        },
        Case {
            name: "dread",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.17647058823529413, 0.08627450980392157, 0.24313725490196078, 1.0], "bright": [0.6627450980392157, 0.21176470588235294, 0.44313725490196076, 1.0], "accent": [0.9411764705882353, 0.3803921568627451, 0.5058823529411764, 1.0], "variant": 3}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "quest",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.6745098039215687, 0.4823529411764706, 0.12941176470588237, 1.0], "bright": [1.0, 0.8784313725490196, 0.47058823529411764, 1.0], "accent": [1.0, 0.9490196078431372, 0.7411764705882353, 1.0], "variant": 4}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "levelUp",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.7529411764705882, 0.4745098039215686, 0.08627450980392157, 1.0], "bright": [1.0, 0.803921568627451, 0.3333333333333333, 1.0], "accent": [1.0, 0.9686274509803922, 0.788235294117647, 1.0], "variant": 5}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "healNeeded",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.0784313725490196, 0.4666666666666667, 0.3607843137254902, 1.0], "bright": [0.3803921568627451, 0.8980392156862745, 0.7137254901960784, 1.0], "accent": [0.9411764705882353, 1.0, 0.9764705882352941, 1.0], "variant": 6}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "expiring",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.3607843137254902, 0.40784313725490196, 0.4745098039215686, 1.0], "bright": [0.6941176470588235, 0.7372549019607844, 0.803921568627451, 1.0], "accent": [0.9490196078431372, 0.8392156862745098, 0.6431372549019608, 1.0], "variant": 7}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "rally",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.20392156862745098, 0.35294117647058826, 0.6862745098039216, 1.0], "bright": [0.4392156862745098, 0.7215686274509804, 1.0, 1.0], "accent": [0.7764705882352941, 0.9647058823529412, 1.0, 1.0], "variant": 8}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "discovery",
            source: include_str!("../../packages/text-effects/attention.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.403921568627451, 0.3411764705882353, 0.7490196078431373, 1.0], "bright": [0.6588235294117647, 0.6274509803921569, 1.0, 1.0], "accent": [0.8980392156862745, 1.0, 1.0, 1.0], "variant": 9}),
            outset: 400,
            pane: true,
        },
        Case {
            name: "acid",
            source: include_str!("../../packages/text-effects/acid.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.30196078431372547, 0.10196078431372549, 0.47058823529411764, 1.0], "bright": [0.6784313725490196, 0.3568627450980392, 0.9098039215686274, 1.0], "accent": [0.9294117647058824, 0.8431372549019608, 1.0, 1.0]}),
            outset: 40,
            pane: false,
        },
        Case {
            name: "spectral",
            source: include_str!("../../packages/text-effects/spectral.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.2980392156862745, 0.40784313725490196, 0.5803921568627451, 1.0], "bright": [0.6470588235294118, 0.8196078431372549, 0.8901960784313725, 1.0], "accent": [0.9411764705882353, 0.984313725490196, 1.0, 1.0]}),
            outset: 36,
            pane: false,
        },
        Case {
            name: "swarm",
            source: include_str!("../../packages/text-effects/swarm.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.5254901960784314, 0.38823529411764707, 0.12941176470588237, 1.0], "bright": [1.0, 0.8, 0.3333333333333333, 1.0], "accent": [0.9568627450980393, 0.9529411764705882, 0.8745098039215686, 1.0], "particles": 1}),
            outset: 40,
            pane: false,
        },
        Case {
            name: "thorns",
            source: include_str!("../../packages/text-effects/thorns.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.235,0.310,0.149,1.0], "bright": [0.514,0.702,0.302,1.0], "accent": [0.878,0.784,0.525,1.0]}),
            outset: 24,
            pane: false,
        },
        Case {
            name: "slash",
            source: include_str!("../../packages/text-effects/combat.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.13725490196078433, 0.3176470588235294, 0.5490196078431373, 1.0], "bright": [0.5411764705882353, 0.796078431372549, 1.0, 1.0], "accent": [1.0, 1.0, 1.0, 1.0], "variant": 0}),
            outset: 52,
            pane: false,
        },
        Case {
            name: "critical",
            source: include_str!("../../packages/text-effects/combat.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.6274509803921569, 0.16862745098039217, 0.10980392156862745, 1.0], "bright": [1.0, 0.7294117647058823, 0.3843137254901961, 1.0], "accent": [1.0, 0.9607843137254902, 0.8392156862745098, 1.0], "variant": 1}),
            outset: 88,
            pane: false,
        },
        Case {
            name: "parry",
            source: include_str!("../../packages/text-effects/combat.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.3843137254901961, 0.4235294117647059, 0.5333333333333333, 1.0], "bright": [0.7490196078431373, 0.8666666666666667, 0.9607843137254902, 1.0], "accent": [1.0, 0.9490196078431372, 0.792156862745098, 1.0], "variant": 2}),
            outset: 80,
            pane: false,
        },
        Case {
            name: "healWave",
            source: include_str!("../../packages/text-effects/grace.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.0784313725490196, 0.47058823529411764, 0.34509803921568627, 1.0], "bright": [0.43529411764705883, 0.9215686274509803, 0.7294117647058823, 1.0], "accent": [0.8941176470588236, 1.0, 0.9607843137254902, 1.0], "variant": 0}),
            outset: 40,
            pane: false,
        },
        Case {
            name: "blessing",
            source: include_str!("../../packages/text-effects/grace.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.596078431372549, 0.396078431372549, 0.10980392156862745, 1.0], "bright": [0.9450980392156862, 0.7764705882352941, 0.3803921568627451, 1.0], "accent": [1.0, 0.9450980392156862, 0.788235294117647, 1.0], "variant": 1}),
            outset: 44,
            pane: false,
        },
        Case {
            name: "teleport",
            source: include_str!("../../packages/text-effects/teleport.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.2823529411764706, 0.27450980392156865, 0.6705882352941176, 1.0], "bright": [0.5607843137254902, 0.8627450980392157, 0.9098039215686274, 1.0], "accent": [0.8901960784313725, 0.9098039215686274, 1.0, 1.0]}),
            outset: 64,
            pane: false,
        },
        Case {
            name: "cleave",
            source: include_str!("../../packages/text-effects/combat.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.615686274509804, 0.25098039215686274, 0.15294117647058825, 1.0], "bright": [1.0, 0.7294117647058823, 0.4470588235294118, 1.0], "accent": [1.0, 0.9607843137254902, 0.8666666666666667, 1.0], "variant": 3}),
            outset: 48,
            pane: false,
        },
        Case {
            name: "riposte",
            source: include_str!("../../packages/text-effects/combat.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.19215686274509805, 0.3215686274509804, 0.5294117647058824, 1.0], "bright": [0.6235294117647059, 0.8627450980392157, 0.9411764705882353, 1.0], "accent": [1.0, 1.0, 1.0, 1.0], "variant": 4}),
            outset: 42,
            pane: false,
        },
        Case {
            name: "dispel",
            source: include_str!("../../packages/text-effects/sigil.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.38823529411764707, 0.3137254901960784, 0.6196078431372549, 1.0], "bright": [0.6980392156862745, 0.6274509803921569, 0.9372549019607843, 1.0], "accent": [0.9647058823529412, 0.9333333333333333, 1.0, 1.0], "variant": 0}),
            outset: 44,
            pane: false,
        },
        Case {
            name: "divineShield",
            source: include_str!("../../packages/text-effects/sigil.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.611764705882353, 0.4470588235294118, 0.1411764705882353, 1.0], "bright": [0.9607843137254902, 0.8196078431372549, 0.4627450980392157, 1.0], "accent": [1.0, 0.9686274509803922, 0.8666666666666667, 1.0], "variant": 1}),
            outset: 18,
            pane: false,
        },
        Case {
            name: "fireflies",
            source: include_str!("../../packages/text-effects/fireflies.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.38823529411764707, 0.4823529411764706, 0.11372549019607843, 1.0], "bright": [0.8392156862745098, 0.9019607843137255, 0.4196078431372549, 1.0], "accent": [1.0, 1.0, 0.8470588235294118, 1.0], "particles": 1}),
            outset: 32,
            pane: false,
        },
        Case {
            name: "runicOrbit",
            source: include_str!("../../packages/text-effects/runic-orbit.wgsl"),
            params: serde_json::json!({"intensity": 1.0, "speed": 1.0, "base": [0.29411764705882354, 0.3176470588235294, 0.615686274509804, 1.0], "bright": [0.7058823529411765, 0.7098039215686275, 0.9568627450980393, 1.0], "accent": [0.9607843137254902, 0.9254901960784314, 1.0, 1.0]}),
            outset: 24,
            pane: false,
        },
    ]
}

impl Case {
    /// Shared rendering inputs; regression tests override playback to exercise retirement.
    pub fn decoration(&self) -> smudgy_session_model::inline_content::InlineDecoration {
        use smudgy_session_model::{
            inline_content::{InlineDecoration, InlineOwner, TextEffect},
            text_shader::{EffectScale, Shader, ShaderEffect},
        };
        use std::sync::Arc;
        let case = self;
        let shader = Shader::compile(case.name, case.source).unwrap();
        InlineDecoration::new(
            0..21,
            TextEffect {
                shader: Arc::new(ShaderEffect {
                    shader: shader.clone(),
                    uniforms: shader.uniforms(case.params.as_object().unwrap()).unwrap(),
                    scale: EffectScale::default(),
                    capture_scale: smudgy_session_model::text_shader::CaptureScale::new(
                        if kinetic_signal(case.name) {
                            4.0
                        } else if case.name == "emphasis" {
                            (case.params["peak"].as_f64().unwrap() as f32 + 1.0).min(8.0)
                        } else if matches!(case.name, "astralConjunction" | "phoenixRebirth") {
                            4.0
                        } else if matches!(case.name, "explode" | "implode" | "eventHorizon") {
                            3.0
                        } else if material_batch(case.name)
                            || shader.fragments_per_glyph > 0
                            || matches!(case.name, "reverseSmoke" | "voidBloom" | "silence")
                        {
                            2.0
                        } else {
                            1.0
                        },
                    )
                    .unwrap(),
                    pane: case.pane,
                    fade_in_ms: if attention_burst(case.name)
                        || anchored_intro(case.name)
                        || shader.fragments_per_glyph > 0
                        || matches!(
                            case.name,
                            "emphasis" | "liquefy" | "inkBloom" | "reverseSmoke"
                        ) {
                        0
                    } else {
                        90
                    },
                    fade_out_ms: if case.name == "rootbind" {
                        240
                    } else if attention_burst(case.name)
                        || anchored_intro(case.name)
                        || shader.fragments_per_glyph > 0
                        || matches!(
                            case.name,
                            "emphasis" | "liquefy" | "inkBloom" | "reverseSmoke"
                        )
                    {
                        0
                    } else {
                        240
                    },
                    replace: !kinetic_signal(case.name)
                        && (anchored_intro(case.name)
                            || shader.fragments_per_glyph > 0
                            || matches!(
                                case.name,
                                "liquefy"
                                    | "inkBloom"
                                    | "smoke"
                                    | "reverseSmoke"
                                    | "spectral"
                                    | "thorns"
                                    | "teleport"
                                    | "emphasis"
                                    | "tide"
                                    | "elastic"
                                    | "quake"
                                    | "explode"
                                    | "implode"
                            )),
                    hold: false,
                    animated: true,
                }),

                duration_ms: 2400,
                outset: case.outset,
            },
            InlineOwner::default(),
        )
    }
}
