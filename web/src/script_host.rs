//! Synchronous, per-session JavaScript automation in a dedicated web worker.
//!
//! Only user-authored profile source and explicitly installed local packages
//! are evaluated. MUD output is passed as data, never compiled. Browser workers isolate sessions from the UI and
//! from one another, but this is not a security sandbox for hostile scripts.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::{Function, Object, Reflect};
use smudgy_engine::{
    AutomationEffect, AutomationHost, InputOutcome, PaneEffect, PlaintextAutomation,
};
use smudgy_session_model::automation::AutomationPlan;
use smudgy_session_model::pane::{
    DefStateSpec, PaneError, PaneKind, PaneNamespace, PanePlacement, PaneRegistry, SplitDirection,
    TabPosition,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};

pub struct WorkerAutomation {
    shared: PlaintextAutomation,
    profile: Option<PlaintextAutomation>,
    script: Option<Script>,
}

struct Script {
    api: Object,
    on_line: Option<Function>,
    on_input: Option<Function>,
    effects: Rc<RefCell<Vec<AutomationEffect>>>,
    active: Rc<Cell<bool>>,
    pane_active: Rc<Cell<bool>>,
    startup_effects: Vec<AutomationEffect>,
    _send: Closure<dyn FnMut(JsValue)>,
    _print: Closure<dyn FnMut(JsValue)>,
    _pane_split: Closure<dyn FnMut(JsValue, JsValue, JsValue) -> JsValue>,
    _pane_add_tab: Closure<dyn FnMut(JsValue, JsValue) -> JsValue>,
    _pane_echo: Closure<dyn FnMut(JsValue, JsValue) -> JsValue>,
    _pane_clear: Closure<dyn FnMut(JsValue) -> JsValue>,
    _pane_close: Closure<dyn FnMut(JsValue) -> JsValue>,
    _pane_hidden: Closure<dyn FnMut(JsValue) -> JsValue>,
    _pane_set_hidden: Closure<dyn FnMut(JsValue, JsValue) -> JsValue>,
}

impl WorkerAutomation {
    pub fn new(plan: &AutomationPlan, factory: Option<&Function>) -> Result<Self, String> {
        let shared = PlaintextAutomation::from_definition(&plan.shared)?;
        let profile = plan
            .profile
            .as_ref()
            .map(PlaintextAutomation::from_definition)
            .transpose()?;
        let script = match factory {
            Some(factory) => Some(Script::new(factory)?),
            None if plan.shared.script.trim().is_empty()
                && plan
                    .profile
                    .as_ref()
                    .is_none_or(|profile| profile.script.trim().is_empty()) =>
            {
                None
            }
            None => return Err("session script module was not loaded".to_owned()),
        };
        Ok(Self {
            shared,
            profile,
            script,
        })
    }
}

impl AutomationHost for WorkerAutomation {
    fn on_start(&mut self) -> Vec<AutomationEffect> {
        self.script.as_mut().map_or_else(Vec::new, |script| {
            std::mem::take(&mut script.startup_effects)
        })
    }

    fn on_submitted_input(&mut self, input: &str) -> InputOutcome {
        let Some(script) = &self.script else {
            return InputOutcome::default();
        };
        let (handled, effects) = script.run_input(input);
        InputOutcome { handled, effects }
    }

    fn on_user_input(&mut self, input: &str) -> InputOutcome {
        // A profile can override a server-wide alias without mutating the
        // shared definition. Typed-submission scripts already ran before this
        // stage; startup commands reach aliases but not those scripts.
        let mut outcome = self
            .profile
            .as_mut()
            .map_or_else(InputOutcome::default, |profile| {
                profile.on_user_input(input)
            });
        if !outcome.handled {
            outcome = self.shared.on_user_input(input);
        }
        outcome
    }

    fn on_output_line(&mut self, line: &str) -> Vec<AutomationEffect> {
        let mut effects = self.shared.on_output_line(line);
        if let Some(profile) = &mut self.profile {
            effects.extend(profile.on_output_line(line));
        }
        if let Some(script) = &self.script {
            effects.extend(script.run_line(line));
        }
        effects
    }
}

impl Script {
    fn new(factory: &Function) -> Result<Self, String> {
        let effects = Rc::new(RefCell::new(Vec::new()));
        let active = Rc::new(Cell::new(false));
        let pane_active = Rc::new(Cell::new(true));
        let pane_registry = Rc::new(RefCell::new(PaneRegistry::new()));
        let outbound = Rc::clone(&effects);
        let sending = Rc::clone(&active);
        let send = Closure::<dyn FnMut(JsValue)>::new(move |value: JsValue| {
            if sending.get()
                && let Some(command) = value.as_string()
            {
                outbound.borrow_mut().push(AutomationEffect::Send(command));
            }
        });
        let printed = Rc::clone(&effects);
        let printing = Rc::clone(&active);
        let print = Closure::<dyn FnMut(JsValue)>::new(move |value: JsValue| {
            if printing.get()
                && let Some(line) = value.as_string()
            {
                printed.borrow_mut().push(AutomationEffect::Display(line));
            }
        });
        let api = Object::new();
        Reflect::set(&api, &JsValue::from_str("send"), send.as_ref())
            .map_err(|error| format!("script API setup failed: {error:?}"))?;
        Reflect::set(&api, &JsValue::from_str("print"), print.as_ref())
            .map_err(|error| format!("script API setup failed: {error:?}"))?;

        let pane_split = pane_split_closure(
            Rc::clone(&pane_registry),
            Rc::clone(&effects),
            Rc::clone(&pane_active),
        );
        let pane_add_tab = pane_add_tab_closure(
            Rc::clone(&pane_registry),
            Rc::clone(&effects),
            Rc::clone(&pane_active),
        );
        let pane_echo = pane_echo_closure(
            Rc::clone(&pane_registry),
            Rc::clone(&effects),
            Rc::clone(&pane_active),
        );
        let pane_clear = pane_clear_closure(
            Rc::clone(&pane_registry),
            Rc::clone(&effects),
            Rc::clone(&pane_active),
        );
        let pane_close = pane_close_closure(
            Rc::clone(&pane_registry),
            Rc::clone(&effects),
            Rc::clone(&pane_active),
        );
        let pane_hidden = pane_hidden_closure(Rc::clone(&pane_registry));
        let pane_set_hidden =
            pane_set_hidden_closure(pane_registry, Rc::clone(&effects), Rc::clone(&pane_active));
        for (name, function) in [
            ("__paneSplit", pane_split.as_ref()),
            ("__paneAddTab", pane_add_tab.as_ref()),
            ("__paneEcho", pane_echo.as_ref()),
            ("__paneClear", pane_clear.as_ref()),
            ("__paneClose", pane_close.as_ref()),
            ("__paneHidden", pane_hidden.as_ref()),
            ("__paneSetHidden", pane_set_hidden.as_ref()),
        ] {
            Reflect::set(&api, &JsValue::from_str(name), function)
                .map_err(|error| format!("script pane API setup failed: {error:?}"))?;
        }

        let handlers = factory
            .call1(&JsValue::UNDEFINED, &api)
            .map_err(|error| format!("session script startup failed: {error:?}"))?;
        pane_active.set(false);
        let on_line = handler(&handlers, "onLine")?;
        let on_input = handler(&handlers, "onInput")?;
        let startup_effects = effects.borrow_mut().drain(..).collect();
        Ok(Self {
            api,
            on_line,
            on_input,
            effects,
            active,
            pane_active,
            startup_effects,
            _send: send,
            _print: print,
            _pane_split: pane_split,
            _pane_add_tab: pane_add_tab,
            _pane_echo: pane_echo,
            _pane_clear: pane_clear,
            _pane_close: pane_close,
            _pane_hidden: pane_hidden,
            _pane_set_hidden: pane_set_hidden,
        })
    }

    fn run_line(&self, line: &str) -> Vec<AutomationEffect> {
        self.effects.borrow_mut().clear();
        if let Some(handler) = &self.on_line {
            self.active.set(true);
            self.pane_active.set(true);
            let result = handler.call2(&JsValue::UNDEFINED, &JsValue::from_str(line), &self.api);
            self.active.set(false);
            self.pane_active.set(false);
            match result {
                Ok(value) if value.is_instance_of::<js_sys::Promise>() => {
                    self.effects.borrow_mut().push(AutomationEffect::Display(
                        "Session script onLine returned a Promise; handlers must be synchronous"
                            .into(),
                    ));
                }
                Err(error) => self
                    .effects
                    .borrow_mut()
                    .push(AutomationEffect::Display(format!(
                        "Session script onLine error: {error:?}"
                    ))),
                _ => {}
            }
        }
        self.effects.borrow_mut().drain(..).collect()
    }

    fn run_input(&self, input: &str) -> (bool, Vec<AutomationEffect>) {
        self.effects.borrow_mut().clear();
        let mut handled = false;
        if let Some(handler) = &self.on_input {
            self.active.set(true);
            self.pane_active.set(true);
            let result = handler.call2(&JsValue::UNDEFINED, &JsValue::from_str(input), &self.api);
            self.active.set(false);
            self.pane_active.set(false);
            match result {
                Ok(value) if value.is_instance_of::<js_sys::Promise>() => {
                    self.effects.borrow_mut().push(AutomationEffect::Display(
                        "Session script onInput returned a Promise; handlers must be synchronous"
                            .into(),
                    ));
                }
                Ok(value) => handled = value.as_bool() == Some(true),
                Err(error) => {
                    self.effects
                        .borrow_mut()
                        .push(AutomationEffect::Display(format!(
                            "Session script onInput error: {error:?}"
                        )));
                }
            }
        }
        (handled, self.effects.borrow_mut().drain(..).collect())
    }
}

type Effects = Rc<RefCell<Vec<AutomationEffect>>>;
type Registry = Rc<RefCell<PaneRegistry>>;

fn pane_error(error: impl std::fmt::Display) -> JsValue {
    let result = Object::new();
    let _ = Reflect::set(
        &result,
        &JsValue::from_str("error"),
        &JsValue::from_str(&error.to_string()),
    );
    result.into()
}

fn pane_result(name: &str, created: bool) -> JsValue {
    let result = Object::new();
    let _ = Reflect::set(
        &result,
        &JsValue::from_str("name"),
        &JsValue::from_str(name),
    );
    let _ = Reflect::set(
        &result,
        &JsValue::from_str("created"),
        &JsValue::from_bool(created),
    );
    result.into()
}

fn property(value: &JsValue, name: &str) -> Option<JsValue> {
    Reflect::get(value, &JsValue::from_str(name))
        .ok()
        .filter(|value| !value.is_undefined())
}

fn finite_f32(value: f64, field: &str) -> Result<f32, String> {
    if !value.is_finite() || !(f64::from(f32::MIN)..=f64::from(f32::MAX)).contains(&value) {
        return Err(format!("{field} must be a finite 32-bit number"));
    }
    // The range check above makes this the intended narrowing conversion at
    // the JavaScript/WASM boundary.
    #[allow(clippy::cast_possible_truncation)]
    Ok(value as f32)
}

fn pane_spec(value: &JsValue) -> Result<(String, PaneKind, DefStateSpec), String> {
    let name = property(value, "name")
        .and_then(|value| value.as_string())
        .ok_or_else(|| "pane spec requires a name".to_owned())?;
    let kind = match property(value, "terminal").and_then(|value| value.as_bool()) {
        Some(false) => {
            return Err("widgets-only panes are not available in the browser build".to_owned());
        }
        _ => PaneKind::Terminal,
    };
    let title_bar = property(value, "titleBar")
        .and_then(|value| value.as_string())
        .map(|value| {
            smudgy_session_model::pane::TitleBarPolicy::parse(&value)
                .ok_or_else(|| format!("invalid titleBar '{value}'"))
        })
        .transpose()?;
    let hidden = property(value, "hidden").and_then(|value| value.as_bool());
    let font_size = property(value, "fontSize")
        .and_then(|value| value.as_f64())
        .map(|value| finite_f32(value, "pane fontSize"))
        .transpose()?;
    Ok((
        name,
        kind,
        DefStateSpec {
            title_bar,
            hidden,
            font_size,
        },
    ))
}

fn pane_split_closure(
    registry: Registry,
    effects: Effects,
    active: Rc<Cell<bool>>,
) -> Closure<dyn FnMut(JsValue, JsValue, JsValue) -> JsValue> {
    Closure::new(
        move |reference: JsValue, direction: JsValue, spec: JsValue| {
            if !active.get() {
                return pane_error(
                    "pane mutations are only available during synchronous script work",
                );
            }
            let Some(reference) = reference.as_string() else {
                return pane_error("pane split requires a reference name");
            };
            let Some(direction) = direction
                .as_string()
                .and_then(|value| SplitDirection::parse(&value))
            else {
                return pane_error("invalid pane split direction");
            };
            let (name, kind, def_state) = match pane_spec(&spec) {
                Ok(spec) => spec,
                Err(error) => return pane_error(error),
            };
            let size_px = match direction {
                SplitDirection::Left | SplitDirection::Right => property(&spec, "width"),
                SplitDirection::Top | SplitDirection::Bottom => property(&spec, "height"),
            }
            .and_then(|value| value.as_f64())
            .map(|value| finite_f32(value, "pane split size"))
            .transpose();
            let size_px = match size_px {
                Ok(size) => size,
                Err(error) => return pane_error(error),
            };
            if size_px.is_some_and(|size| !size.is_finite() || size <= 0.0) {
                return pane_error("pane split size must be a positive finite number");
            }

            let mut registry = registry.borrow_mut();
            let Some(reference) = registry
                .resolve(&PaneNamespace::User, &reference)
                .map(|def| def.key)
            else {
                return pane_error("pane split reference does not exist");
            };
            let outcome = match registry.split(&PaneNamespace::User, &name, kind, def_state, None) {
                Ok(outcome) => outcome,
                Err(error) => return pane_error(error),
            };
            if outcome.created {
                effects
                    .borrow_mut()
                    .push(AutomationEffect::Pane(PaneEffect::Opened {
                        def: outcome.def.clone(),
                        placement: PanePlacement::Split {
                            reference,
                            direction,
                            size_px,
                        },
                    }));
            } else if outcome.def_changed {
                effects
                    .borrow_mut()
                    .push(AutomationEffect::Pane(PaneEffect::Updated(
                        outcome.def.clone(),
                    )));
            }
            pane_result(&outcome.def.name, outcome.created)
        },
    )
}

fn pane_add_tab_closure(
    registry: Registry,
    effects: Effects,
    active: Rc<Cell<bool>>,
) -> Closure<dyn FnMut(JsValue, JsValue) -> JsValue> {
    Closure::new(move |reference: JsValue, spec: JsValue| {
        if !active.get() {
            return pane_error("pane mutations are only available during synchronous script work");
        }
        let Some(reference) = reference.as_string() else {
            return pane_error("pane addTab requires a reference name");
        };
        let (name, kind, def_state) = match pane_spec(&spec) {
            Ok(spec) => spec,
            Err(error) => return pane_error(error),
        };
        let selected = property(&spec, "selected")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let mut registry = registry.borrow_mut();
        let Some(reference) = registry
            .resolve(&PaneNamespace::User, &reference)
            .map(|def| def.key)
        else {
            return pane_error("pane tab reference does not exist");
        };
        let outcome = match registry.split(&PaneNamespace::User, &name, kind, def_state, None) {
            Ok(outcome) => outcome,
            Err(error) => return pane_error(error),
        };
        if outcome.created {
            effects
                .borrow_mut()
                .push(AutomationEffect::Pane(PaneEffect::Opened {
                    def: outcome.def.clone(),
                    placement: PanePlacement::Tab {
                        reference,
                        position: TabPosition::After,
                        selected,
                    },
                }));
        } else if outcome.def_changed {
            effects
                .borrow_mut()
                .push(AutomationEffect::Pane(PaneEffect::Updated(
                    outcome.def.clone(),
                )));
        }
        pane_result(&outcome.def.name, outcome.created)
    })
}

fn pane_echo_closure(
    registry: Registry,
    effects: Effects,
    active: Rc<Cell<bool>>,
) -> Closure<dyn FnMut(JsValue, JsValue) -> JsValue> {
    Closure::new(move |name: JsValue, text: JsValue| {
        if !active.get() {
            return pane_error("pane mutations are only available during synchronous script work");
        }
        let (Some(name), Some(text)) = (name.as_string(), text.as_string()) else {
            return pane_error("pane echo requires a pane name and text");
        };
        let registry = registry.borrow();
        let Some(def) = registry.resolve(&PaneNamespace::User, &name) else {
            return pane_error(PaneError::NoSuchPane(name));
        };
        if def.kind != PaneKind::Terminal {
            return pane_error("cannot echo into a widgets-only pane");
        }
        effects
            .borrow_mut()
            .push(AutomationEffect::Pane(PaneEffect::Echo {
                key: def.key,
                text,
            }));
        JsValue::TRUE
    })
}

fn pane_clear_closure(
    registry: Registry,
    effects: Effects,
    active: Rc<Cell<bool>>,
) -> Closure<dyn FnMut(JsValue) -> JsValue> {
    Closure::new(move |name: JsValue| {
        if !active.get() {
            return pane_error("pane mutations are only available during synchronous script work");
        }
        let Some(name) = name.as_string() else {
            return pane_error("pane clear requires a pane name");
        };
        let registry = registry.borrow();
        let Some(def) = registry.resolve(&PaneNamespace::User, &name) else {
            return pane_error(PaneError::NoSuchPane(name));
        };
        if def.kind != PaneKind::Terminal {
            return pane_error("cannot clear a widgets-only pane");
        }
        effects
            .borrow_mut()
            .push(AutomationEffect::Pane(PaneEffect::Clear(def.key)));
        JsValue::TRUE
    })
}

fn pane_close_closure(
    registry: Registry,
    effects: Effects,
    active: Rc<Cell<bool>>,
) -> Closure<dyn FnMut(JsValue) -> JsValue> {
    Closure::new(move |name: JsValue| {
        if !active.get() {
            return pane_error("pane mutations are only available during synchronous script work");
        }
        let Some(name) = name.as_string() else {
            return pane_error("pane close requires a pane name");
        };
        let mut registry = registry.borrow_mut();
        if registry.resolve(&PaneNamespace::User, &name).is_none() {
            return JsValue::TRUE;
        }
        match registry.close(&PaneNamespace::User, &name) {
            Ok(key) => {
                effects
                    .borrow_mut()
                    .push(AutomationEffect::Pane(PaneEffect::Closed(key)));
                JsValue::TRUE
            }
            Err(error) => pane_error(error),
        }
    })
}

fn pane_hidden_closure(registry: Registry) -> Closure<dyn FnMut(JsValue) -> JsValue> {
    Closure::new(move |name: JsValue| {
        let Some(name) = name.as_string() else {
            return JsValue::FALSE;
        };
        JsValue::from_bool(
            registry
                .borrow()
                .resolve(&PaneNamespace::User, &name)
                .is_some_and(|def| def.hidden),
        )
    })
}

fn pane_set_hidden_closure(
    registry: Registry,
    effects: Effects,
    active: Rc<Cell<bool>>,
) -> Closure<dyn FnMut(JsValue, JsValue) -> JsValue> {
    Closure::new(move |name: JsValue, hidden: JsValue| {
        if !active.get() {
            return pane_error("pane mutations are only available during synchronous script work");
        }
        let (Some(name), Some(hidden)) = (name.as_string(), hidden.as_bool()) else {
            return pane_error("pane visibility requires a pane name and boolean");
        };
        match registry
            .borrow_mut()
            .set_hidden(&PaneNamespace::User, &name, hidden)
        {
            Ok(Some(def)) => {
                effects
                    .borrow_mut()
                    .push(AutomationEffect::Pane(PaneEffect::Updated(def)));
                JsValue::TRUE
            }
            Ok(None) => JsValue::TRUE,
            Err(error) => pane_error(error),
        }
    })
}

fn handler(handlers: &JsValue, name: &str) -> Result<Option<Function>, String> {
    let value = Reflect::get(handlers, &JsValue::from_str(name))
        .map_err(|error| format!("could not load script handler {name}: {error:?}"))?;
    Ok(value.dyn_into::<Function>().ok())
}
