use iced::keyboard::{Key, Modifiers, key, key::Named};
use smudgy_core::models::hotkeys::HotkeyDefinition;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MaybePhysicalKey {
    Key(iced::keyboard::Key),
    Physical(iced::keyboard::key::Physical),
}

/// Efficient storage for hotkey data with pre-converted iced types
#[derive(Debug, Clone)]
pub struct HotkeyKeys {
    pub main_key: MaybePhysicalKey,
    pub modifiers: iced::keyboard::Modifiers,
}

impl From<HotkeyDefinition> for HotkeyKeys {
    fn from(hotkey: HotkeyDefinition) -> Self {
        let main_key = hotkey_to_maybe_physical_key(&hotkey);
        let modifiers = hotkey_to_iced_modifiers(&hotkey);

        HotkeyKeys {
            main_key,
            modifiers,
        }
    }
}

/// Converts a vector of MaybePhysicalKeys to strings and updates the hotkey definition
pub fn set_key_and_modifiers_from_maybe_physical(
    hotkey: &mut HotkeyDefinition,
    keys: Vec<MaybePhysicalKey>,
) {
    let mut key_out: Vec<String> = Vec::new();
    let mut modifiers: Vec<String> = Vec::new();

    for maybe_key in keys {
        match maybe_key {
            MaybePhysicalKey::Key(key) => match key {
                Key::Named(Named::Control) => {
                    modifiers.push("CTRL".to_string());
                }
                Key::Named(Named::Alt) => {
                    modifiers.push("ALT".to_string());
                }
                Key::Named(Named::Shift) => {
                    modifiers.push("SHIFT".to_string());
                }
                Key::Named(Named::Super) => {
                    modifiers.push("SUPER".to_string());
                }
                Key::Named(name) => {
                    key_out.push(format!("{:?}", name));
                }
                Key::Character(c) => {
                    key_out.push(format!("Character({})", c));
                }
                Key::Unidentified => {
                    key_out.push("UNIDENTIFIED".to_string());
                }
            },
            MaybePhysicalKey::Physical(physical) => match physical {
                key::Physical::Code(code) => {
                    key_out.push(format!("Code({:?})", code));
                }
                key::Physical::Unidentified(id) => {
                    key_out.push(format!("PhysicalUnidentified({:?})", id));
                }
            },
        }
    }

    hotkey.key = key_out
        .first()
        .unwrap_or(&"UNIDENTIFIED".to_string())
        .clone();
    hotkey.modifiers = modifiers;
}

/// Converts a vector of iced Keys to strings and updates the hotkey definition
// Public counterpart to `set_key_and_modifiers_from_maybe_physical`, kept for
// the logical-key capture path and exercised by the unit tests below.
#[allow(dead_code)]
pub fn set_key_and_modifiers_from_iced(hotkey: &mut HotkeyDefinition, keys: Vec<Key>) {
    let mut key_out: Vec<String> = Vec::new();
    let mut modifiers: Vec<String> = Vec::new();

    for key in keys {
        match key {
            Key::Named(Named::Control) => {
                modifiers.push("CTRL".to_string());
            }
            Key::Named(Named::Alt) => {
                modifiers.push("ALT".to_string());
            }
            Key::Named(Named::Shift) => {
                modifiers.push("SHIFT".to_string());
            }
            Key::Named(Named::Super) => {
                modifiers.push("SUPER".to_string());
            }

            Key::Named(name) => {
                key_out.push(format!("{:?}", name));
            }
            Key::Character(c) => {
                key_out.push(c.to_string());
            }
            Key::Unidentified => {
                key_out.push("UNIDENTIFIED".to_string());
            }
        }
    }

    hotkey.key = key_out
        .first()
        .unwrap_or(&"UNIDENTIFIED".to_string())
        .clone();
    hotkey.modifiers = modifiers;
}

/// Converts a hotkey definition to a MaybePhysicalKey (the primary key, not modifiers)
pub fn hotkey_to_maybe_physical_key(hotkey: &HotkeyDefinition) -> MaybePhysicalKey {
    // Check for physical key codes first
    if hotkey.key.starts_with("Code(")
        && let Some(code_str) = hotkey.key.get(5..hotkey.key.len() - 1)
        && let Some(code) = physical_code_from_str(code_str)
    {
        return MaybePhysicalKey::Physical(key::Physical::Code(code));
    }

    // Check for named logical keys
    if let Key::Named(named) = named_key_from_str(&hotkey.key) {
        return MaybePhysicalKey::Key(Key::Named(named));
    }

    // Check for character keys
    if hotkey.key.starts_with("Character(")
        && let Some(c) = hotkey.key.get(10..hotkey.key.len() - 1)
    {
        return MaybePhysicalKey::Key(Key::Character(c.into()));
    }

    // Fallback to unidentified
    MaybePhysicalKey::Key(Key::Unidentified)
}

/// Converts a hotkey definition to iced Modifiers
pub fn hotkey_to_iced_modifiers(hotkey: &HotkeyDefinition) -> Modifiers {
    hotkey
        .modifiers
        .iter()
        .fold(Modifiers::empty(), |m, item| match item.as_str() {
            "CTRL" => m.union(Modifiers::CTRL),
            "ALT" => m.union(Modifiers::ALT),
            "SHIFT" => m.union(Modifiers::SHIFT),
            "SUPER" => m.union(Modifiers::LOGO),
            _ => m,
        })
}

/// Converts a string produced by `format!("{:?}", code)` back to an iced physical `key::Code`.
///
/// Generated from the `key::Code` enum in iced_core 0.14; regenerate when upgrading iced.
/// Each arm references the variant by name, so a removed or renamed variant fails to compile.
fn physical_code_from_str(s: &str) -> Option<key::Code> {
    match s {
        "Backquote" => Some(key::Code::Backquote),
        "Backslash" => Some(key::Code::Backslash),
        "BracketLeft" => Some(key::Code::BracketLeft),
        "BracketRight" => Some(key::Code::BracketRight),
        "Comma" => Some(key::Code::Comma),
        "Digit0" => Some(key::Code::Digit0),
        "Digit1" => Some(key::Code::Digit1),
        "Digit2" => Some(key::Code::Digit2),
        "Digit3" => Some(key::Code::Digit3),
        "Digit4" => Some(key::Code::Digit4),
        "Digit5" => Some(key::Code::Digit5),
        "Digit6" => Some(key::Code::Digit6),
        "Digit7" => Some(key::Code::Digit7),
        "Digit8" => Some(key::Code::Digit8),
        "Digit9" => Some(key::Code::Digit9),
        "Equal" => Some(key::Code::Equal),
        "IntlBackslash" => Some(key::Code::IntlBackslash),
        "IntlRo" => Some(key::Code::IntlRo),
        "IntlYen" => Some(key::Code::IntlYen),
        "KeyA" => Some(key::Code::KeyA),
        "KeyB" => Some(key::Code::KeyB),
        "KeyC" => Some(key::Code::KeyC),
        "KeyD" => Some(key::Code::KeyD),
        "KeyE" => Some(key::Code::KeyE),
        "KeyF" => Some(key::Code::KeyF),
        "KeyG" => Some(key::Code::KeyG),
        "KeyH" => Some(key::Code::KeyH),
        "KeyI" => Some(key::Code::KeyI),
        "KeyJ" => Some(key::Code::KeyJ),
        "KeyK" => Some(key::Code::KeyK),
        "KeyL" => Some(key::Code::KeyL),
        "KeyM" => Some(key::Code::KeyM),
        "KeyN" => Some(key::Code::KeyN),
        "KeyO" => Some(key::Code::KeyO),
        "KeyP" => Some(key::Code::KeyP),
        "KeyQ" => Some(key::Code::KeyQ),
        "KeyR" => Some(key::Code::KeyR),
        "KeyS" => Some(key::Code::KeyS),
        "KeyT" => Some(key::Code::KeyT),
        "KeyU" => Some(key::Code::KeyU),
        "KeyV" => Some(key::Code::KeyV),
        "KeyW" => Some(key::Code::KeyW),
        "KeyX" => Some(key::Code::KeyX),
        "KeyY" => Some(key::Code::KeyY),
        "KeyZ" => Some(key::Code::KeyZ),
        "Minus" => Some(key::Code::Minus),
        "Period" => Some(key::Code::Period),
        "Quote" => Some(key::Code::Quote),
        "Semicolon" => Some(key::Code::Semicolon),
        "Slash" => Some(key::Code::Slash),
        "AltLeft" => Some(key::Code::AltLeft),
        "AltRight" => Some(key::Code::AltRight),
        "Backspace" => Some(key::Code::Backspace),
        "CapsLock" => Some(key::Code::CapsLock),
        "ContextMenu" => Some(key::Code::ContextMenu),
        "ControlLeft" => Some(key::Code::ControlLeft),
        "ControlRight" => Some(key::Code::ControlRight),
        "Enter" => Some(key::Code::Enter),
        "SuperLeft" => Some(key::Code::SuperLeft),
        "SuperRight" => Some(key::Code::SuperRight),
        "ShiftLeft" => Some(key::Code::ShiftLeft),
        "ShiftRight" => Some(key::Code::ShiftRight),
        "Space" => Some(key::Code::Space),
        "Tab" => Some(key::Code::Tab),
        "Convert" => Some(key::Code::Convert),
        "KanaMode" => Some(key::Code::KanaMode),
        "Lang1" => Some(key::Code::Lang1),
        "Lang2" => Some(key::Code::Lang2),
        "Lang3" => Some(key::Code::Lang3),
        "Lang4" => Some(key::Code::Lang4),
        "Lang5" => Some(key::Code::Lang5),
        "NonConvert" => Some(key::Code::NonConvert),
        "Delete" => Some(key::Code::Delete),
        "End" => Some(key::Code::End),
        "Help" => Some(key::Code::Help),
        "Home" => Some(key::Code::Home),
        "Insert" => Some(key::Code::Insert),
        "PageDown" => Some(key::Code::PageDown),
        "PageUp" => Some(key::Code::PageUp),
        "ArrowDown" => Some(key::Code::ArrowDown),
        "ArrowLeft" => Some(key::Code::ArrowLeft),
        "ArrowRight" => Some(key::Code::ArrowRight),
        "ArrowUp" => Some(key::Code::ArrowUp),
        "NumLock" => Some(key::Code::NumLock),
        "Numpad0" => Some(key::Code::Numpad0),
        "Numpad1" => Some(key::Code::Numpad1),
        "Numpad2" => Some(key::Code::Numpad2),
        "Numpad3" => Some(key::Code::Numpad3),
        "Numpad4" => Some(key::Code::Numpad4),
        "Numpad5" => Some(key::Code::Numpad5),
        "Numpad6" => Some(key::Code::Numpad6),
        "Numpad7" => Some(key::Code::Numpad7),
        "Numpad8" => Some(key::Code::Numpad8),
        "Numpad9" => Some(key::Code::Numpad9),
        "NumpadAdd" => Some(key::Code::NumpadAdd),
        "NumpadBackspace" => Some(key::Code::NumpadBackspace),
        "NumpadClear" => Some(key::Code::NumpadClear),
        "NumpadClearEntry" => Some(key::Code::NumpadClearEntry),
        "NumpadComma" => Some(key::Code::NumpadComma),
        "NumpadDecimal" => Some(key::Code::NumpadDecimal),
        "NumpadDivide" => Some(key::Code::NumpadDivide),
        "NumpadEnter" => Some(key::Code::NumpadEnter),
        "NumpadEqual" => Some(key::Code::NumpadEqual),
        "NumpadHash" => Some(key::Code::NumpadHash),
        "NumpadMemoryAdd" => Some(key::Code::NumpadMemoryAdd),
        "NumpadMemoryClear" => Some(key::Code::NumpadMemoryClear),
        "NumpadMemoryRecall" => Some(key::Code::NumpadMemoryRecall),
        "NumpadMemoryStore" => Some(key::Code::NumpadMemoryStore),
        "NumpadMemorySubtract" => Some(key::Code::NumpadMemorySubtract),
        "NumpadMultiply" => Some(key::Code::NumpadMultiply),
        "NumpadParenLeft" => Some(key::Code::NumpadParenLeft),
        "NumpadParenRight" => Some(key::Code::NumpadParenRight),
        "NumpadStar" => Some(key::Code::NumpadStar),
        "NumpadSubtract" => Some(key::Code::NumpadSubtract),
        "Escape" => Some(key::Code::Escape),
        "Fn" => Some(key::Code::Fn),
        "FnLock" => Some(key::Code::FnLock),
        "PrintScreen" => Some(key::Code::PrintScreen),
        "ScrollLock" => Some(key::Code::ScrollLock),
        "Pause" => Some(key::Code::Pause),
        "BrowserBack" => Some(key::Code::BrowserBack),
        "BrowserFavorites" => Some(key::Code::BrowserFavorites),
        "BrowserForward" => Some(key::Code::BrowserForward),
        "BrowserHome" => Some(key::Code::BrowserHome),
        "BrowserRefresh" => Some(key::Code::BrowserRefresh),
        "BrowserSearch" => Some(key::Code::BrowserSearch),
        "BrowserStop" => Some(key::Code::BrowserStop),
        "Eject" => Some(key::Code::Eject),
        "LaunchApp1" => Some(key::Code::LaunchApp1),
        "LaunchApp2" => Some(key::Code::LaunchApp2),
        "LaunchMail" => Some(key::Code::LaunchMail),
        "MediaPlayPause" => Some(key::Code::MediaPlayPause),
        "MediaSelect" => Some(key::Code::MediaSelect),
        "MediaStop" => Some(key::Code::MediaStop),
        "MediaTrackNext" => Some(key::Code::MediaTrackNext),
        "MediaTrackPrevious" => Some(key::Code::MediaTrackPrevious),
        "Power" => Some(key::Code::Power),
        "Sleep" => Some(key::Code::Sleep),
        "AudioVolumeDown" => Some(key::Code::AudioVolumeDown),
        "AudioVolumeMute" => Some(key::Code::AudioVolumeMute),
        "AudioVolumeUp" => Some(key::Code::AudioVolumeUp),
        "WakeUp" => Some(key::Code::WakeUp),
        "Meta" => Some(key::Code::Meta),
        "Hyper" => Some(key::Code::Hyper),
        "Turbo" => Some(key::Code::Turbo),
        "Abort" => Some(key::Code::Abort),
        "Resume" => Some(key::Code::Resume),
        "Suspend" => Some(key::Code::Suspend),
        "Again" => Some(key::Code::Again),
        "Copy" => Some(key::Code::Copy),
        "Cut" => Some(key::Code::Cut),
        "Find" => Some(key::Code::Find),
        "Open" => Some(key::Code::Open),
        "Paste" => Some(key::Code::Paste),
        "Props" => Some(key::Code::Props),
        "Select" => Some(key::Code::Select),
        "Undo" => Some(key::Code::Undo),
        "Hiragana" => Some(key::Code::Hiragana),
        "Katakana" => Some(key::Code::Katakana),
        "F1" => Some(key::Code::F1),
        "F2" => Some(key::Code::F2),
        "F3" => Some(key::Code::F3),
        "F4" => Some(key::Code::F4),
        "F5" => Some(key::Code::F5),
        "F6" => Some(key::Code::F6),
        "F7" => Some(key::Code::F7),
        "F8" => Some(key::Code::F8),
        "F9" => Some(key::Code::F9),
        "F10" => Some(key::Code::F10),
        "F11" => Some(key::Code::F11),
        "F12" => Some(key::Code::F12),
        "F13" => Some(key::Code::F13),
        "F14" => Some(key::Code::F14),
        "F15" => Some(key::Code::F15),
        "F16" => Some(key::Code::F16),
        "F17" => Some(key::Code::F17),
        "F18" => Some(key::Code::F18),
        "F19" => Some(key::Code::F19),
        "F20" => Some(key::Code::F20),
        "F21" => Some(key::Code::F21),
        "F22" => Some(key::Code::F22),
        "F23" => Some(key::Code::F23),
        "F24" => Some(key::Code::F24),
        "F25" => Some(key::Code::F25),
        "F26" => Some(key::Code::F26),
        "F27" => Some(key::Code::F27),
        "F28" => Some(key::Code::F28),
        "F29" => Some(key::Code::F29),
        "F30" => Some(key::Code::F30),
        "F31" => Some(key::Code::F31),
        "F32" => Some(key::Code::F32),
        "F33" => Some(key::Code::F33),
        "F34" => Some(key::Code::F34),
        "F35" => Some(key::Code::F35),
        _ => None,
    }
}

/// Converts a string produced by `format!("{:?}", named)` back to an iced `Named` key.
///
/// Generated from the `Named` enum in iced_core 0.14; regenerate when upgrading iced.
fn named_key_from_str(s: &str) -> Key {
    match s {
        "Alt" => Key::Named(Named::Alt),
        "AltGraph" => Key::Named(Named::AltGraph),
        "CapsLock" => Key::Named(Named::CapsLock),
        "Control" => Key::Named(Named::Control),
        "Fn" => Key::Named(Named::Fn),
        "FnLock" => Key::Named(Named::FnLock),
        "NumLock" => Key::Named(Named::NumLock),
        "ScrollLock" => Key::Named(Named::ScrollLock),
        "Shift" => Key::Named(Named::Shift),
        "Symbol" => Key::Named(Named::Symbol),
        "SymbolLock" => Key::Named(Named::SymbolLock),
        "Meta" => Key::Named(Named::Meta),
        "Hyper" => Key::Named(Named::Hyper),
        "Super" => Key::Named(Named::Super),
        "Enter" => Key::Named(Named::Enter),
        "Tab" => Key::Named(Named::Tab),
        "Space" => Key::Named(Named::Space),
        "ArrowDown" => Key::Named(Named::ArrowDown),
        "ArrowLeft" => Key::Named(Named::ArrowLeft),
        "ArrowRight" => Key::Named(Named::ArrowRight),
        "ArrowUp" => Key::Named(Named::ArrowUp),
        "End" => Key::Named(Named::End),
        "Home" => Key::Named(Named::Home),
        "PageDown" => Key::Named(Named::PageDown),
        "PageUp" => Key::Named(Named::PageUp),
        "Backspace" => Key::Named(Named::Backspace),
        "Clear" => Key::Named(Named::Clear),
        "Copy" => Key::Named(Named::Copy),
        "CrSel" => Key::Named(Named::CrSel),
        "Cut" => Key::Named(Named::Cut),
        "Delete" => Key::Named(Named::Delete),
        "EraseEof" => Key::Named(Named::EraseEof),
        "ExSel" => Key::Named(Named::ExSel),
        "Insert" => Key::Named(Named::Insert),
        "Paste" => Key::Named(Named::Paste),
        "Redo" => Key::Named(Named::Redo),
        "Undo" => Key::Named(Named::Undo),
        "Accept" => Key::Named(Named::Accept),
        "Again" => Key::Named(Named::Again),
        "Attn" => Key::Named(Named::Attn),
        "Cancel" => Key::Named(Named::Cancel),
        "ContextMenu" => Key::Named(Named::ContextMenu),
        "Escape" => Key::Named(Named::Escape),
        "Execute" => Key::Named(Named::Execute),
        "Find" => Key::Named(Named::Find),
        "Help" => Key::Named(Named::Help),
        "Pause" => Key::Named(Named::Pause),
        "Play" => Key::Named(Named::Play),
        "Props" => Key::Named(Named::Props),
        "Select" => Key::Named(Named::Select),
        "ZoomIn" => Key::Named(Named::ZoomIn),
        "ZoomOut" => Key::Named(Named::ZoomOut),
        "BrightnessDown" => Key::Named(Named::BrightnessDown),
        "BrightnessUp" => Key::Named(Named::BrightnessUp),
        "Eject" => Key::Named(Named::Eject),
        "LogOff" => Key::Named(Named::LogOff),
        "Power" => Key::Named(Named::Power),
        "PowerOff" => Key::Named(Named::PowerOff),
        "PrintScreen" => Key::Named(Named::PrintScreen),
        "Hibernate" => Key::Named(Named::Hibernate),
        "Standby" => Key::Named(Named::Standby),
        "WakeUp" => Key::Named(Named::WakeUp),
        "AllCandidates" => Key::Named(Named::AllCandidates),
        "Alphanumeric" => Key::Named(Named::Alphanumeric),
        "CodeInput" => Key::Named(Named::CodeInput),
        "Compose" => Key::Named(Named::Compose),
        "Convert" => Key::Named(Named::Convert),
        "FinalMode" => Key::Named(Named::FinalMode),
        "GroupFirst" => Key::Named(Named::GroupFirst),
        "GroupLast" => Key::Named(Named::GroupLast),
        "GroupNext" => Key::Named(Named::GroupNext),
        "GroupPrevious" => Key::Named(Named::GroupPrevious),
        "ModeChange" => Key::Named(Named::ModeChange),
        "NextCandidate" => Key::Named(Named::NextCandidate),
        "NonConvert" => Key::Named(Named::NonConvert),
        "PreviousCandidate" => Key::Named(Named::PreviousCandidate),
        "Process" => Key::Named(Named::Process),
        "SingleCandidate" => Key::Named(Named::SingleCandidate),
        "HangulMode" => Key::Named(Named::HangulMode),
        "HanjaMode" => Key::Named(Named::HanjaMode),
        "JunjaMode" => Key::Named(Named::JunjaMode),
        "Eisu" => Key::Named(Named::Eisu),
        "Hankaku" => Key::Named(Named::Hankaku),
        "Hiragana" => Key::Named(Named::Hiragana),
        "HiraganaKatakana" => Key::Named(Named::HiraganaKatakana),
        "KanaMode" => Key::Named(Named::KanaMode),
        "KanjiMode" => Key::Named(Named::KanjiMode),
        "Katakana" => Key::Named(Named::Katakana),
        "Romaji" => Key::Named(Named::Romaji),
        "Zenkaku" => Key::Named(Named::Zenkaku),
        "ZenkakuHankaku" => Key::Named(Named::ZenkakuHankaku),
        "Soft1" => Key::Named(Named::Soft1),
        "Soft2" => Key::Named(Named::Soft2),
        "Soft3" => Key::Named(Named::Soft3),
        "Soft4" => Key::Named(Named::Soft4),
        "ChannelDown" => Key::Named(Named::ChannelDown),
        "ChannelUp" => Key::Named(Named::ChannelUp),
        "Close" => Key::Named(Named::Close),
        "MailForward" => Key::Named(Named::MailForward),
        "MailReply" => Key::Named(Named::MailReply),
        "MailSend" => Key::Named(Named::MailSend),
        "MediaClose" => Key::Named(Named::MediaClose),
        "MediaFastForward" => Key::Named(Named::MediaFastForward),
        "MediaPause" => Key::Named(Named::MediaPause),
        "MediaPlay" => Key::Named(Named::MediaPlay),
        "MediaPlayPause" => Key::Named(Named::MediaPlayPause),
        "MediaRecord" => Key::Named(Named::MediaRecord),
        "MediaRewind" => Key::Named(Named::MediaRewind),
        "MediaStop" => Key::Named(Named::MediaStop),
        "MediaTrackNext" => Key::Named(Named::MediaTrackNext),
        "MediaTrackPrevious" => Key::Named(Named::MediaTrackPrevious),
        "New" => Key::Named(Named::New),
        "Open" => Key::Named(Named::Open),
        "Print" => Key::Named(Named::Print),
        "Save" => Key::Named(Named::Save),
        "SpellCheck" => Key::Named(Named::SpellCheck),
        "Key11" => Key::Named(Named::Key11),
        "Key12" => Key::Named(Named::Key12),
        "AudioBalanceLeft" => Key::Named(Named::AudioBalanceLeft),
        "AudioBalanceRight" => Key::Named(Named::AudioBalanceRight),
        "AudioBassBoostDown" => Key::Named(Named::AudioBassBoostDown),
        "AudioBassBoostToggle" => Key::Named(Named::AudioBassBoostToggle),
        "AudioBassBoostUp" => Key::Named(Named::AudioBassBoostUp),
        "AudioFaderFront" => Key::Named(Named::AudioFaderFront),
        "AudioFaderRear" => Key::Named(Named::AudioFaderRear),
        "AudioSurroundModeNext" => Key::Named(Named::AudioSurroundModeNext),
        "AudioTrebleDown" => Key::Named(Named::AudioTrebleDown),
        "AudioTrebleUp" => Key::Named(Named::AudioTrebleUp),
        "AudioVolumeDown" => Key::Named(Named::AudioVolumeDown),
        "AudioVolumeUp" => Key::Named(Named::AudioVolumeUp),
        "AudioVolumeMute" => Key::Named(Named::AudioVolumeMute),
        "MicrophoneToggle" => Key::Named(Named::MicrophoneToggle),
        "MicrophoneVolumeDown" => Key::Named(Named::MicrophoneVolumeDown),
        "MicrophoneVolumeUp" => Key::Named(Named::MicrophoneVolumeUp),
        "MicrophoneVolumeMute" => Key::Named(Named::MicrophoneVolumeMute),
        "SpeechCorrectionList" => Key::Named(Named::SpeechCorrectionList),
        "SpeechInputToggle" => Key::Named(Named::SpeechInputToggle),
        "LaunchApplication1" => Key::Named(Named::LaunchApplication1),
        "LaunchApplication2" => Key::Named(Named::LaunchApplication2),
        "LaunchCalendar" => Key::Named(Named::LaunchCalendar),
        "LaunchContacts" => Key::Named(Named::LaunchContacts),
        "LaunchMail" => Key::Named(Named::LaunchMail),
        "LaunchMediaPlayer" => Key::Named(Named::LaunchMediaPlayer),
        "LaunchMusicPlayer" => Key::Named(Named::LaunchMusicPlayer),
        "LaunchPhone" => Key::Named(Named::LaunchPhone),
        "LaunchScreenSaver" => Key::Named(Named::LaunchScreenSaver),
        "LaunchSpreadsheet" => Key::Named(Named::LaunchSpreadsheet),
        "LaunchWebBrowser" => Key::Named(Named::LaunchWebBrowser),
        "LaunchWebCam" => Key::Named(Named::LaunchWebCam),
        "LaunchWordProcessor" => Key::Named(Named::LaunchWordProcessor),
        "BrowserBack" => Key::Named(Named::BrowserBack),
        "BrowserFavorites" => Key::Named(Named::BrowserFavorites),
        "BrowserForward" => Key::Named(Named::BrowserForward),
        "BrowserHome" => Key::Named(Named::BrowserHome),
        "BrowserRefresh" => Key::Named(Named::BrowserRefresh),
        "BrowserSearch" => Key::Named(Named::BrowserSearch),
        "BrowserStop" => Key::Named(Named::BrowserStop),
        "AppSwitch" => Key::Named(Named::AppSwitch),
        "Call" => Key::Named(Named::Call),
        "Camera" => Key::Named(Named::Camera),
        "CameraFocus" => Key::Named(Named::CameraFocus),
        "EndCall" => Key::Named(Named::EndCall),
        "GoBack" => Key::Named(Named::GoBack),
        "GoHome" => Key::Named(Named::GoHome),
        "HeadsetHook" => Key::Named(Named::HeadsetHook),
        "LastNumberRedial" => Key::Named(Named::LastNumberRedial),
        "Notification" => Key::Named(Named::Notification),
        "MannerMode" => Key::Named(Named::MannerMode),
        "VoiceDial" => Key::Named(Named::VoiceDial),
        "TV" => Key::Named(Named::TV),
        "TV3DMode" => Key::Named(Named::TV3DMode),
        "TVAntennaCable" => Key::Named(Named::TVAntennaCable),
        "TVAudioDescription" => Key::Named(Named::TVAudioDescription),
        "TVAudioDescriptionMixDown" => Key::Named(Named::TVAudioDescriptionMixDown),
        "TVAudioDescriptionMixUp" => Key::Named(Named::TVAudioDescriptionMixUp),
        "TVContentsMenu" => Key::Named(Named::TVContentsMenu),
        "TVDataService" => Key::Named(Named::TVDataService),
        "TVInput" => Key::Named(Named::TVInput),
        "TVInputComponent1" => Key::Named(Named::TVInputComponent1),
        "TVInputComponent2" => Key::Named(Named::TVInputComponent2),
        "TVInputComposite1" => Key::Named(Named::TVInputComposite1),
        "TVInputComposite2" => Key::Named(Named::TVInputComposite2),
        "TVInputHDMI1" => Key::Named(Named::TVInputHDMI1),
        "TVInputHDMI2" => Key::Named(Named::TVInputHDMI2),
        "TVInputHDMI3" => Key::Named(Named::TVInputHDMI3),
        "TVInputHDMI4" => Key::Named(Named::TVInputHDMI4),
        "TVInputVGA1" => Key::Named(Named::TVInputVGA1),
        "TVMediaContext" => Key::Named(Named::TVMediaContext),
        "TVNetwork" => Key::Named(Named::TVNetwork),
        "TVNumberEntry" => Key::Named(Named::TVNumberEntry),
        "TVPower" => Key::Named(Named::TVPower),
        "TVRadioService" => Key::Named(Named::TVRadioService),
        "TVSatellite" => Key::Named(Named::TVSatellite),
        "TVSatelliteBS" => Key::Named(Named::TVSatelliteBS),
        "TVSatelliteCS" => Key::Named(Named::TVSatelliteCS),
        "TVSatelliteToggle" => Key::Named(Named::TVSatelliteToggle),
        "TVTerrestrialAnalog" => Key::Named(Named::TVTerrestrialAnalog),
        "TVTerrestrialDigital" => Key::Named(Named::TVTerrestrialDigital),
        "TVTimer" => Key::Named(Named::TVTimer),
        "AVRInput" => Key::Named(Named::AVRInput),
        "AVRPower" => Key::Named(Named::AVRPower),
        "ColorF0Red" => Key::Named(Named::ColorF0Red),
        "ColorF1Green" => Key::Named(Named::ColorF1Green),
        "ColorF2Yellow" => Key::Named(Named::ColorF2Yellow),
        "ColorF3Blue" => Key::Named(Named::ColorF3Blue),
        "ColorF4Grey" => Key::Named(Named::ColorF4Grey),
        "ColorF5Brown" => Key::Named(Named::ColorF5Brown),
        "ClosedCaptionToggle" => Key::Named(Named::ClosedCaptionToggle),
        "Dimmer" => Key::Named(Named::Dimmer),
        "DisplaySwap" => Key::Named(Named::DisplaySwap),
        "DVR" => Key::Named(Named::DVR),
        "Exit" => Key::Named(Named::Exit),
        "FavoriteClear0" => Key::Named(Named::FavoriteClear0),
        "FavoriteClear1" => Key::Named(Named::FavoriteClear1),
        "FavoriteClear2" => Key::Named(Named::FavoriteClear2),
        "FavoriteClear3" => Key::Named(Named::FavoriteClear3),
        "FavoriteRecall0" => Key::Named(Named::FavoriteRecall0),
        "FavoriteRecall1" => Key::Named(Named::FavoriteRecall1),
        "FavoriteRecall2" => Key::Named(Named::FavoriteRecall2),
        "FavoriteRecall3" => Key::Named(Named::FavoriteRecall3),
        "FavoriteStore0" => Key::Named(Named::FavoriteStore0),
        "FavoriteStore1" => Key::Named(Named::FavoriteStore1),
        "FavoriteStore2" => Key::Named(Named::FavoriteStore2),
        "FavoriteStore3" => Key::Named(Named::FavoriteStore3),
        "Guide" => Key::Named(Named::Guide),
        "GuideNextDay" => Key::Named(Named::GuideNextDay),
        "GuidePreviousDay" => Key::Named(Named::GuidePreviousDay),
        "Info" => Key::Named(Named::Info),
        "InstantReplay" => Key::Named(Named::InstantReplay),
        "Link" => Key::Named(Named::Link),
        "ListProgram" => Key::Named(Named::ListProgram),
        "LiveContent" => Key::Named(Named::LiveContent),
        "Lock" => Key::Named(Named::Lock),
        "MediaApps" => Key::Named(Named::MediaApps),
        "MediaAudioTrack" => Key::Named(Named::MediaAudioTrack),
        "MediaLast" => Key::Named(Named::MediaLast),
        "MediaSkipBackward" => Key::Named(Named::MediaSkipBackward),
        "MediaSkipForward" => Key::Named(Named::MediaSkipForward),
        "MediaStepBackward" => Key::Named(Named::MediaStepBackward),
        "MediaStepForward" => Key::Named(Named::MediaStepForward),
        "MediaTopMenu" => Key::Named(Named::MediaTopMenu),
        "NavigateIn" => Key::Named(Named::NavigateIn),
        "NavigateNext" => Key::Named(Named::NavigateNext),
        "NavigateOut" => Key::Named(Named::NavigateOut),
        "NavigatePrevious" => Key::Named(Named::NavigatePrevious),
        "NextFavoriteChannel" => Key::Named(Named::NextFavoriteChannel),
        "NextUserProfile" => Key::Named(Named::NextUserProfile),
        "OnDemand" => Key::Named(Named::OnDemand),
        "Pairing" => Key::Named(Named::Pairing),
        "PinPDown" => Key::Named(Named::PinPDown),
        "PinPMove" => Key::Named(Named::PinPMove),
        "PinPToggle" => Key::Named(Named::PinPToggle),
        "PinPUp" => Key::Named(Named::PinPUp),
        "PlaySpeedDown" => Key::Named(Named::PlaySpeedDown),
        "PlaySpeedReset" => Key::Named(Named::PlaySpeedReset),
        "PlaySpeedUp" => Key::Named(Named::PlaySpeedUp),
        "RandomToggle" => Key::Named(Named::RandomToggle),
        "RcLowBattery" => Key::Named(Named::RcLowBattery),
        "RecordSpeedNext" => Key::Named(Named::RecordSpeedNext),
        "RfBypass" => Key::Named(Named::RfBypass),
        "ScanChannelsToggle" => Key::Named(Named::ScanChannelsToggle),
        "ScreenModeNext" => Key::Named(Named::ScreenModeNext),
        "Settings" => Key::Named(Named::Settings),
        "SplitScreenToggle" => Key::Named(Named::SplitScreenToggle),
        "STBInput" => Key::Named(Named::STBInput),
        "STBPower" => Key::Named(Named::STBPower),
        "Subtitle" => Key::Named(Named::Subtitle),
        "Teletext" => Key::Named(Named::Teletext),
        "VideoModeNext" => Key::Named(Named::VideoModeNext),
        "Wink" => Key::Named(Named::Wink),
        "ZoomToggle" => Key::Named(Named::ZoomToggle),
        "F1" => Key::Named(Named::F1),
        "F2" => Key::Named(Named::F2),
        "F3" => Key::Named(Named::F3),
        "F4" => Key::Named(Named::F4),
        "F5" => Key::Named(Named::F5),
        "F6" => Key::Named(Named::F6),
        "F7" => Key::Named(Named::F7),
        "F8" => Key::Named(Named::F8),
        "F9" => Key::Named(Named::F9),
        "F10" => Key::Named(Named::F10),
        "F11" => Key::Named(Named::F11),
        "F12" => Key::Named(Named::F12),
        "F13" => Key::Named(Named::F13),
        "F14" => Key::Named(Named::F14),
        "F15" => Key::Named(Named::F15),
        "F16" => Key::Named(Named::F16),
        "F17" => Key::Named(Named::F17),
        "F18" => Key::Named(Named::F18),
        "F19" => Key::Named(Named::F19),
        "F20" => Key::Named(Named::F20),
        "F21" => Key::Named(Named::F21),
        "F22" => Key::Named(Named::F22),
        "F23" => Key::Named(Named::F23),
        "F24" => Key::Named(Named::F24),
        "F25" => Key::Named(Named::F25),
        "F26" => Key::Named(Named::F26),
        "F27" => Key::Named(Named::F27),
        "F28" => Key::Named(Named::F28),
        "F29" => Key::Named(Named::F29),
        "F30" => Key::Named(Named::F30),
        "F31" => Key::Named(Named::F31),
        "F32" => Key::Named(Named::F32),
        "F33" => Key::Named(Named::F33),
        "F34" => Key::Named(Named::F34),
        "F35" => Key::Named(Named::F35),
        _ => Key::Unidentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::{Key, key::Named};

    #[test]
    fn test_named_key_from_str_modifier_keys() {
        // Test modifier keys
        assert_eq!(named_key_from_str("Alt"), Key::Named(Named::Alt));
        assert_eq!(named_key_from_str("Control"), Key::Named(Named::Control));
        assert_eq!(named_key_from_str("Shift"), Key::Named(Named::Shift));
        assert_eq!(named_key_from_str("Super"), Key::Named(Named::Super));
        assert_eq!(named_key_from_str("Meta"), Key::Named(Named::Meta));
        assert_eq!(named_key_from_str("AltGraph"), Key::Named(Named::AltGraph));
    }

    #[test]
    fn test_named_key_from_str_navigation_keys() {
        // Test navigation keys
        assert_eq!(named_key_from_str("Enter"), Key::Named(Named::Enter));
        assert_eq!(named_key_from_str("Tab"), Key::Named(Named::Tab));
        assert_eq!(named_key_from_str("Space"), Key::Named(Named::Space));
        assert_eq!(named_key_from_str("Escape"), Key::Named(Named::Escape));
        assert_eq!(
            named_key_from_str("Backspace"),
            Key::Named(Named::Backspace)
        );
        assert_eq!(named_key_from_str("Delete"), Key::Named(Named::Delete));
    }

    #[test]
    fn test_named_key_from_str_arrow_keys() {
        // Test arrow keys
        assert_eq!(named_key_from_str("ArrowUp"), Key::Named(Named::ArrowUp));
        assert_eq!(
            named_key_from_str("ArrowDown"),
            Key::Named(Named::ArrowDown)
        );
        assert_eq!(
            named_key_from_str("ArrowLeft"),
            Key::Named(Named::ArrowLeft)
        );
        assert_eq!(
            named_key_from_str("ArrowRight"),
            Key::Named(Named::ArrowRight)
        );
    }

    #[test]
    fn test_named_key_from_str_home_end_keys() {
        // Test Home/End/PageUp/PageDown keys
        assert_eq!(named_key_from_str("Home"), Key::Named(Named::Home));
        assert_eq!(named_key_from_str("End"), Key::Named(Named::End));
        assert_eq!(named_key_from_str("PageUp"), Key::Named(Named::PageUp));
        assert_eq!(named_key_from_str("PageDown"), Key::Named(Named::PageDown));
    }

    #[test]
    fn test_named_key_from_str_function_keys() {
        // Test function keys F1-F12
        assert_eq!(named_key_from_str("F1"), Key::Named(Named::F1));
        assert_eq!(named_key_from_str("F2"), Key::Named(Named::F2));
        assert_eq!(named_key_from_str("F3"), Key::Named(Named::F3));
        assert_eq!(named_key_from_str("F4"), Key::Named(Named::F4));
        assert_eq!(named_key_from_str("F5"), Key::Named(Named::F5));
        assert_eq!(named_key_from_str("F6"), Key::Named(Named::F6));
        assert_eq!(named_key_from_str("F7"), Key::Named(Named::F7));
        assert_eq!(named_key_from_str("F8"), Key::Named(Named::F8));
        assert_eq!(named_key_from_str("F9"), Key::Named(Named::F9));
        assert_eq!(named_key_from_str("F10"), Key::Named(Named::F10));
        assert_eq!(named_key_from_str("F11"), Key::Named(Named::F11));
        assert_eq!(named_key_from_str("F12"), Key::Named(Named::F12));
    }

    #[test]
    fn test_named_key_from_str_extended_function_keys() {
        // Test extended function keys F13-F35
        assert_eq!(named_key_from_str("F13"), Key::Named(Named::F13));
        assert_eq!(named_key_from_str("F20"), Key::Named(Named::F20));
        assert_eq!(named_key_from_str("F24"), Key::Named(Named::F24));
        assert_eq!(named_key_from_str("F30"), Key::Named(Named::F30));
        assert_eq!(named_key_from_str("F35"), Key::Named(Named::F35));
    }

    #[test]
    fn test_named_key_from_str_lock_keys() {
        // Test lock keys
        assert_eq!(named_key_from_str("CapsLock"), Key::Named(Named::CapsLock));
        assert_eq!(named_key_from_str("NumLock"), Key::Named(Named::NumLock));
        assert_eq!(
            named_key_from_str("ScrollLock"),
            Key::Named(Named::ScrollLock)
        );
    }

    #[test]
    fn test_named_key_from_str_editing_keys() {
        // Test editing keys
        assert_eq!(named_key_from_str("Insert"), Key::Named(Named::Insert));
        assert_eq!(named_key_from_str("Copy"), Key::Named(Named::Copy));
        assert_eq!(named_key_from_str("Cut"), Key::Named(Named::Cut));
        assert_eq!(named_key_from_str("Paste"), Key::Named(Named::Paste));
        assert_eq!(named_key_from_str("Undo"), Key::Named(Named::Undo));
        assert_eq!(named_key_from_str("Redo"), Key::Named(Named::Redo));
    }

    #[test]
    fn test_named_key_from_str_special_keys() {
        // Test special keys
        assert_eq!(
            named_key_from_str("PrintScreen"),
            Key::Named(Named::PrintScreen)
        );
        assert_eq!(named_key_from_str("Pause"), Key::Named(Named::Pause));
        assert_eq!(
            named_key_from_str("ContextMenu"),
            Key::Named(Named::ContextMenu)
        );
        assert_eq!(named_key_from_str("Help"), Key::Named(Named::Help));
    }

    #[test]
    fn test_named_key_from_str_media_keys() {
        // Test media keys
        assert_eq!(
            named_key_from_str("MediaPlay"),
            Key::Named(Named::MediaPlay)
        );
        assert_eq!(
            named_key_from_str("MediaPause"),
            Key::Named(Named::MediaPause)
        );
        assert_eq!(
            named_key_from_str("MediaPlayPause"),
            Key::Named(Named::MediaPlayPause)
        );
        assert_eq!(
            named_key_from_str("MediaStop"),
            Key::Named(Named::MediaStop)
        );
        assert_eq!(
            named_key_from_str("MediaTrackNext"),
            Key::Named(Named::MediaTrackNext)
        );
        assert_eq!(
            named_key_from_str("MediaTrackPrevious"),
            Key::Named(Named::MediaTrackPrevious)
        );
    }

    #[test]
    fn test_named_key_from_str_browser_keys() {
        // Test browser keys
        assert_eq!(
            named_key_from_str("BrowserBack"),
            Key::Named(Named::BrowserBack)
        );
        assert_eq!(
            named_key_from_str("BrowserForward"),
            Key::Named(Named::BrowserForward)
        );
        assert_eq!(
            named_key_from_str("BrowserRefresh"),
            Key::Named(Named::BrowserRefresh)
        );
        assert_eq!(
            named_key_from_str("BrowserHome"),
            Key::Named(Named::BrowserHome)
        );
        assert_eq!(
            named_key_from_str("BrowserSearch"),
            Key::Named(Named::BrowserSearch)
        );
        assert_eq!(
            named_key_from_str("BrowserFavorites"),
            Key::Named(Named::BrowserFavorites)
        );
        assert_eq!(
            named_key_from_str("BrowserStop"),
            Key::Named(Named::BrowserStop)
        );
    }

    #[test]
    fn test_named_key_from_str_invalid_keys() {
        // Test invalid/unrecognized key names
        assert_eq!(named_key_from_str("InvalidKey"), Key::Unidentified);
        assert_eq!(named_key_from_str(""), Key::Unidentified);
        assert_eq!(named_key_from_str("123"), Key::Unidentified);
        assert_eq!(named_key_from_str("!@#$"), Key::Unidentified);
        assert_eq!(named_key_from_str("NotAKey"), Key::Unidentified);
    }

    #[test]
    fn test_named_key_from_str_case_sensitivity() {
        // Test case sensitivity - the function is case-sensitive
        assert_eq!(named_key_from_str("enter"), Key::Unidentified); // lowercase
        assert_eq!(named_key_from_str("ENTER"), Key::Unidentified); // uppercase
        assert_eq!(named_key_from_str("Enter"), Key::Named(Named::Enter)); // correct case

        assert_eq!(named_key_from_str("space"), Key::Unidentified); // lowercase
        assert_eq!(named_key_from_str("SPACE"), Key::Unidentified); // uppercase
        assert_eq!(named_key_from_str("Space"), Key::Named(Named::Space)); // correct case
    }

    #[test]
    fn test_named_key_from_str_edge_cases() {
        // Test edge cases
        assert_eq!(named_key_from_str("F0"), Key::Unidentified); // F0 doesn't exist
        assert_eq!(named_key_from_str("F36"), Key::Unidentified); // Beyond F35
        assert_eq!(named_key_from_str("F1000"), Key::Unidentified); // Way beyond range

        // Test with extra spaces (should not match)
        assert_eq!(named_key_from_str(" Enter"), Key::Unidentified);
        assert_eq!(named_key_from_str("Enter "), Key::Unidentified);
        assert_eq!(named_key_from_str(" Enter "), Key::Unidentified);
    }

    #[test]
    fn test_named_key_from_str_less_common_keys() {
        // Test less common but valid keys
        assert_eq!(named_key_from_str("Fn"), Key::Named(Named::Fn));
        assert_eq!(named_key_from_str("FnLock"), Key::Named(Named::FnLock));
        assert_eq!(named_key_from_str("Hyper"), Key::Named(Named::Hyper));
        assert_eq!(named_key_from_str("Symbol"), Key::Named(Named::Symbol));
        assert_eq!(
            named_key_from_str("SymbolLock"),
            Key::Named(Named::SymbolLock)
        );
        assert_eq!(named_key_from_str("Clear"), Key::Named(Named::Clear));
        assert_eq!(named_key_from_str("Execute"), Key::Named(Named::Execute));
        assert_eq!(named_key_from_str("Select"), Key::Named(Named::Select));
        assert_eq!(named_key_from_str("Find"), Key::Named(Named::Find));
        assert_eq!(named_key_from_str("Again"), Key::Named(Named::Again));
        assert_eq!(named_key_from_str("Props"), Key::Named(Named::Props));
        assert_eq!(named_key_from_str("ZoomIn"), Key::Named(Named::ZoomIn));
        assert_eq!(named_key_from_str("ZoomOut"), Key::Named(Named::ZoomOut));
    }

    #[test]
    fn test_set_key_and_modifiers_from_iced() {
        let mut hotkey = HotkeyDefinition {
            key: "".to_string(),
            modifiers: vec![],
            script: None,
            package: None,
            language: smudgy_core::models::ScriptLang::Plaintext,
            enabled: true,
        };

        let keys = vec![
            Key::Named(Named::Control),
            Key::Named(Named::Shift),
            Key::Character("a".into()),
        ];

        set_key_and_modifiers_from_iced(&mut hotkey, keys);

        assert_eq!(hotkey.key, "a");
        assert_eq!(hotkey.modifiers, vec!["CTRL", "SHIFT"]);
    }

    #[test]
    fn test_hotkey_to_maybe_physical_key() {
        let hotkey = HotkeyDefinition {
            key: "Space".to_string(),
            modifiers: vec!["CTRL".to_string(), "ALT".to_string()],
            script: None,
            package: None,
            language: smudgy_core::models::ScriptLang::Plaintext,
            enabled: true,
        };

        let maybe_key = hotkey_to_maybe_physical_key(&hotkey);

        match maybe_key {
            MaybePhysicalKey::Key(Key::Named(Named::Space)) => {
                // Expected result
            }
            _ => panic!("Expected MaybePhysicalKey::Key(Key::Named(Named::Space))"),
        }
    }

    #[test]
    fn test_from_hotkey_definition() {
        let hotkey = HotkeyDefinition {
            key: "Enter".to_string(),
            modifiers: vec!["CTRL".to_string(), "SHIFT".to_string()],
            script: None,
            package: None,
            language: smudgy_core::models::ScriptLang::Plaintext,
            enabled: true,
        };

        let hotkey_keys: HotkeyKeys = hotkey.into();

        // Test the main key
        match hotkey_keys.main_key {
            MaybePhysicalKey::Key(Key::Named(Named::Enter)) => {
                // Expected result
            }
            _ => panic!("Expected MaybePhysicalKey::Key(Key::Named(Named::Enter))"),
        }

        // Test the modifiers
        assert!(hotkey_keys.modifiers.contains(Modifiers::CTRL));
        assert!(hotkey_keys.modifiers.contains(Modifiers::SHIFT));
        assert!(!hotkey_keys.modifiers.contains(Modifiers::ALT));
    }

    #[test]
    fn test_hotkey_to_maybe_physical_key_character() {
        let hotkey = HotkeyDefinition {
            key: "Character(a)".to_string(),
            modifiers: vec![],
            script: None,
            package: None,
            language: smudgy_core::models::ScriptLang::Plaintext,
            enabled: true,
        };

        let maybe_key = hotkey_to_maybe_physical_key(&hotkey);

        match maybe_key {
            MaybePhysicalKey::Key(Key::Character(c)) if c.as_str() == "a" => {
                // Expected result
            }
            _ => panic!("Expected MaybePhysicalKey::Key(Key::Character('a'))"),
        }
    }

    #[test]
    fn test_hotkey_to_maybe_physical_key_code() {
        let hotkey = HotkeyDefinition {
            key: "Code(KeyA)".to_string(),
            modifiers: vec![],
            script: None,
            package: None,
            language: smudgy_core::models::ScriptLang::Plaintext,
            enabled: true,
        };

        let maybe_key = hotkey_to_maybe_physical_key(&hotkey);

        match maybe_key {
            MaybePhysicalKey::Physical(key::Physical::Code(key::Code::KeyA)) => {
                // Expected result
            }
            _ => {
                panic!("Expected MaybePhysicalKey::Physical(key::Physical::Code(key::Code::KeyA))")
            }
        }
    }

    #[test]
    fn test_hotkey_to_maybe_physical_key_code_f1() {
        let hotkey = HotkeyDefinition {
            key: "Code(F1)".to_string(),
            modifiers: vec![],
            script: None,
            package: None,
            language: smudgy_core::models::ScriptLang::Plaintext,
            enabled: true,
        };

        let maybe_key = hotkey_to_maybe_physical_key(&hotkey);

        match maybe_key {
            MaybePhysicalKey::Physical(key::Physical::Code(key::Code::F1)) => {
                // Expected result
            }
            _ => panic!("Expected MaybePhysicalKey::Physical(key::Physical::Code(key::Code::F1))"),
        }
    }

    #[test]
    fn test_hotkey_to_maybe_physical_key_invalid_code() {
        let hotkey = HotkeyDefinition {
            key: "Code(InvalidCode)".to_string(),
            modifiers: vec![],
            script: None,
            package: None,
            language: smudgy_core::models::ScriptLang::Plaintext,
            enabled: true,
        };

        let maybe_key = hotkey_to_maybe_physical_key(&hotkey);

        match maybe_key {
            MaybePhysicalKey::Key(Key::Unidentified) => {
                // Expected result - should fall back to unidentified
            }
            _ => panic!("Expected MaybePhysicalKey::Key(Key::Unidentified) for invalid code"),
        }
    }

    #[test]
    fn test_physical_code_from_str() {
        // Test valid codes
        assert_eq!(physical_code_from_str("KeyA"), Some(key::Code::KeyA));
        assert_eq!(physical_code_from_str("KeyZ"), Some(key::Code::KeyZ));
        assert_eq!(physical_code_from_str("F1"), Some(key::Code::F1));
        assert_eq!(physical_code_from_str("Enter"), Some(key::Code::Enter));
        assert_eq!(physical_code_from_str("Space"), Some(key::Code::Space));

        // Test invalid codes
        assert_eq!(physical_code_from_str("InvalidKey"), None);
        assert_eq!(physical_code_from_str(""), None);
        assert_eq!(physical_code_from_str("NotAKey"), None);
    }
}
