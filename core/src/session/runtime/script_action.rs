use std::sync::Arc;

use crate::session::runtime::script_engine::{FunctionId, ScriptId};

#[derive(Clone, Debug)]
pub enum ScriptAction {
    Noop,
    SendRaw(Arc<String>),
    SendSimple(Arc<String>),
    EvalJavascript(ScriptId),
    CallJavascriptFunction(FunctionId),
}
