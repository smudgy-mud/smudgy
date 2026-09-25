//! Persisted, portable automation definitions and the browser's two-scope plan.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationDefinition {
    #[serde(default)]
    pub aliases: Vec<PlaintextRule>,
    #[serde(default)]
    pub triggers: Vec<PlaintextRule>,
    /// User-authored JavaScript, evaluated once in the owning browser worker.
    /// Desktop hosts may ignore this field until they use the same JS policy.
    #[serde(default)]
    pub script: String,
}

/// The browser session's two automation scopes. Server-wide rules run in every
/// profile; the selected profile (including Default) adds only its own rules.
/// On input, profile aliases get first refusal. On output, shared triggers
/// run before profile triggers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationPlan {
    pub shared: AutomationDefinition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<AutomationDefinition>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaintextRule {
    pub pattern: String,
    pub command: String,
}
