//! Runtime-neutral automation contracts and the no-script implementation.
//!
//! The desktop Deno host and a browser JavaScript host can implement
//! [`AutomationHost`]. The initial browser client uses [`PlaintextAutomation`]
//! directly, so no scripting engine is linked into the portable client.

use regex::Regex;
use smudgy_session_model::automation::AutomationDefinition;
use smudgy_session_model::pane::{PaneDef, PaneKey, PanePlacement};

/// A host-visible mutation of the portable pane registry.
#[derive(Debug, Clone, PartialEq)]
pub enum PaneEffect {
    Opened {
        def: PaneDef,
        placement: PanePlacement,
    },
    Updated(PaneDef),
    Echo {
        key: PaneKey,
        text: String,
    },
    Clear(PaneKey),
    Closed(PaneKey),
}

/// An action produced by an automation host for the session engine to apply.
#[derive(Debug, Clone, PartialEq)]
pub enum AutomationEffect {
    /// Send one command to the MUD. The engine adds the Telnet line ending.
    Send(String),
    /// Add a local informational line to the terminal.
    Display(String),
    /// Mutate a script pane while preserving callback order.
    Pane(PaneEffect),
}

/// Result of presenting an input stage to an automation host.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InputOutcome {
    /// Whether the stage consumed the original command.
    pub handled: bool,
    /// Ordered actions produced while handling the command.
    pub effects: Vec<AutomationEffect>,
}

/// The single computation capability required by the portable session engine.
///
/// This is deliberately synchronous. A browser JavaScript host executes in
/// its session's dedicated worker, preserving Smudgy's run-to-completion event
/// ordering without blocking the window or another session.
pub trait AutomationHost {
    /// Effects emitted while constructing the synchronous script scope.
    fn on_start(&mut self) -> Vec<AutomationEffect> {
        Vec::new()
    }

    /// Intercept a typed, unmasked submission before alias matching. Profile
    /// startup commands and masked submissions never enter this stage.
    fn on_submitted_input(&mut self, _input: &str) -> InputOutcome {
        InputOutcome::default()
    }

    /// Match aliases on an outgoing command after submission interception.
    fn on_user_input(&mut self, input: &str) -> InputOutcome;

    fn on_output_line(&mut self, line: &str) -> Vec<AutomationEffect>;
}

/// Compile-time-selected automation host that performs no automation.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAutomation;

impl AutomationHost for NoAutomation {
    fn on_user_input(&mut self, _input: &str) -> InputOutcome {
        InputOutcome::default()
    }

    fn on_output_line(&mut self, _line: &str) -> Vec<AutomationEffect> {
        Vec::new()
    }
}

/// A regex alias whose replacement uses Rust regex captures (`$1`, `$name`).
#[derive(Debug, Clone)]
pub struct Alias {
    pattern: Regex,
    replacement: String,
}

impl Alias {
    /// Compile an alias pattern and capture-expanding replacement.
    ///
    /// # Errors
    ///
    /// Returns the regex compiler error when `pattern` is invalid.
    pub fn new(pattern: &str, replacement: impl Into<String>) -> Result<Self, regex::Error> {
        Ok(Self {
            pattern: Regex::new(pattern)?,
            replacement: replacement.into(),
        })
    }
}

/// A regex trigger that sends a plaintext command using capture replacement.
#[derive(Debug, Clone)]
pub struct Trigger {
    pattern: Regex,
    response: String,
}

impl Trigger {
    /// Compile a trigger pattern and capture-expanding response.
    ///
    /// # Errors
    ///
    /// Returns the regex compiler error when `pattern` is invalid.
    pub fn new(pattern: &str, response: impl Into<String>) -> Result<Self, regex::Error> {
        Ok(Self {
            pattern: Regex::new(pattern)?,
            response: response.into(),
        })
    }
}

/// Plaintext aliases and triggers with deterministic, insertion-order output.
#[derive(Debug, Default)]
pub struct PlaintextAutomation {
    aliases: Vec<Alias>,
    triggers: Vec<Trigger>,
}

impl PlaintextAutomation {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            aliases: Vec::new(),
            triggers: Vec::new(),
        }
    }

    pub fn add_alias(&mut self, alias: Alias) {
        self.aliases.push(alias);
    }

    pub fn add_trigger(&mut self, trigger: Trigger) {
        self.triggers.push(trigger);
    }

    /// Compile a saved profile's plaintext rules in their declared order.
    ///
    /// # Errors
    /// Returns the rule index and regex error for the first invalid pattern.
    pub fn from_definition(definition: &AutomationDefinition) -> Result<Self, String> {
        let mut host = Self::new();
        for (index, rule) in definition.aliases.iter().enumerate() {
            host.add_alias(
                Alias::new(&rule.pattern, &rule.command)
                    .map_err(|error| format!("Alias {}: {error}", index + 1))?,
            );
        }
        for (index, rule) in definition.triggers.iter().enumerate() {
            host.add_trigger(
                Trigger::new(&rule.pattern, &rule.command)
                    .map_err(|error| format!("Trigger {}: {error}", index + 1))?,
            );
        }
        Ok(host)
    }
}

impl AutomationHost for PlaintextAutomation {
    fn on_user_input(&mut self, input: &str) -> InputOutcome {
        let Some(alias) = self
            .aliases
            .iter()
            .find(|alias| alias.pattern.is_match(input))
        else {
            return InputOutcome::default();
        };

        InputOutcome {
            handled: true,
            effects: vec![AutomationEffect::Send(expand(
                &alias.pattern,
                &alias.replacement,
                input,
            ))],
        }
    }

    fn on_output_line(&mut self, line: &str) -> Vec<AutomationEffect> {
        self.triggers
            .iter()
            .filter(|trigger| trigger.pattern.is_match(line))
            .map(|trigger| {
                AutomationEffect::Send(expand(&trigger.pattern, &trigger.response, line))
            })
            .collect()
    }
}

fn expand(pattern: &Regex, template: &str, input: &str) -> String {
    let mut expanded = String::new();
    pattern
        .captures(input)
        .expect("caller checked for a match")
        .expand(template, &mut expanded);
    expanded
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_session_model::automation::PlaintextRule;

    #[test]
    fn aliases_consume_input_and_expand_captures() {
        let mut host = PlaintextAutomation::new();
        host.add_alias(Alias::new(r"^k (.+)$", "kill $1").unwrap());

        assert_eq!(
            host.on_user_input("k goblin"),
            InputOutcome {
                handled: true,
                effects: vec![AutomationEffect::Send("kill goblin".into())],
            }
        );
        assert_eq!(host.on_user_input("look"), InputOutcome::default());
    }

    #[test]
    fn triggers_preserve_definition_order() {
        let mut host = PlaintextAutomation::new();
        host.add_trigger(Trigger::new(r"^HP: (\d+)$", "say hp=$1").unwrap());
        host.add_trigger(Trigger::new(r"^HP:", "score").unwrap());

        assert_eq!(
            host.on_output_line("HP: 42"),
            vec![
                AutomationEffect::Send("say hp=42".into()),
                AutomationEffect::Send("score".into()),
            ]
        );
    }

    #[test]
    fn saved_profile_rules_compile_without_script_runtime() {
        let definition = AutomationDefinition {
            aliases: vec![PlaintextRule {
                pattern: "^k (.+)$".into(),
                command: "kill $1".into(),
            }],
            triggers: vec![PlaintextRule {
                pattern: "^HP: (.+)$".into(),
                command: "say $1".into(),
            }],
            script: "function onLine() {}".into(),
        };
        let mut automation = PlaintextAutomation::from_definition(&definition).unwrap();
        assert_eq!(
            automation.on_user_input("k rat").effects,
            vec![AutomationEffect::Send("kill rat".into())]
        );
        assert_eq!(
            automation.on_output_line("HP: 42"),
            vec![AutomationEffect::Send("say 42".into())]
        );
        assert!(
            PlaintextAutomation::from_definition(&AutomationDefinition {
                aliases: vec![PlaintextRule {
                    pattern: "[".into(),
                    command: "x".into()
                }],
                ..AutomationDefinition::default()
            })
            .unwrap_err()
            .starts_with("Alias 1:")
        );
    }
}
