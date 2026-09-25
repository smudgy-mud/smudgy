use iced::keyboard;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MaybePhysicalKey {
    Key(keyboard::Key),
    Physical(keyboard::key::Physical),
}

#[derive(Debug, Clone)]
pub struct HotkeyKeys {
    pub main_key: MaybePhysicalKey,
    pub modifiers: keyboard::Modifiers,
}
