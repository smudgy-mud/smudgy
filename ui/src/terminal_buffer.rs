use iced::Background;
use iced::widget::text::Span;
use selection::Selection;
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    num::NonZeroUsize,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::prefs::TerminalPrefs;
use smudgy_core::session::runtime::line_operation::LineOperation;
use smudgy_core::session::styled_line::{
    Blink, Color, LinkAction, LinkColor, LinkDecoration, LinkSpan, LinkStyle, LinkTextStyle,
    LinkTooltip, LinkVisibility, LinkVisibilityAction, Style, StyledLine, Underline,
};
use unicode_segmentation::UnicodeSegmentation;

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpanMetadata {
    pub blink: Blink,
    pub underline: LinkDecoration,
    pub overline: LinkDecoration,
    pub strikethrough: LinkDecoration,
    pub decoration_color: Option<iced::Color>,
}

type Link = SpanMetadata;

pub mod selection;

pub(crate) fn authored_color(color: LinkColor) -> iced::Color {
    iced::Color::from_rgba8(
        color.red,
        color.green,
        color.blue,
        f32::from(color.alpha) / 255.0,
    )
}

/// A click on a link span, as delivered to the pane's `on_link` handler.
#[derive(Debug, Clone)]
pub struct LinkClickEvent {
    pub action: LinkAction,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// Stable identity for one live link in a terminal buffer.
///
/// Byte offsets are deliberately not part of the identity: scripts and CR
/// overprinting can move an otherwise unchanged link within its line. The
/// buffer remaps this id when a line is replaced, so spoiler, visibility, and
/// widget interaction state follow the link instead of being reset by an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct LinkKey(u64);

#[cfg(test)]
impl LinkKey {
    pub(crate) const fn test(id: u64) -> Self {
        Self(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LinkAddress {
    line: usize,
    begin: usize,
    end: usize,
}

impl LinkAddress {
    fn new(line: usize, link: &LinkSpan) -> Self {
        Self {
            line,
            begin: link.begin_pos,
            end: link.end_pos,
        }
    }
}

#[derive(Debug, Clone)]
struct SelectionValueState {
    selected: bool,
    references: usize,
}

/// Session-scoped state shared by every terminal buffer and every widget that
/// renders them. Link-instance visibility belongs to a buffer; selection
/// groups and visited destinations describe the session-wide protocol.
#[derive(Debug, Default)]
pub(crate) struct LinkProtocolState {
    selected_values: HashMap<(Arc<str>, Arc<str>), SelectionValueState>,
    visited_actions: Vec<LinkAction>,
    visual_generation: u64,
}

impl LinkProtocolState {
    pub(crate) fn selected(
        &self,
        selection: &smudgy_core::session::styled_line::LinkSelection,
    ) -> bool {
        self.selected_values
            .get(&(selection.group.clone(), selection.value.clone()))
            .map_or(selection.selected, |state| state.selected)
    }

    pub(crate) fn visited(&self, action: &LinkAction) -> bool {
        let target = action.disclosed_target().unwrap_or(action);
        self.visited_actions.iter().any(|visited| visited == target)
    }

    pub(crate) fn mark_visited(&mut self, action: &LinkAction) {
        if !self.visited(action) {
            self.visited_actions
                .push(action.disclosed_target().unwrap_or(action).clone());
            self.visual_generation = self.visual_generation.wrapping_add(1);
        }
    }

    pub(crate) fn toggle_link_selection(&mut self, action: &LinkAction) -> Option<bool> {
        let selection = action.protocol()?.selection.as_ref()?;
        let key = (selection.group.clone(), selection.value.clone());
        let current = self.selected_values.get(&key)?.selected;
        if selection.disabled {
            return Some(current);
        }
        let next = if current { !selection.toggle } else { true };
        if next && selection.exclusive {
            for ((group, _), state) in &mut self.selected_values {
                if group == &selection.group {
                    state.selected = false;
                }
            }
        }
        if let Some(state) = self.selected_values.get_mut(&key) {
            state.selected = next;
        }
        self.visual_generation = self.visual_generation.wrapping_add(1);
        Some(next)
    }

    pub(crate) fn record_menu_choice(&mut self, source: &LinkAction) {
        self.toggle_link_selection(source);
        self.mark_visited(source);
    }

    pub(crate) fn visual_generation(&self) -> u64 {
        self.visual_generation
    }

    fn retain_selection(&mut self, selection: &smudgy_core::session::styled_line::LinkSelection) {
        let key = (selection.group.clone(), selection.value.clone());
        if let Some(state) = self.selected_values.get_mut(&key) {
            state.references += 1;
            return;
        }
        let mut changed_existing = false;
        if selection.selected && selection.exclusive {
            for ((group, _), state) in &mut self.selected_values {
                if group == &selection.group {
                    changed_existing |= state.selected;
                    state.selected = false;
                }
            }
        }
        self.selected_values.insert(
            key,
            SelectionValueState {
                selected: selection.selected,
                references: 1,
            },
        );
        // The new line will shape because its source changed. Invalidate other
        // cached lines only if this exclusive default changed their selection.
        if changed_existing {
            self.visual_generation = self.visual_generation.wrapping_add(1);
        }
    }

    fn release_selection(&mut self, selection: &smudgy_core::session::styled_line::LinkSelection) {
        let key = (selection.group.clone(), selection.value.clone());
        let remove = self.selected_values.get_mut(&key).is_some_and(|state| {
            state.references -= 1;
            state.references == 0
        });
        if remove {
            self.selected_values.remove(&key);
        }
    }

    #[cfg(test)]
    fn selection_references(
        &self,
        selection: &smudgy_core::session::styled_line::LinkSelection,
    ) -> usize {
        self.selected_values
            .get(&(selection.group.clone(), selection.value.clone()))
            .map_or(0, |state| state.references)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LinkRegistration {
    action: LinkAction,
    selection: Option<smudgy_core::session::styled_line::LinkSelection>,
    spoiler: bool,
    visibility: Option<LinkVisibility>,
}

impl LinkRegistration {
    fn new(link: &LinkSpan) -> Self {
        let protocol = link.action.protocol();
        Self {
            action: link.action.clone(),
            selection: protocol.and_then(|protocol| protocol.selection.clone()),
            spoiler: protocol.is_some_and(|protocol| protocol.spoiler),
            visibility: protocol.and_then(|protocol| protocol.visibility.clone()),
        }
    }

    fn matches(&self, link: &LinkSpan) -> bool {
        let protocol = link.action.protocol();
        self.action == link.action
            && self.selection.as_ref() == protocol.and_then(|protocol| protocol.selection.as_ref())
            && self.spoiler == protocol.is_some_and(|protocol| protocol.spoiler)
            && self.visibility.as_ref()
                == protocol.and_then(|protocol| protocol.visibility.as_ref())
    }
}

#[derive(Debug, Clone)]
struct LiveLink {
    address: LinkAddress,
    registration: LinkRegistration,
}

#[derive(Debug, Clone)]
pub(crate) struct VisibilityState {
    config: LinkVisibility,
    pub(crate) concealed: bool,
    created: Instant,
    activated: Option<Instant>,
    expiry_activated: bool,
    skip_first_prompt: bool,
    skip_first_output: bool,
    pub(crate) revealed_phase: bool,
}

impl VisibilityState {
    pub(crate) fn new(config: &LinkVisibility) -> Self {
        let (concealed, revealed_phase) = match config.action {
            LinkVisibilityAction::Conceal => (false, false),
            LinkVisibilityAction::Reveal | LinkVisibilityAction::RevealThenConceal => (true, false),
        };
        Self {
            config: config.clone(),
            concealed,
            created: Instant::now(),
            activated: None,
            expiry_activated: false,
            skip_first_prompt: false,
            skip_first_output: false,
            revealed_phase,
        }
    }

    fn has_expiry_trigger(&self) -> bool {
        self.config.expire.input || self.config.expire.prompt || self.config.expire.output
    }

    fn activate_expiry(&mut self) {
        self.expiry_activated = true;
        self.skip_first_prompt = self.config.expire.prompt;
        self.skip_first_output = self.config.expire.output;
    }

    pub(crate) fn apply_expiry(&mut self) -> bool {
        let was_concealed = self.concealed;
        let was_revealed_phase = self.revealed_phase;
        match self.config.action {
            LinkVisibilityAction::Conceal => self.concealed = true,
            LinkVisibilityAction::Reveal => self.concealed = false,
            LinkVisibilityAction::RevealThenConceal if !self.revealed_phase => {
                self.concealed = false;
                self.revealed_phase = true;
            }
            LinkVisibilityAction::RevealThenConceal => {}
        }
        self.activated = None;
        self.expiry_activated = false;
        self.skip_first_prompt = false;
        self.skip_first_output = false;
        self.concealed != was_concealed || self.revealed_phase != was_revealed_phase
    }
}

#[derive(Debug, Default)]
pub(crate) struct BufferLinkState {
    shared: Rc<RefCell<LinkProtocolState>>,
    live: HashMap<LinkKey, LiveLink>,
    by_address: HashMap<LinkAddress, LinkKey>,
    by_line: HashMap<usize, Vec<LinkKey>>,
    next_key: u64,
    revealed_spoilers: HashSet<LinkKey>,
    visibility: HashMap<LinkKey, VisibilityState>,
    /// Absolute logical lines removed by an irreversible `wholeline`
    /// concealment. The source line remains in scrollback so absolute numbering
    /// stays stable, but layout, copy, and link enumeration treat it as absent.
    deleted_lines: HashSet<usize>,
    pending_deleted_line_retirement: HashSet<usize>,
    visual_generation: u64,
    previous_output_at: Option<Instant>,
}

#[derive(Debug, Default)]
struct DetachedLinks(Vec<(LinkKey, LiveLink)>);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MatchScore {
    matches: usize,
    distance: usize,
}

impl MatchScore {
    fn with_match(self, distance: usize) -> Self {
        Self {
            matches: self.matches + 1,
            distance: self.distance.saturating_add(distance),
        }
    }

    fn better_than(self, other: Self) -> bool {
        self.matches > other.matches
            || (self.matches == other.matches && self.distance < other.distance)
    }
}

impl BufferLinkState {
    /// The exact matcher stores one score per pair of old/new links. Keep that
    /// scratch space below a small, fixed budget and use the bounded fallback
    /// for server-controlled lines that would exceed it.
    const MAX_EXACT_MATCH_BYTES: usize = 4 * 1024 * 1024;
    const FALLBACK_LOOKAHEAD: usize = 32;

    pub(crate) fn new(shared: Rc<RefCell<LinkProtocolState>>) -> Self {
        Self {
            shared,
            ..Self::default()
        }
    }

    pub(crate) fn replace_line(&mut self, line_number: usize, new: Option<&StyledLine>) {
        if self.deleted_lines.contains(&line_number) {
            let detached = self.detach_line(line_number);
            self.discard_detached(detached);
            return;
        }
        if self.line_matches(line_number, new) {
            return;
        }
        let detached = self.detach_line(line_number);
        self.install_line(line_number, detached, new);
    }

    fn line_matches(&self, line_number: usize, new: Option<&StyledLine>) -> bool {
        let keys = self
            .by_line
            .get(&line_number)
            .map_or(&[][..], Vec::as_slice);
        let links = new.map_or(&[][..], |line| line.links.as_slice());
        keys.len() == links.len()
            && keys.iter().zip(links).all(|(key, link)| {
                self.live.get(key).is_some_and(|live| {
                    live.address == LinkAddress::new(line_number, link)
                        && live.registration.matches(link)
                })
            })
    }

    fn detach_line(&mut self, line_number: usize) -> DetachedLinks {
        let mut detached = self
            .by_line
            .remove(&line_number)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|key| {
                let live = self.live.remove(&key)?;
                self.by_address.remove(&live.address);
                Some((key, live))
            })
            .collect::<Vec<_>>();
        detached.sort_by_key(|(_, live)| live.address.begin);
        DetachedLinks(detached)
    }

    fn install_line(
        &mut self,
        line_number: usize,
        detached: DetachedLinks,
        new: Option<&StyledLine>,
    ) {
        if self.deleted_lines.contains(&line_number) {
            self.discard_detached(detached);
            return;
        }
        let old = detached.0;
        let new = new.map_or_else(Vec::new, |line| {
            line.links
                .iter()
                .map(|link| LiveLink {
                    address: LinkAddress::new(line_number, link),
                    registration: LinkRegistration::new(link),
                })
                .collect()
        });
        let matches = Self::monotonic_matches(&old, &new);
        let mut old_matched = vec![false; old.len()];
        let mut new_to_old = vec![None; new.len()];
        for (old_index, new_index) in matches {
            old_matched[old_index] = true;
            new_to_old[new_index] = Some(old_index);
        }

        let mut installed = Vec::with_capacity(new.len());
        for (new_index, live) in new.into_iter().enumerate() {
            if let Some(old_index) = new_to_old[new_index] {
                installed.push((old[old_index].0, live, false));
            } else {
                let key = LinkKey(self.next_key);
                self.next_key = self
                    .next_key
                    .checked_add(1)
                    .expect("terminal link identity space exhausted");
                installed.push((key, live, true));
            }
        }

        // Retain replacements before releasing their predecessors so a streamed
        // line extension cannot transiently retire and reinitialize a group.
        {
            let mut shared = self.shared.borrow_mut();
            for (_, live, added) in &installed {
                if !added {
                    continue;
                }
                if let Some(selection) = &live.registration.selection {
                    shared.retain_selection(selection);
                }
            }
            for (index, (key, live)) in old.iter().enumerate() {
                if old_matched[index] {
                    continue;
                }
                if let Some(selection) = &live.registration.selection {
                    shared.release_selection(selection);
                }
                self.revealed_spoilers.remove(key);
                self.visibility.remove(key);
            }
        }

        let mut keys = Vec::with_capacity(installed.len());
        for (key, live, added) in installed {
            if added && let Some(visibility) = &live.registration.visibility {
                self.visibility
                    .insert(key, VisibilityState::new(visibility));
            }
            self.by_address.insert(live.address, key);
            self.live.insert(key, live);
            keys.push(key);
        }
        if !keys.is_empty() {
            self.by_line.insert(line_number, keys);
        }
    }

    /// Monotonic matching keeps two identical links from crossing and swapping
    /// state when edits move their byte ranges. Ordinary lines use an exact
    /// maximum-cardinality, minimum-distance match. Pathological lines use a
    /// bounded-lookahead fallback so remote input can never allocate an
    /// old-links by new-links matrix.
    fn monotonic_matches(old: &[(LinkKey, LiveLink)], new: &[LiveLink]) -> Vec<(usize, usize)> {
        if old.is_empty() || new.is_empty() {
            return Vec::new();
        }

        // Exact address/registration pairs at either edge are safe anchors and
        // make the streaming append case linear regardless of line size.
        let mut prefix = 0;
        while prefix < old.len()
            && prefix < new.len()
            && old[prefix].1.address == new[prefix].address
            && old[prefix].1.registration == new[prefix].registration
        {
            prefix += 1;
        }

        let mut suffix = 0;
        while suffix < old.len() - prefix
            && suffix < new.len() - prefix
            && old[old.len() - 1 - suffix].1.address == new[new.len() - 1 - suffix].address
            && old[old.len() - 1 - suffix].1.registration
                == new[new.len() - 1 - suffix].registration
        {
            suffix += 1;
        }

        let old_middle = &old[prefix..old.len() - suffix];
        let new_middle = &new[prefix..new.len() - suffix];
        let mut matches = Vec::with_capacity(old.len().min(new.len()));
        matches.extend((0..prefix).map(|index| (index, index)));

        let middle = (old_middle.len() + 1)
            .checked_mul(new_middle.len() + 1)
            .filter(|cells| {
                cells.saturating_mul(std::mem::size_of::<MatchScore>())
                    <= Self::MAX_EXACT_MATCH_BYTES
            })
            .map_or_else(
                || Self::bounded_monotonic_matches(old_middle, new_middle),
                |_| Self::exact_monotonic_matches(old_middle, new_middle),
            );
        matches.extend(
            middle
                .into_iter()
                .map(|(old_index, new_index)| (old_index + prefix, new_index + prefix)),
        );
        matches.extend(
            (0..suffix).map(|offset| (old.len() - suffix + offset, new.len() - suffix + offset)),
        );
        matches
    }

    fn exact_monotonic_matches(
        old: &[(LinkKey, LiveLink)],
        new: &[LiveLink],
    ) -> Vec<(usize, usize)> {
        if old.is_empty() || new.is_empty() {
            return Vec::new();
        }
        let width = new.len() + 1;
        let mut scores = vec![MatchScore::default(); (old.len() + 1) * width];
        let at = |i: usize, j: usize| i * width + j;

        for i in (0..old.len()).rev() {
            for j in (0..new.len()).rev() {
                let mut best = scores[at(i + 1, j)];
                let skip_new = scores[at(i, j + 1)];
                if skip_new.better_than(best) {
                    best = skip_new;
                }
                if old[i].1.registration == new[j].registration {
                    let matched = scores[at(i + 1, j + 1)]
                        .with_match(old[i].1.address.begin.abs_diff(new[j].address.begin));
                    if matched.better_than(best) {
                        best = matched;
                    }
                }
                scores[at(i, j)] = best;
            }
        }

        let mut matches = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < old.len() && j < new.len() {
            if old[i].1.registration == new[j].registration {
                let matched = scores[at(i + 1, j + 1)]
                    .with_match(old[i].1.address.begin.abs_diff(new[j].address.begin));
                if matched == scores[at(i, j)] {
                    matches.push((i, j));
                    i += 1;
                    j += 1;
                    continue;
                }
            }
            if scores[at(i + 1, j)] == scores[at(i, j)] {
                i += 1;
            } else {
                j += 1;
            }
        }
        matches
    }

    /// Linear-memory fallback for lines too large for exact alignment. It
    /// greedily preserves equal registrations, looking a short distance ahead
    /// to recover from insertions/deletions without permitting quadratic work.
    fn bounded_monotonic_matches(
        old: &[(LinkKey, LiveLink)],
        new: &[LiveLink],
    ) -> Vec<(usize, usize)> {
        let mut matches = Vec::with_capacity(old.len().min(new.len()));
        let (mut old_index, mut new_index) = (0, 0);

        while old_index < old.len() && new_index < new.len() {
            if old[old_index].1.registration == new[new_index].registration {
                matches.push((old_index, new_index));
                old_index += 1;
                new_index += 1;
                continue;
            }

            let old_limit = (old_index + Self::FALLBACK_LOOKAHEAD + 1).min(old.len());
            let new_limit = (new_index + Self::FALLBACK_LOOKAHEAD + 1).min(new.len());
            let skip_old = (old_index + 1..old_limit)
                .find(|&candidate| old[candidate].1.registration == new[new_index].registration);
            let skip_new = (new_index + 1..new_limit)
                .find(|&candidate| old[old_index].1.registration == new[candidate].registration);

            match (skip_old, skip_new) {
                (Some(candidate), Some(other)) => {
                    let old_skip = candidate - old_index;
                    let new_skip = other - new_index;
                    if old_skip < new_skip
                        || (old_skip == new_skip
                            && old[candidate]
                                .1
                                .address
                                .begin
                                .abs_diff(new[new_index].address.begin)
                                <= old[old_index]
                                    .1
                                    .address
                                    .begin
                                    .abs_diff(new[other].address.begin))
                    {
                        old_index = candidate;
                    } else {
                        new_index = other;
                    }
                }
                (Some(candidate), None) => old_index = candidate,
                (None, Some(candidate)) => new_index = candidate,
                (None, None) => {
                    // Neither current registration appears nearby. Retire both
                    // candidates together; this bounds comparisons per link.
                    old_index += 1;
                    new_index += 1;
                }
            }
        }

        matches
    }

    fn remove_line(&mut self, line_number: usize) {
        self.deleted_lines.remove(&line_number);
        self.pending_deleted_line_retirement.remove(&line_number);
        let detached = self.detach_line(line_number);
        self.discard_detached(detached);
    }

    fn discard_detached(&mut self, detached: DetachedLinks) {
        let mut shared = self.shared.borrow_mut();
        for (key, live) in detached.0 {
            if let Some(selection) = &live.registration.selection {
                shared.release_selection(selection);
            }
            self.revealed_spoilers.remove(&key);
            self.visibility.remove(&key);
        }
    }

    fn retire_all(&mut self) {
        if self.live.is_empty() {
            return;
        }
        {
            let mut shared = self.shared.borrow_mut();
            for live in self.live.values() {
                if let Some(selection) = &live.registration.selection {
                    shared.release_selection(selection);
                }
            }
        }
        self.live.clear();
        self.by_address.clear();
        self.by_line.clear();
        self.revealed_spoilers.clear();
        self.visibility.clear();
        self.deleted_lines.clear();
        self.pending_deleted_line_retirement.clear();
    }

    pub(crate) fn shared(&self) -> Rc<RefCell<LinkProtocolState>> {
        Rc::clone(&self.shared)
    }

    pub(crate) fn key_at(&self, line_number: usize, link: &LinkSpan) -> Option<LinkKey> {
        self.by_address
            .get(&LinkAddress::new(line_number, link))
            .copied()
    }

    fn address(&self, key: LinkKey) -> Option<LinkAddress> {
        self.live.get(&key).map(|live| live.address)
    }

    pub(crate) fn line(&self, key: LinkKey) -> Option<usize> {
        self.address(key).map(|address| address.line)
    }

    pub(crate) fn position(&self, key: LinkKey) -> Option<(usize, usize, usize)> {
        self.address(key)
            .map(|address| (address.line, address.begin, address.end))
    }

    fn keys(&self) -> Vec<LinkKey> {
        let mut links: Vec<_> = self
            .live
            .iter()
            .filter(|(_, live)| !self.line_concealed(live.address.line))
            .map(|(key, live)| (live.address, *key))
            .collect();
        links.sort_by_key(|(address, _)| (address.line, address.begin, address.end));
        links.into_iter().map(|(_, key)| key).collect()
    }

    pub(crate) fn contains(&self, key: LinkKey) -> bool {
        self.live.contains_key(&key)
    }

    pub(crate) fn spoiler_revealed(&self, key: LinkKey) -> bool {
        self.revealed_spoilers.contains(&key)
    }

    pub(crate) fn reveal_spoiler(&mut self, key: LinkKey) -> bool {
        if self
            .live
            .get(&key)
            .is_some_and(|link| link.registration.spoiler)
            && self.revealed_spoilers.insert(key)
        {
            self.visual_generation = self.visual_generation.wrapping_add(1);
            true
        } else {
            false
        }
    }

    pub(crate) fn concealed(&self, key: LinkKey) -> bool {
        self.visibility
            .get(&key)
            .is_some_and(|visibility| visibility.concealed)
    }

    pub(crate) fn line_concealed(&self, line_number: usize) -> bool {
        self.deleted_lines.contains(&line_number)
            || self
                .by_line
                .get(&line_number)
                .into_iter()
                .flatten()
                .any(|key| {
                    self.visibility.get(key).is_some_and(|visibility| {
                        visibility.concealed && visibility.config.whole_line
                    })
                })
    }

    /// Release every registration on a line after an irreversible whole-line
    /// concealment. The tombstone remains so the logical row cannot reappear or
    /// expose co-located links during later interaction passes.
    pub(crate) fn retire_deleted_line_registrations(&mut self) {
        let lines: Vec<_> = self.pending_deleted_line_retirement.drain().collect();
        for line_number in lines {
            let detached = self.detach_line(line_number);
            self.discard_detached(detached);
        }
    }

    fn mark_line_deleted(&mut self, line_number: usize) {
        self.deleted_lines.insert(line_number);
        self.pending_deleted_line_retirement.insert(line_number);
    }

    pub(crate) fn activate_visibility(&mut self, key: LinkKey, now: Instant) -> Option<Instant> {
        let state = self.visibility.get_mut(&key)?;
        let deadline = match state.config.action {
            LinkVisibilityAction::Conceal if !state.concealed => {
                if let Some(delay_ms) = state.config.delay_ms.filter(|delay_ms| *delay_ms > 0) {
                    state.activated = Some(now);
                    Some(now + Duration::from_millis(delay_ms))
                } else if state.has_expiry_trigger() {
                    // Expiry is click-armed. Prompt/output skip the first response
                    // event so a command link does not immediately expire on the
                    // output and prompt caused by its own activation.
                    state.activate_expiry();
                    None
                } else {
                    state.concealed = true;
                    state.activated = None;
                    None
                }
            }
            LinkVisibilityAction::RevealThenConceal if state.revealed_phase => {
                state.concealed = true;
                state.revealed_phase = false;
                state.activated = None;
                None
            }
            _ => None,
        };
        let delete_whole_line = state.concealed
            && state.config.whole_line
            && matches!(
                state.config.action,
                LinkVisibilityAction::Conceal | LinkVisibilityAction::RevealThenConceal
            );
        if delete_whole_line
            && let Some(line_number) = self.live.get(&key).map(|live| live.address.line)
        {
            self.mark_line_deleted(line_number);
        }
        self.visual_generation = self.visual_generation.wrapping_add(1);
        deadline
    }

    pub(crate) fn update_visibility_timers(&mut self, now: Instant) -> (Option<Instant>, bool) {
        let mut changed = false;
        let mut next = None;
        let mut deleted_keys = Vec::new();
        for (key, state) in &mut self.visibility {
            let deadline = match state.config.action {
                LinkVisibilityAction::Conceal => state
                    .activated
                    .zip(state.config.delay_ms)
                    .map(|(at, delay_ms)| at + Duration::from_millis(delay_ms)),
                LinkVisibilityAction::Reveal | LinkVisibilityAction::RevealThenConceal
                    if state.concealed =>
                {
                    state
                        .config
                        .delay_ms
                        .map(|delay_ms| state.created + Duration::from_millis(delay_ms))
                }
                _ => None,
            };
            if let Some(deadline) = deadline {
                if now >= deadline {
                    state.concealed = matches!(state.config.action, LinkVisibilityAction::Conceal);
                    state.revealed_phase = !state.concealed;
                    state.activated = None;
                    changed = true;
                    if state.concealed
                        && state.config.whole_line
                        && matches!(state.config.action, LinkVisibilityAction::Conceal)
                    {
                        deleted_keys.push(*key);
                    }
                } else {
                    next = Some(next.map_or(deadline, |current: Instant| current.min(deadline)));
                }
            }
        }
        let deleted_lines: Vec<_> = deleted_keys
            .into_iter()
            .filter_map(|key| self.live.get(&key).map(|live| live.address.line))
            .collect();
        for line_number in deleted_lines {
            self.mark_line_deleted(line_number);
        }
        if changed {
            self.visual_generation = self.visual_generation.wrapping_add(1);
        }
        (next, changed)
    }

    fn note_input(&mut self) {
        self.apply_expiry(|visibility| {
            visibility.expiry_activated && visibility.config.expire.input
        });
    }

    fn note_prompt(&mut self) {
        self.apply_expiry(|visibility| {
            if !visibility.expiry_activated || !visibility.config.expire.prompt {
                return false;
            }
            if visibility.skip_first_prompt {
                visibility.skip_first_prompt = false;
                return false;
            }
            true
        });
    }

    fn note_output(&mut self, now: Instant) {
        let previous = self.previous_output_at.replace(now);
        let Some(previous) = previous else {
            return;
        };
        self.apply_expiry(|visibility| {
            if !visibility.expiry_activated || !visibility.config.expire.output {
                return false;
            }
            if now.saturating_duration_since(previous)
                < Duration::from_millis(visibility.config.expire.output_delay_ms)
            {
                return false;
            }
            if visibility.skip_first_output {
                visibility.skip_first_output = false;
                return false;
            }
            true
        });
    }

    fn apply_expiry(&mut self, mut trigger: impl FnMut(&mut VisibilityState) -> bool) {
        if self.visibility.is_empty() {
            return;
        }
        let mut changed = false;
        let mut deleted_keys = Vec::new();
        for (key, visibility) in &mut self.visibility {
            if trigger(visibility) {
                let state_changed = visibility.apply_expiry();
                changed |= state_changed;
                if state_changed
                    && visibility.concealed
                    && visibility.config.whole_line
                    && matches!(visibility.config.action, LinkVisibilityAction::Conceal)
                {
                    deleted_keys.push(*key);
                }
            }
        }
        let deleted_lines: Vec<_> = deleted_keys
            .into_iter()
            .filter_map(|key| self.live.get(&key).map(|live| live.address.line))
            .collect();
        for line_number in deleted_lines {
            self.mark_line_deleted(line_number);
        }
        if changed {
            self.visual_generation = self.visual_generation.wrapping_add(1);
        }
    }

    pub(crate) fn visual_generation(&self) -> u64 {
        self.visual_generation
    }
}

/// The chip fill behind a link: a nearly-transparent wash of the text's own
/// foreground. The alpha matches the Markdown widget's link chip, so a link whose
/// foreground is the Markdown link color renders identically to a Markdown link.
const LINK_WASH_ALPHA: f32 = 0.14;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkRenderStyle {
    pub authored: bool,
    pub style: LinkTextStyle,
    pub spoiler_concealed: bool,
    pub hidden: bool,
}

impl LinkRenderStyle {
    fn base(link: &LinkSpan) -> Self {
        Self {
            authored: link.style.is_some(),
            style: link
                .style
                .as_ref()
                .map_or_else(LinkTextStyle::default, |style| style.base.clone()),
            spoiler_concealed: false,
            hidden: false,
        }
    }
}

/// Bidirectional byte-offset mapping between the immutable source line and the
/// text actually shaped by the renderer. Most lines are identity-mapped;
/// concealed link content needs a real map because one grapheme becomes one space.
#[derive(Debug, Clone, Default)]
pub(crate) enum RenderedOffsets {
    #[default]
    Identity,
    Mapped {
        identity_prefix: usize,
        source: Rc<[usize]>,
        rendered: Rc<[usize]>,
    },
}

impl RenderedOffsets {
    fn map(offset: usize, from: &[usize], to: &[usize]) -> usize {
        if offset == usize::MAX {
            return usize::MAX;
        }
        match from.binary_search(&offset) {
            Ok(index) => to[index],
            Err(0) => 0,
            Err(index) if index == from.len() => *to.last().unwrap_or(&0),
            Err(index) => {
                let from_start = from[index - 1];
                let from_end = from[index];
                let to_start = to[index - 1];
                let to_end = to[index];
                if from_end - from_start == to_end - to_start {
                    to_start + offset - from_start
                } else {
                    // Selection/link offsets should land on grapheme boundaries.
                    // If an external edit leaves one inside a concealed cluster,
                    // snap to its beginning rather than exposing partial text.
                    to_start
                }
            }
        }
    }

    pub(crate) fn source_to_rendered(&self, offset: usize) -> usize {
        match self {
            Self::Identity => offset,
            Self::Mapped {
                identity_prefix,
                source,
                rendered,
            } => {
                if offset <= *identity_prefix {
                    offset
                } else {
                    Self::map(offset, source, rendered)
                }
            }
        }
    }

    pub(crate) fn rendered_to_source(&self, offset: usize) -> usize {
        match self {
            Self::Identity => offset,
            Self::Mapped {
                identity_prefix,
                source,
                rendered,
            } => {
                if offset <= *identity_prefix {
                    offset
                } else {
                    Self::map(offset, rendered, source)
                }
            }
        }
    }

    pub(crate) fn map_selection(
        &self,
        selection: Option<(usize, usize)>,
    ) -> Option<(usize, usize)> {
        selection.map(|(from, to)| (self.source_to_rendered(from), self.source_to_rendered(to)))
    }
}

#[derive(Debug)]
pub(crate) struct RenderedSpans {
    pub(crate) spans: Rc<Vec<Span<'static, Link>>>,
    pub(crate) offsets: RenderedOffsets,
}

#[derive(Debug)]
enum RenderedOffsetsBuilder {
    Identity {
        end: usize,
    },
    Mapped {
        identity_prefix: usize,
        source: Vec<usize>,
        rendered: Vec<usize>,
        source_end: usize,
        rendered_end: usize,
    },
}

impl Default for RenderedOffsetsBuilder {
    fn default() -> Self {
        Self::Identity { end: 0 }
    }
}

impl RenderedOffsetsBuilder {
    fn push(&mut self, source: &str, rendered: &str, rewritten: bool) {
        if let Self::Identity { end } = self {
            if !rewritten {
                debug_assert_eq!(source, rendered);
                *end += source.len();
                return;
            }

            let identity_prefix = *end;
            *self = Self::Mapped {
                identity_prefix,
                source: vec![identity_prefix],
                rendered: vec![identity_prefix],
                source_end: identity_prefix,
                rendered_end: identity_prefix,
            };
        }

        let Self::Mapped {
            source: source_offsets,
            rendered: rendered_offsets,
            source_end,
            rendered_end,
            ..
        } = self
        else {
            unreachable!("identity spans return before offset mapping")
        };

        let mut rendered_graphemes = rendered.graphemes(true);
        for source_grapheme in source.graphemes(true) {
            let rendered_grapheme = rendered_graphemes
                .next()
                .expect("rendered terminal text preserves grapheme count");
            *source_end += source_grapheme.len();
            *rendered_end += rendered_grapheme.len();
            source_offsets.push(*source_end);
            rendered_offsets.push(*rendered_end);
        }
        debug_assert!(rendered_graphemes.next().is_none());
    }

    fn finish(self) -> RenderedOffsets {
        match self {
            Self::Identity { .. } => RenderedOffsets::Identity,
            Self::Mapped {
                identity_prefix,
                source,
                rendered,
                ..
            } => {
                if source == rendered {
                    RenderedOffsets::Identity
                } else {
                    RenderedOffsets::Mapped {
                        identity_prefix,
                        source: source.into(),
                        rendered: rendered.into(),
                    }
                }
            }
        }
    }
}

struct RenderedSpansBuilder<'a> {
    spans: Vec<Span<'static, Link>>,
    offsets: RenderedOffsetsBuilder,
    prefs: &'a TerminalPrefs,
    line_hidden: bool,
}

impl<'a> RenderedSpansBuilder<'a> {
    fn new(capacity: usize, prefs: &'a TerminalPrefs, line_hidden: bool) -> Self {
        Self {
            spans: Vec::with_capacity(capacity),
            offsets: RenderedOffsetsBuilder::default(),
            prefs,
            line_hidden,
        }
    }

    fn push(
        &mut self,
        text: &str,
        style: Style,
        linked: bool,
        link_style: Option<&LinkRenderStyle>,
    ) {
        let (span, rewritten) = make_resolved_span_with_rewrite(
            text,
            style,
            linked,
            link_style,
            self.line_hidden,
            self.prefs,
        );
        self.offsets.push(text, span.text.as_ref(), rewritten);
        self.spans.push(span);
    }

    fn finish(self) -> RenderedSpans {
        RenderedSpans {
            spans: Rc::new(self.spans),
            offsets: self.offsets.finish(),
        }
    }
}

/// One renderable segment: underlined over the foreground wash when linked (unless
/// the span sets an explicit background, which wins — the underline stays).
/// "Explicit" is judged by the resolved color model: `bg: "default"` normalizes to
/// `DefaultBackground` at the op boundary and so still washes, while a background
/// literally painted the theme's background color counts as explicit and doesn't.
#[inline]
pub(crate) fn make_span(
    text: &str,
    style: Style,
    linked: bool,
    link_style: Option<&LinkStyle>,
    prefs: &TerminalPrefs,
) -> Span<'static, Link> {
    let resolved = link_style.map(|style| LinkRenderStyle {
        authored: true,
        style: style.base.clone(),
        spoiler_concealed: false,
        hidden: false,
    });
    make_resolved_span(text, style, linked, resolved.as_ref(), false, prefs)
}

#[inline]
fn make_resolved_span(
    text: &str,
    style: Style,
    linked: bool,
    link_style: Option<&LinkRenderStyle>,
    line_hidden: bool,
    prefs: &TerminalPrefs,
) -> Span<'static, Link> {
    make_resolved_span_with_rewrite(text, style, linked, link_style, line_hidden, prefs).0
}

#[inline]
fn make_resolved_span_with_rewrite(
    text: &str,
    style: Style,
    linked: bool,
    link_style: Option<&LinkRenderStyle>,
    line_hidden: bool,
    prefs: &TerminalPrefs,
) -> (Span<'static, Link>, bool) {
    let mut attributes = style.attributes;
    let authored = link_style.map(|style| &style.style);
    let sgr_bold = attributes.bold;
    let authored_bold = authored.and_then(|style| style.bold);
    // OSC-authored bold is literal styling. The preference only controls how
    // an SGR bold attribute is presented, and an authored `bold: false`
    // suppresses both of that SGR attribute's visual effects.
    let bold_weight =
        authored_bold.unwrap_or_else(|| sgr_bold && prefs.bold_mode.uses_bold_weight());
    let bold_brightness =
        sgr_bold && authored_bold != Some(false) && prefs.bold_mode.uses_bright_palette();
    if let Some(value) = authored.and_then(|style| style.italic) {
        attributes.italic = value;
    }
    let logical_fg = if bold_brightness {
        match style.fg {
            Color::Ansi { color, bold: false } => Color::Ansi { color, bold: true },
            Color::DefaultForeground { bold: false } => Color::DefaultForeground { bold: true },
            other => other,
        }
    } else {
        style.fg
    };
    let terminal_color = |color| match color {
        Color::DefaultBackground => prefs.palette.background,
        other => prefs.resolve(other),
    };
    let logical_fg = authored
        .and_then(|style| style.foreground)
        .map_or_else(|| terminal_color(logical_fg), authored_color);
    let logical_bg = authored
        .and_then(|style| style.background)
        .map(authored_color)
        .or_else(|| (style.bg != Color::DefaultBackground).then(|| terminal_color(style.bg)));
    let (mut fg, mut background) = if attributes.reverse {
        (
            logical_bg.unwrap_or(prefs.palette.background),
            Some(logical_fg),
        )
    } else {
        (logical_fg, logical_bg)
    };
    if attributes.faint {
        fg.a *= 0.5;
    }
    let mut font = prefs.font;
    if bold_weight {
        font.weight = iced::font::Weight::Bold;
    }
    if attributes.italic {
        font.style = iced::font::Style::Italic;
    }
    let sgr_underline = match attributes.underline {
        Underline::None => LinkDecoration::None,
        Underline::Single => LinkDecoration::Solid,
        Underline::Double => LinkDecoration::Double,
    };
    let mut underline = authored
        .and_then(|style| style.underline)
        .unwrap_or(sgr_underline);
    if linked
        && !link_style.is_some_and(|style| style.authored)
        && underline == LinkDecoration::None
    {
        underline = LinkDecoration::Solid;
    }
    let overline = authored
        .and_then(|style| style.overline)
        .unwrap_or(LinkDecoration::None);
    let strikethrough =
        authored
            .and_then(|style| style.strikethrough)
            .unwrap_or(if attributes.crossed_out {
                LinkDecoration::Solid
            } else {
                LinkDecoration::None
            });
    let decoration_color = authored
        .and_then(|style| style.decoration_color)
        .map(authored_color);
    let hidden = line_hidden || link_style.is_some_and(|style| style.hidden);
    let spoiler_concealed = link_style.is_some_and(|style| style.spoiler_concealed);
    if spoiler_concealed {
        underline = LinkDecoration::None;
    }
    if hidden {
        fg = iced::Color::TRANSPARENT;
        background = None;
        underline = LinkDecoration::None;
    }
    let overline = if hidden || spoiler_concealed {
        LinkDecoration::None
    } else {
        overline
    };
    let strikethrough = if hidden || spoiler_concealed {
        LinkDecoration::None
    } else {
        strikethrough
    };
    let decoration_color = (!hidden && !spoiler_concealed)
        .then_some(decoration_color)
        .flatten();
    // Color emoji glyphs do not necessarily honor a span foreground, so
    // painting foreground and background alike cannot reliably conceal them.
    // Shape one ordinary space per grapheme instead; reveal re-bakes the
    // original text, while the offset map keeps selection tied to the
    // immutable source line.
    let rewritten = hidden || spoiler_concealed;
    let rendered_text = if rewritten {
        text.graphemes(true).map(|_| ' ').collect()
    } else {
        text.to_string()
    };
    let mut span = Span::<'static, Link>::new(Cow::Owned(rendered_text))
        .color(fg)
        .font(font)
        .underline(underline != LinkDecoration::None)
        .strikethrough(strikethrough != LinkDecoration::None);
    // Only a meaningful background sets the span highlight: the widget draws a
    // quad per highlighted span region, so the (overwhelmingly common) default
    // background must stay decoration-free rather than painting a quad of the
    // pane's own color under every span.
    if linked
        && !link_style.is_some_and(|style| style.authored)
        && background.is_none()
        && !hidden
        && !spoiler_concealed
    {
        span = span.background(Background::Color(iced::Color {
            a: LINK_WASH_ALPHA,
            ..fg
        }));
    } else if let Some(bg) = background {
        span = span.background(Background::Color(bg));
    }
    let metadata = SpanMetadata {
        blink: attributes.blink,
        underline,
        overline,
        strikethrough,
        decoration_color,
    };
    let span = if metadata == SpanMetadata::default() {
        span
    } else {
        span.link(metadata)
    };
    (span, rewritten)
}

/// Bakes a styled line's semantic colors into renderable spans using the
/// given palette. Style spans are split at link boundaries so linked ranges get
/// the link affordance without disturbing the line's own colors.
#[inline]
fn to_spans(styled_line: &Arc<StyledLine>, prefs: &TerminalPrefs) -> Rc<Vec<Span<'static, Link>>> {
    to_spans_with(styled_line, prefs, false, LinkRenderStyle::base).spans
}

fn to_spans_with(
    styled_line: &Arc<StyledLine>,
    prefs: &TerminalPrefs,
    line_hidden: bool,
    resolve: impl Fn(&LinkSpan) -> LinkRenderStyle,
) -> RenderedSpans {
    let mut rendered = RenderedSpansBuilder::new(styled_line.spans.len(), prefs, line_hidden);
    for span_info in &styled_line.spans {
        let (begin, end) = (span_info.begin_pos, span_info.end_pos);
        if styled_line.links.is_empty() || begin == end {
            rendered.push(&styled_line.text[begin..end], span_info.style, false, None);
            continue;
        }
        // Links are sorted and non-overlapping; walk the ones intersecting this span,
        // alternating plain and linked segments.
        let mut cursor = begin;
        for link in &styled_line.links {
            if link.end_pos <= cursor {
                continue;
            }
            if link.begin_pos >= end {
                break;
            }
            let linked_begin = link.begin_pos.max(cursor);
            if linked_begin > cursor {
                rendered.push(
                    &styled_line.text[cursor..linked_begin],
                    span_info.style,
                    false,
                    None,
                );
            }
            let linked_end = link.end_pos.min(end);
            let resolved = resolve(link);
            rendered.push(
                &styled_line.text[linked_begin..linked_end],
                span_info.style,
                true,
                Some(&resolved),
            );
            cursor = linked_end;
        }
        if cursor < end {
            rendered.push(&styled_line.text[cursor..end], span_info.style, false, None);
        }
    }
    rendered.finish()
}

/// Clamp a byte offset to `text`'s length and snap it down to the nearest char
/// boundary, yielding an offset that is always safe to slice `text` at.
#[inline]
fn clamp_to_char_boundary(text: &str, mut col: usize) -> usize {
    if col >= text.len() {
        return text.len();
    }
    while col > 0 && !text.is_char_boundary(col) {
        col -= 1;
    }
    col
}

#[inline]
fn strip_possessive_suffix(word: &str) -> &str {
    if let Some(stripped) = word.strip_suffix("'s") {
        stripped
    } else if let Some(stripped) = word.strip_suffix("'S") {
        stripped
    } else if let Some(stripped) = word.strip_suffix("’s") {
        stripped
    } else if let Some(stripped) = word.strip_suffix("’S") {
        stripped
    } else {
        word
    }
}

impl AsRef<[Span<'static, SpanMetadata>]> for BufferLine {
    fn as_ref(&self) -> &[Span<'static, SpanMetadata>] {
        self.spans().as_slice()
    }
}

#[derive(Debug, Clone)]
pub struct BufferLine {
    pub styled_line: Arc<StyledLine>,
    /// Renderable spans, baked from `styled_line` on first access. Lazy so a
    /// line that scrolls through the buffer unseen (a burst larger than the
    /// window, scrollback eviction) never pays `to_spans` at all; only lines
    /// the pane actually lays out are baked. Cleared — not eagerly rebaked —
    /// on palette changes and line edits.
    spans: std::cell::OnceCell<Rc<Vec<Span<'static, SpanMetadata>>>>,
}

/// One case-insensitive text match in the terminal's absolute line/column
/// coordinate space. Columns are UTF-8 byte offsets, matching [`Selection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalTextMatch {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

impl PartialEq for BufferLine {
    fn eq(&self, other: &Self) -> bool {
        self.styled_line == other.styled_line
    }
}

impl From<Arc<StyledLine>> for BufferLine {
    fn from(styled_line: Arc<StyledLine>) -> Self {
        Self {
            spans: std::cell::OnceCell::new(),
            styled_line,
        }
    }
}

impl BufferLine {
    /// The line's renderable spans, baking them against the current palette on
    /// first access. The returned `Rc` is pointer-stable until the spans are
    /// invalidated (palette change, line edit) — the pane's paragraph cache
    /// keys on that identity.
    pub fn spans(&self) -> &Rc<Vec<Span<'static, SpanMetadata>>> {
        self.spans.get_or_init(|| {
            let prefs = crate::prefs::current();
            to_spans(&self.styled_line, &prefs)
        })
    }

    pub(crate) fn spans_with_link_state(
        &self,
        prefs: &TerminalPrefs,
        line_hidden: bool,
        resolve: impl Fn(&LinkSpan) -> LinkRenderStyle,
    ) -> RenderedSpans {
        to_spans_with(&self.styled_line, prefs, line_hidden, resolve)
    }

    pub(crate) fn rendered_spans(&self) -> RenderedSpans {
        RenderedSpans {
            spans: self.spans().clone(),
            offsets: RenderedOffsets::Identity,
        }
    }

    /// Drop the baked spans so the next access re-bakes them (and downstream
    /// paragraph caches, keyed on the `Rc` identity, naturally miss).
    fn invalidate_spans(&mut self) {
        self.spans.take();
    }
}

#[derive(Debug)]
pub struct TerminalBuffer {
    lines: VecDeque<BufferLine>,
    max_lines: NonZeroUsize,
    line_terminated: bool,
    last_line_number: usize,
    /// The prefs generation the lines' spans were baked with; see
    /// [`Self::refresh_styles`].
    span_generation: u64,
    /// How many held lines carry link spans, maintained at every structural
    /// mutation — so the per-frame hover path can skip hit testing entirely on
    /// the (overwhelmingly common) linkless buffer via [`Self::has_links`].
    lines_with_links: usize,
    /// Bumped whenever keyboard focus returns to this pane's command editor.
    /// Terminal widget instances observe the epoch and drop their independent
    /// link-navigation focus before processing further keyboard input.
    link_navigation_reset_epoch: u64,
    link_state: Rc<RefCell<BufferLinkState>>,
    /// Link-instance state detached by an explicit core-authored carriage-
    /// return replacement. It survives UI batch boundaries and unrelated
    /// trigger output until the matching finish update arrives.
    open_line_replacement: Option<DetachedLinks>,
}

impl Default for TerminalBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalBuffer {
    /// Creates a new, empty `TerminalBuffer` with a default line limit (e.g., 10,000 lines).
    /// The internal buffer is pre-allocated to this default limit.
    pub fn new() -> Self {
        const DEFAULT_MAX_LINES: usize = 10_000;
        let max_lines =
            NonZeroUsize::new(DEFAULT_MAX_LINES).expect("Default max lines is non-zero");
        Self::new_with_max_lines(max_lines)
    }

    /// Creates a new `TerminalBuffer` with a specified maximum number of lines.
    ///
    /// # Arguments
    ///
    /// * `max_lines`: The maximum number of lines the buffer can hold. Must be non-zero.
    ///   The internal `VecDeque` will be initialized with this capacity.
    pub fn new_with_max_lines(max_lines: NonZeroUsize) -> Self {
        Self::new_with_protocol_state(
            max_lines,
            Rc::new(RefCell::new(LinkProtocolState::default())),
        )
    }

    pub(crate) fn new_with_protocol_state(
        max_lines: NonZeroUsize,
        protocol_state: Rc<RefCell<LinkProtocolState>>,
    ) -> Self {
        Self {
            lines: VecDeque::with_capacity(max_lines.get()),
            max_lines,
            line_terminated: false,
            last_line_number: 0,
            span_generation: crate::prefs::current().generation,
            lines_with_links: 0,
            link_navigation_reset_epoch: 0,
            link_state: Rc::new(RefCell::new(BufferLinkState::new(protocol_state))),
            open_line_replacement: None,
        }
    }

    pub fn note_visibility_input(&self) {
        self.link_state.borrow_mut().note_input();
    }

    pub fn note_command_input_focus(&mut self) {
        self.link_navigation_reset_epoch = self.link_navigation_reset_epoch.wrapping_add(1);
    }

    pub(crate) fn link_navigation_reset_epoch(&self) -> u64 {
        self.link_navigation_reset_epoch
    }

    pub fn note_visibility_prompt(&self) {
        self.link_state.borrow_mut().note_prompt();
    }

    pub fn note_visibility_output(&self) {
        self.link_state.borrow_mut().note_output(Instant::now());
    }

    pub(crate) fn link_state(&self) -> Rc<RefCell<BufferLinkState>> {
        Rc::clone(&self.link_state)
    }

    pub(crate) fn link_protocol_state(&self) -> Rc<RefCell<LinkProtocolState>> {
        self.link_state.borrow().shared()
    }

    /// Whether any held line carries a link span. O(1); the per-frame hover
    /// path uses it to skip hit testing on linkless buffers.
    pub fn has_links(&self) -> bool {
        self.lines_with_links > 0
    }

    /// Account for `line` entering the buffer (call beside every push).
    fn note_added(&mut self, line_number: usize, line: &BufferLine) {
        if line.styled_line.links.is_empty() {
            return;
        }
        self.lines_with_links += 1;
        self.link_state
            .borrow_mut()
            .replace_line(line_number, Some(&line.styled_line));
    }

    /// Account for `line` leaving the buffer (call on every pop).
    fn note_removed(&mut self, line_number: usize, line: &BufferLine) {
        if line.styled_line.links.is_empty() {
            return;
        }
        self.lines_with_links -= 1;
        self.link_state.borrow_mut().remove_line(line_number);
    }

    /// Pop the oldest line, keeping the link accounting straight.
    fn evict_front(&mut self) {
        let line_number = self.last_line_number - self.lines.len() + 1;
        if let Some(line) = self.lines.pop_front() {
            self.note_removed(line_number, &line);
        }
    }

    /// Changes the scrollback limit, trimming the oldest lines if the buffer
    /// already exceeds it.
    pub fn set_max_lines(&mut self, max_lines: NonZeroUsize) {
        self.max_lines = max_lines;
        while self.lines.len() > max_lines.get() {
            self.evict_front();
        }
    }

    /// Invalidates every line's baked spans if the preferences changed since
    /// they were built (palette swaps, etc.), so visible lines re-bake against
    /// the new palette on their next layout — and never-shown scrollback pays
    /// nothing. Dropping the span `Rc`s naturally invalidates downstream
    /// paragraph caches. Cheap one-off per settings change; a no-op otherwise.
    pub fn refresh_styles(&mut self) {
        let prefs = crate::prefs::current();
        if prefs.generation == self.span_generation {
            return;
        }

        for line in &mut self.lines {
            line.invalidate_spans();
        }

        self.span_generation = prefs.generation;
    }

    pub fn commit_current_line(&mut self) {
        self.line_terminated = true;
    }

    pub fn extend_line(&mut self, line_in: Arc<StyledLine>) {
        if self.line_terminated {
            self.line_terminated = false;

            while self.lines.len() > (self.max_lines.get() - 1) {
                self.evict_front();
            }

            self.last_line_number += 1;
            let line: BufferLine = line_in.into();
            self.note_added(self.last_line_number, &line);
            self.lines.push_back(line);
        } else if line_in.is_blank_fragment() && !self.lines.is_empty() {
            // Nothing to glue: a bare prompt boundary's empty flush, or a
            // line end arriving on an open prompt row. The row stays open
            // and untouched — and, on a server that ends every line in a
            // prompt boundary, the copy this would otherwise make per line
            // is skipped. (An empty buffer has no open row to leave alone:
            // the fragment opens row 1 below, blank or not.)
        } else {
            match self.lines.pop_back() {
                Some(line) => {
                    // An open row that is itself blank (a bare prompt
                    // boundary opened it) takes the new fragment's `Arc`
                    // outright rather than copying it into a join.
                    let joined: BufferLine = if line.styled_line.is_blank_fragment() {
                        line_in.into()
                    } else {
                        Arc::new(line.styled_line.append(&line_in)).into()
                    };
                    let had_links = !line.styled_line.links.is_empty();
                    let has_links = !joined.styled_line.links.is_empty();
                    if has_links && !had_links {
                        self.lines_with_links += 1;
                    } else if !has_links && had_links {
                        self.lines_with_links -= 1;
                    }
                    if had_links || has_links {
                        self.link_state
                            .borrow_mut()
                            .replace_line(self.last_line_number, Some(&joined.styled_line));
                    }
                    self.lines.push_back(joined);
                }
                None => {
                    self.last_line_number += 1;
                    let line: BufferLine = line_in.into();
                    self.note_added(self.last_line_number, &line);
                    self.lines.push_back(line);
                }
            }
        }
    }

    /// Start a producer-identified carriage-return replacement. The retired
    /// line's link state is detached rather than released, so the matching
    /// finish update can restore it even after a UI flush or intervening output.
    pub fn begin_open_line_replacement(&mut self) {
        if self.open_line_replacement.is_some() {
            return;
        }
        if self.line_terminated {
            self.open_line_replacement = Some(DetachedLinks::default());
            return;
        }
        let Some(old) = self.lines.pop_back() else {
            self.open_line_replacement = Some(DetachedLinks::default());
            return;
        };

        let detached = if old.styled_line.links.is_empty() {
            DetachedLinks::default()
        } else {
            self.lines_with_links -= 1;
            self.link_state
                .borrow_mut()
                .detach_line(self.last_line_number)
        };
        self.last_line_number -= 1;
        self.line_terminated = true;
        self.open_line_replacement = Some(detached);
    }

    /// Finish a producer-identified carriage-return replacement. `None`
    /// retires the preserved state because routing removed the replacement
    /// from main; `Some` installs a fresh open line and order-preservingly
    /// remaps matching link instances onto it.
    pub fn finish_open_line_replacement(&mut self, replacement: Option<Arc<StyledLine>>) {
        let Some(detached) = self.open_line_replacement.take() else {
            if let Some(replacement) = replacement {
                self.extend_line(replacement);
            }
            return;
        };
        let Some(replacement) = replacement else {
            self.link_state.borrow_mut().discard_detached(detached);
            return;
        };

        while self.lines.len() > (self.max_lines.get() - 1) {
            self.evict_front();
        }
        self.last_line_number += 1;
        let replacement: BufferLine = replacement.into();
        if !replacement.styled_line.links.is_empty() {
            self.lines_with_links += 1;
        }
        self.link_state.borrow_mut().install_line(
            self.last_line_number,
            detached,
            Some(&replacement.styled_line),
        );
        self.lines.push_back(replacement);
        self.line_terminated = false;
    }

    /// Adds a line to the buffer.
    /// If the buffer is at its `max_lines` capacity, the oldest line is removed.
    // Buffer-manipulation helper; exercised by tests and kept as part of the
    // buffer's coherent line API (the live path uses `extend_line`).
    #[allow(dead_code)]
    pub fn push_line(&mut self, line: Arc<StyledLine>) {
        let limit = self.max_lines.get();

        // Remove oldest lines if the buffer is at or would exceed the limit.
        // We want lines.len() to be at most limit - 1 before push_back,
        // so that after push_back, lines.len() is at most limit.
        while self.lines.len() >= limit {
            self.evict_front();
        }
        self.last_line_number += 1;
        let line: BufferLine = line.into();
        self.note_added(self.last_line_number, &line);
        self.lines.push_back(line);
        self.line_terminated = true;
    }

    /// Returns a reverse iterator over the lines in the buffer.
    /// This allows iterating from the most recently added line to the oldest.
    // Part of the buffer's iteration API; kept alongside `iter_rev_with_offset`.
    #[allow(dead_code)]
    pub fn iter_rev(
        &self,
    ) -> impl DoubleEndedIterator<Item = &BufferLine> + ExactSizeIterator<Item = &BufferLine> {
        self.lines.iter().rev()
    }

    pub fn iter_rev_with_line_number(
        &self,
        last_line_number: Option<usize>,
    ) -> impl Iterator<Item = (usize, &BufferLine)> {
        let buffer_last_line_number = self.last_line_number;
        let to_skip = buffer_last_line_number - last_line_number.unwrap_or(buffer_last_line_number);
        self.lines
            .iter()
            .rev()
            .skip(to_skip)
            .zip(to_skip..)
            .map(move |(line, i)| (buffer_last_line_number - i, line))
    }

    /// Returns an iterator over the lines in the buffer, starting from an offset from the end and iterating in reverse.
    ///
    /// # Arguments
    ///
    /// * `offset`: The number of lines to skip from the end before starting reverse iteration.
    ///   An offset of 0 is equivalent to `iter_rev()`.
    // Part of the buffer's iteration API; kept for scrollback-offset rendering.
    #[allow(dead_code)]
    pub fn iter_rev_with_offset(
        &self,
        offset: usize,
    ) -> impl DoubleEndedIterator<Item = &BufferLine> + ExactSizeIterator<Item = &BufferLine> {
        self.lines.iter().rev().skip(offset)
    }

    /// Returns the current number of lines in the buffer.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Returns `true` if the buffer contains no lines.
    // Kept as the conventional companion to `len()`.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn last_line_number(&self) -> usize {
        self.last_line_number
    }

    /// Find every visible whole-line text match, newest first. Matches on the
    /// same line are returned right-to-left so the initial result is the
    /// terminal's most recent occurrence in both dimensions.
    ///
    /// This runs on the UI thread against the whole scrollback on every search
    /// keystroke, so the scan reuses its working buffers across lines and
    /// takes an allocation-free lowercase path for all-ASCII lines.
    pub(crate) fn find_text_matches(&self, query: &str) -> Vec<TerminalTextMatch> {
        if query.is_empty() {
            return Vec::new();
        }

        let mut folded = String::new();
        let mut folded_boundaries = Vec::new();
        let mut source_boundaries = Vec::new();
        fold_grapheme_boundaries_into(
            query,
            &mut folded,
            &mut folded_boundaries,
            &mut source_boundaries,
        );
        let folded_query = std::mem::take(&mut folded);
        if folded_query.is_empty() {
            return Vec::new();
        }
        // A folded query containing a non-ASCII byte can never be a substring
        // of an all-ASCII line, and an all-ASCII pair needs no grapheme
        // machinery: folding is per-byte lowercasing, so folded match offsets
        // ARE source columns.
        let ascii_query = folded_query.is_ascii();

        let link_state = self.link_state.borrow();
        let mut concealed: Vec<(usize, usize)> = Vec::new();
        let mut line_matches: Vec<TerminalTextMatch> = Vec::new();
        let mut results = Vec::new();

        for (line_number, line) in self.iter_rev_with_line_number(None) {
            if link_state.line_concealed(line_number) {
                continue;
            }
            let text = line.styled_line.text.as_str();
            let ascii_line = text.is_ascii();
            if ascii_line && !ascii_query {
                continue;
            }

            concealed.clear();
            concealed.extend(
                link_state
                    .by_line
                    .get(&line_number)
                    .into_iter()
                    .flatten()
                    .filter_map(|key| {
                        let visibility = link_state.visibility.get(key)?;
                        if !visibility.concealed {
                            return None;
                        }
                        let live = link_state.live.get(key)?;
                        Some((live.address.begin, live.address.end))
                    }),
            );

            line_matches.clear();
            if ascii_line {
                folded.clear();
                folded.push_str(text);
                folded.make_ascii_lowercase();
                line_matches.extend(folded.match_indices(folded_query.as_str()).filter_map(
                    |(start, matched)| {
                        let end = start + matched.len();
                        (!concealed.iter().any(|(hidden_begin, hidden_end)| {
                            start < *hidden_end && end > *hidden_begin
                        }))
                        .then_some(TerminalTextMatch {
                            line: line_number,
                            start,
                            end,
                        })
                    },
                ));
            } else {
                fold_grapheme_boundaries_into(
                    text,
                    &mut folded,
                    &mut folded_boundaries,
                    &mut source_boundaries,
                );
                line_matches.extend(folded.match_indices(folded_query.as_str()).filter_map(
                    |(start, matched)| {
                        let end = start + matched.len();
                        let start_boundary = folded_boundaries
                            .partition_point(|boundary| *boundary <= start)
                            .saturating_sub(1);
                        let end_boundary =
                            folded_boundaries.partition_point(|boundary| *boundary < end);
                        let start_column = source_boundaries[start_boundary];
                        let end_column = source_boundaries[end_boundary];
                        (!concealed.iter().any(|(hidden_begin, hidden_end)| {
                            start_column < *hidden_end && end_column > *hidden_begin
                        }))
                        .then_some(TerminalTextMatch {
                            line: line_number,
                            start: start_column,
                            end: end_column,
                        })
                    },
                ));
            }
            line_matches.dedup();
            results.extend(line_matches.drain(..).rev());
        }
        results
    }

    pub fn selected_text(&self, selection: &Selection) -> String {
        match selection {
            Selection::None => String::new(),
            Selection::Selecting { from, to, .. } | Selection::Selected { from, to } => {
                let offset = self.last_line_number - self.lines.len();

                // Selection line numbers are absolute and outlive the buffer:
                // a `clear()` (clear_lines) or scrollback eviction can leave a
                // stale selection pointing at lines that are no longer held.
                // Clamp to the live range `(offset, last_line_number]` and bail
                // when nothing overlaps, so the subtraction below never
                // underflows and `self.lines[i]` never indexes out of bounds.
                if self.lines.is_empty() || to.line <= offset || from.line > self.last_line_number {
                    return String::new();
                }
                let first_line = from.line.max(offset + 1);
                let last_line = to.line.min(self.last_line_number);
                let start_line_index = first_line - offset - 1;
                let to_line_index = last_line - offset - 1;
                // Only honor the selection's own column bounds on the lines
                // that survived the clamp; a clamped-in edge starts/ends whole.
                let use_from_column = first_line == from.line;
                let use_to_column = last_line == to.line;
                let link_state = self.link_state.borrow();

                (start_line_index..=to_line_index)
                    .filter_map(|i| {
                        let line = &self.lines[i];
                        let line_number = offset + i + 1;
                        if link_state.line_concealed(line_number) {
                            return None;
                        }
                        let text = line.styled_line.text.as_str();
                        let start_column = if i == start_line_index && use_from_column {
                            from.column
                        } else {
                            0
                        };
                        let end_column = if i == to_line_index && use_to_column {
                            to.column
                        } else {
                            text.len()
                        };

                        // Selection columns are byte offsets into the rendered line; clamp
                        // to the text and snap to char boundaries so copy can never slice
                        // past the end or mid-codepoint (either of which panics).
                        let start_column = clamp_to_char_boundary(text, start_column);
                        let end_column = clamp_to_char_boundary(text, end_column).max(start_column);

                        let concealed = link_state
                            .by_line
                            .get(&line_number)
                            .into_iter()
                            .flatten()
                            .filter_map(|key| {
                                let visibility = link_state.visibility.get(key)?;
                                if !visibility.concealed {
                                    return None;
                                }
                                let live = link_state.live.get(key)?;
                                Some((live.address.begin, live.address.end))
                            })
                            .collect::<Vec<_>>();
                        if concealed.is_empty() {
                            return Some(text[start_column..end_column].to_string());
                        }

                        let copied = text[start_column..end_column]
                            .grapheme_indices(true)
                            .map(|(relative, grapheme)| {
                                let begin = start_column + relative;
                                let end = begin + grapheme.len();
                                if concealed.iter().any(|(hidden_begin, hidden_end)| {
                                    begin < *hidden_end && end > *hidden_begin
                                }) {
                                    " "
                                } else {
                                    grapheme
                                }
                            })
                            .collect::<String>();
                        Some(copied)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
    }

    /// Finds the most recent word matching the given prefix.
    /// Tokens are broken apart using any non-alphanumeric delimiter (e.g., `:`, `/`,
    /// `]`, etc.) while preserving useful in-word punctuation like apostrophes and
    /// hyphens. If the user types a delimiter in the prefix, the full token (including
    /// the delimiter and the segment that follows) is matched. Trailing punctuation is
    /// stripped automatically so words like `guard:Awful,` stay searchable. Possessive
    /// endings (`'s`) are removed unless the prefix itself contains an apostrophe.
    ///
    /// # Arguments
    /// * `prefix` - The prefix to match against (case-insensitive)
    /// * `skip_words_in` - Optional set of words to ignore in the search (exact match)
    /// * `skip_words_folded` - Borrowed sets of lowercase-folded words to
    ///   ignore case-insensitively (candidates are folded before the check):
    ///   the completion blacklist and the offered-registered-suggestion
    ///   filter, passed as the caller already holds them — no per-call union
    ///   set is materialized
    /// * `n_recent_lines` - Number of recent lines to search through
    ///
    /// # Returns
    /// * `Option<String>` - The matching word if found, or None otherwise
    pub fn find_recent_word_by_prefix(
        &self,
        prefix: &str,
        skip_words_in: Option<&HashSet<String>>,
        skip_words_folded: &[&HashSet<String>],
        n_recent_lines: usize,
    ) -> Option<String> {
        let lowercase_prefix = prefix.to_lowercase();
        let is_internal_punctuation =
            |c: char| matches!(c, '\'' | '’' | '-' | '‐' | '‑' | '‒' | '–' | '—' | '_');
        let is_segment_delimiter = |c: char| !c.is_alphanumeric() && !is_internal_punctuation(c);
        let prefix_contains_delimiter = prefix.chars().any(is_segment_delimiter);
        let prefix_contains_apostrophe = prefix.chars().any(|c| matches!(c, '\'' | '’'));

        let consider_candidate = |candidate: &str| -> Option<String> {
            let candidate_for_match = if prefix_contains_apostrophe {
                candidate
            } else {
                strip_possessive_suffix(candidate)
            };

            if candidate_for_match.is_empty() {
                return None;
            }

            if let Some(history) = skip_words_in
                && history.contains(candidate_for_match)
            {
                return None;
            }

            let folded_candidate = candidate_for_match.to_lowercase();
            if skip_words_folded
                .iter()
                .any(|folded| folded.contains(&folded_candidate))
            {
                return None;
            }

            if folded_candidate.starts_with(&lowercase_prefix) {
                return Some(candidate_for_match.to_string());
            }

            None
        };

        let link_state = self.link_state.borrow();
        self.iter_rev_with_line_number(None)
            .filter(|(line_number, _)| !link_state.line_concealed(*line_number))
            .take(n_recent_lines)
            .find_map(|(_, line)| {
                // Split line by whitespace to get words
                for raw_word in line.styled_line.text.split_whitespace() {
                    // Clean the word by trimming non-alphanumeric chars from start/end
                    let word = raw_word.trim_matches(|c: char| !c.is_alphanumeric());

                    // Skip empty words
                    if word.is_empty() {
                        continue;
                    }

                    if prefix_contains_delimiter {
                        if let Some(result) = consider_candidate(word) {
                            return Some(result);
                        }
                        continue;
                    }

                    let mut segment_start: Option<usize> = None;

                    for (idx, ch) in word.char_indices() {
                        if is_segment_delimiter(ch) {
                            if let Some(start) = segment_start.take()
                                && start != idx
                                && let Some(result) = consider_candidate(&word[start..idx])
                            {
                                return Some(result);
                            }
                        } else if segment_start.is_none() {
                            segment_start = Some(idx);
                        }
                    }

                    if let Some(start) = segment_start
                        && let Some(result) = consider_candidate(&word[start..])
                    {
                        return Some(result);
                    }
                }
                None
            })
    }

    /// The link action under byte `column` of absolute line `line_number`, if any.
    /// Backs the pane's hover cursor and click dispatch.
    pub fn link_at(&self, line_number: usize, column: usize) -> Option<LinkAction> {
        self.link_span_at(line_number, column)
            .map(|link| link.action.clone())
    }

    pub(crate) fn link_span_at(&self, line_number: usize, column: usize) -> Option<&LinkSpan> {
        if self.link_state.borrow().line_concealed(line_number) {
            return None;
        }
        let offset = self.last_line_number - self.lines.len();
        if line_number <= offset || line_number > self.last_line_number {
            return None;
        }
        let line = self.lines.get(line_number - offset - 1)?;
        line.styled_line
            .links
            .iter()
            .find(|link| link.begin_pos <= column && column < link.end_pos)
    }

    pub(crate) fn link_key(&self, line_number: usize, link: &LinkSpan) -> Option<LinkKey> {
        self.link_state.borrow().key_at(line_number, link)
    }

    pub(crate) fn link_keys(&self) -> std::vec::IntoIter<LinkKey> {
        self.link_state.borrow().keys().into_iter()
    }

    pub(crate) fn link_span(&self, key: LinkKey) -> Option<&LinkSpan> {
        let address = self.link_state.borrow().address(key)?;
        let offset = self.last_line_number - self.lines.len();
        if address.line <= offset || address.line > self.last_line_number {
            return None;
        }
        self.lines
            .get(address.line - offset - 1)?
            .styled_line
            .links
            .iter()
            .find(|link| link.begin_pos == address.begin && link.end_pos == address.end)
    }

    /// The tooltip metadata under byte `column` of absolute line `line_number`.
    /// Kept separate from click lookup so hover can resolve lazy script copy
    /// without manufacturing a click event.
    pub fn link_tooltip_at(&self, line_number: usize, column: usize) -> Option<LinkTooltip> {
        self.link_span_at(line_number, column)
            .and_then(|link| link.tooltip.clone())
    }

    pub fn perform_line_operation(&mut self, line_number: usize, operation: LineOperation) {
        let offset = self.last_line_number - self.lines.len();
        // A line older than the buffer holds (scrolled out, or dropped by
        // `clear_lines`) has no index here; without this guard the subtraction
        // below underflows.
        if line_number <= offset {
            return;
        }
        let line_number = line_number - offset - 1;
        if let Some(line) = self.lines.get_mut(line_number) {
            let absolute_line_number = offset + line_number + 1;
            let had_links = !line.styled_line.links.is_empty();
            let old = Arc::clone(&line.styled_line);
            line.styled_line = operation.apply(&old);
            line.invalidate_spans();
            // An edit can add or drop a line's links; keep the O(1) count true.
            let has_links = !line.styled_line.links.is_empty();
            if has_links && !had_links {
                self.lines_with_links += 1;
            } else if !has_links && had_links {
                self.lines_with_links -= 1;
            }
            if had_links || has_links {
                self.link_state
                    .borrow_mut()
                    .replace_line(absolute_line_number, Some(&line.styled_line));
            }
        }
    }

    /// Drop the unterminated tail line (core's `RetractOpenLine`): the line's
    /// text is being routed elsewhere. Rolls the line number back so the next
    /// line takes the retracted one's number — exactly the accounting core
    /// keeps (`emitted_line_count` never counted the open line). A no-op when
    /// no line is open.
    pub fn retract_open_line(&mut self) {
        if !self.line_terminated
            && let Some(line) = self.lines.pop_back()
        {
            self.note_removed(self.last_line_number, &line);
            self.last_line_number -= 1;
            self.line_terminated = true;
        }
    }

    /// Clear the scrollback (`pane.clear()`), keeping the line numbering —
    /// numbers keep increasing across a clear so core/UI parity is untouched.
    pub fn clear_lines(&mut self) {
        if let Some(detached) = self.open_line_replacement.take() {
            self.link_state.borrow_mut().discard_detached(detached);
        }
        self.link_state.borrow_mut().retire_all();
        self.lines.clear();
        self.lines_with_links = 0;
        self.line_terminated = true;
    }
}

/// Lowercase a string a grapheme at a time into `folded`, retaining each
/// source grapheme's byte boundary in the folded string. Lowercasing can
/// expand a character, so direct byte offsets from the folded text cannot
/// safely index the source. The output buffers are cleared and refilled so a
/// caller scanning many lines can reuse their allocations; characters fold via
/// `char::to_lowercase` into the existing buffer rather than through the
/// per-grapheme `String` that `str::to_lowercase` would allocate.
fn fold_grapheme_boundaries_into(
    value: &str,
    folded: &mut String,
    folded_boundaries: &mut Vec<usize>,
    source_boundaries: &mut Vec<usize>,
) {
    folded.clear();
    folded_boundaries.clear();
    source_boundaries.clear();
    // Byte length bounds the grapheme count, sparing a counting pre-pass.
    folded.reserve(value.len());
    folded_boundaries.reserve(value.len() + 1);
    source_boundaries.reserve(value.len() + 1);
    folded_boundaries.push(0);
    source_boundaries.push(0);
    for (source_start, grapheme) in value.grapheme_indices(true) {
        for ch in grapheme.chars() {
            for lower in ch.to_lowercase() {
                folded.push(lower);
            }
        }
        folded_boundaries.push(folded.len());
        source_boundaries.push(source_start + grapheme.len());
    }
}

impl Drop for TerminalBuffer {
    fn drop(&mut self) {
        if let Some(detached) = self.open_line_replacement.take() {
            self.link_state.borrow_mut().discard_detached(detached);
        }
        self.link_state.borrow_mut().retire_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_core::session::connection::vt_processor::AnsiColor;
    use smudgy_core::session::styled_line::{Blink, StyledLine, TextAttributes, Underline, VtSpan};
    use std::num::NonZeroUsize; // Assuming VtSpan is needed for StyledLine::new

    // Helper to create Arc<StyledLine> for tests
    fn sl(s: &str) -> Arc<StyledLine> {
        Arc::new(StyledLine::new(s, Vec::<VtSpan>::new()))
    }

    #[test]
    fn test_new_buffer_initial_state() {
        let buffer = TerminalBuffer::new();
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.last_line_number, 0);
        assert_eq!(buffer.max_lines.get(), 10_000); // Default max lines
        assert!(!buffer.line_terminated); // Initial state before any line commit or push
    }

    #[test]
    fn test_new_with_max_lines_initial_state() {
        let max_lines = NonZeroUsize::new(50).unwrap();
        let buffer = TerminalBuffer::new_with_max_lines(max_lines);
        assert_eq!(buffer.len(), 0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.last_line_number, 0);
        assert_eq!(buffer.max_lines, max_lines);
        assert!(!buffer.line_terminated);
    }

    #[test]
    fn test_push_line_increments_current_line_number() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(3).unwrap());
        assert_eq!(buffer.last_line_number, 0);

        buffer.push_line(sl("line 1"));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, 1);
        assert!(buffer.line_terminated);

        buffer.push_line(sl("line 2"));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 2);
        assert!(buffer.line_terminated);
    }

    #[test]
    fn test_extend_line_increments_current_line_number() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(3).unwrap());

        // Case 1: Extending when line_terminated is true
        buffer.commit_current_line(); // Make line_terminated true
        assert!(buffer.line_terminated);
        buffer.extend_line(sl("line 1 part 1"));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, 1); // Incremented
        assert!(!buffer.line_terminated); // Becomes false after extend

        // Case 2: Extending when line_terminated is false (continuing a line)
        // The current logic in extend_line when line_terminated is false and buffer not empty
        // pops and re-pushes the existing last line, ignoring the input.
        // So, current_line_number should not change.
        let previous_line_number = buffer.last_line_number;
        buffer.extend_line(sl("line 1 part 2 - ignored"));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, previous_line_number); // Not incremented
        assert!(!buffer.line_terminated);

        // Reset for next test part
        let mut buffer2 = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(3).unwrap());

        // Case 3: Extending when line_terminated is false but buffer is empty (first line)
        assert!(!buffer2.line_terminated);
        assert!(buffer2.is_empty());
        buffer2.extend_line(sl("first line segment"));
        assert_eq!(buffer2.len(), 1);
        assert_eq!(buffer2.last_line_number, 1); // Incremented
        assert!(!buffer2.line_terminated);
    }

    /// Blank fragments (a bare prompt boundary's empty flush, a line end
    /// landing on an open prompt row) consume a line number exactly when a
    /// real fragment would, but never copy the row they land on.
    #[test]
    fn blank_fragments_open_rows_but_never_glue() {
        // A blank on an empty, never-terminated buffer opens row 1, like any
        // first fragment (a corpus starting with an empty line relies on it).
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.extend_line(sl(""));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, 1);
        assert!(!buffer.line_terminated);

        // A real fragment onto that blank open row takes its `Arc` outright:
        // same row, same number, no join.
        let real = sl("HP:10> ");
        buffer.extend_line(real.clone());
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, 1);
        let tail = buffer.iter_rev().next().unwrap();
        assert!(Arc::ptr_eq(&tail.styled_line, &real));

        // A blank onto an open real row leaves it untouched and still open.
        buffer.extend_line(sl(""));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.last_line_number, 1);
        assert!(!buffer.line_terminated);
        let tail = buffer.iter_rev().next().unwrap();
        assert!(Arc::ptr_eq(&tail.styled_line, &real));

        // Once the row is committed, a blank opens the next row as before.
        buffer.commit_current_line();
        buffer.extend_line(sl(""));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 2);
        assert!(!buffer.line_terminated);
    }

    #[test]
    fn selected_text_survives_clear_and_scrollback_eviction() {
        use super::selection::{BufferPosition, Selection};
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl("alpha"));
        buffer.push_line(sl("bravo"));
        let selection = Selection::Selected {
            from: BufferPosition { line: 1, column: 0 },
            to: BufferPosition { line: 2, column: 5 },
        };
        assert_eq!(buffer.selected_text(&selection), "alpha\nbravo");

        // A script `mainPane.clear()` empties the buffer but keeps line
        // numbering; the stale selection must clamp away, never panic
        // (it used to underflow in debug / index out of bounds in release).
        buffer.clear_lines();
        assert_eq!(buffer.selected_text(&selection), "");

        // Fresh content after the clear: the stale low line numbers stay
        // clamped out, so no wrong row is ever read.
        buffer.push_line(sl("charlie"));
        assert_eq!(buffer.selected_text(&selection), "");

        // A selection that straddles the live/evicted boundary keeps only the
        // surviving tail, starting whole (the clamped-in edge drops its column).
        let straddling = Selection::Selected {
            from: BufferPosition { line: 2, column: 2 },
            to: BufferPosition { line: 3, column: 7 },
        };
        assert_eq!(buffer.selected_text(&straddling), "charlie");
    }

    #[test]
    fn test_buffer_wrapping_and_current_line_number() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(2).unwrap());
        buffer.push_line(sl("1"));
        buffer.push_line(sl("2"));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 2);

        buffer.push_line(sl("3")); // Wraps, "1" is popped
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 3);
        assert_eq!(buffer.lines[0].styled_line.text, "2");
        assert_eq!(buffer.lines[1].styled_line.text, "3");

        buffer.push_line(sl("4")); // Wraps, "2" is popped
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.last_line_number, 4);
        assert_eq!(buffer.lines[0].styled_line.text, "3");
        assert_eq!(buffer.lines[1].styled_line.text, "4");
    }

    #[test]
    fn test_iter_rev_with_line_number_empty() {
        let buffer = TerminalBuffer::new();
        assert_eq!(buffer.iter_rev_with_line_number(None).count(), 0);
    }

    #[test]
    fn test_iter_rev_with_line_number_no_wrap() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(5).unwrap());
        buffer.push_line(sl("L1")); // cln=1
        buffer.push_line(sl("L2")); // cln=2
        buffer.push_line(sl("L3")); // cln=3. Lines: [L1,L2,L3]

        // iter().rev(): L3, L2, L1
        // enumerate(): (0,L3), (1,L2), (2,L1)
        // map |(i,line)| (cln - i, line) where cln = 3
        // (3-0, L3) -> (3,L3)
        // (3-1, L2) -> (2,L2)
        // (3-2, L1) -> (1,L1)
        let mut iter = buffer.iter_rev_with_line_number(None);
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((3, "L3"))
        );
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((2, "L2"))
        );
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((1, "L1"))
        );
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn test_iter_rev_with_line_number_with_wrap() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(2).unwrap());
        buffer.push_line(sl("L1")); // cln=1
        buffer.push_line(sl("L2")); // cln=2. Buffer: [L1,L2]
        buffer.push_line(sl("L3")); // cln=3. Buffer: [L2,L3]

        // cln = 3. Lines in buffer (reversed): L3, L2
        // enumerate: (0, L3), (1, L2)
        // map |(i,line)| (cln - i, line)
        // (3-0, L3) -> (3, L3)
        // (3-1, L2) -> (2, L2)
        let mut iter = buffer.iter_rev_with_line_number(None);
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((3, "L3"))
        );
        assert_eq!(
            iter.next().map(|(n, l)| (n, l.styled_line.text.as_str())),
            Some((2, "L2"))
        );
        assert_eq!(iter.next(), None);
    }

    fn linked_line(text: &str, begin: usize, end: usize) -> Arc<StyledLine> {
        use smudgy_core::session::styled_line::{LinkSpan, VtSpan};
        let style = Style {
            fg: Color::Rgb {
                r: 200,
                g: 10,
                b: 10,
            },
            bg: Color::DefaultBackground,
            ..Style::DEFAULT
        };
        let mut line = StyledLine::new(
            text,
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: text.len(),
            }],
        );
        line.links.push(LinkSpan {
            begin_pos: begin,
            end_pos: end,
            action: LinkAction::Send(Arc::from("north")),
            tooltip: None,
            style: None,
        });
        Arc::new(line)
    }

    #[test]
    fn sgr_bold_modes_preserve_the_regular_font_and_choose_weight_and_color() {
        use smudgy_core::models::settings::TerminalBoldMode;

        let mut prefs = (*crate::prefs::current()).clone();
        let regular_font = prefs.font;
        let style = Style {
            fg: Color::Ansi {
                color: AnsiColor::Red,
                bold: false,
            },
            attributes: TextAttributes {
                bold: true,
                ..TextAttributes::DEFAULT
            },
            ..Style::DEFAULT
        };

        prefs.bold_mode = TerminalBoldMode::Bold;
        let bold = make_span("bold", style, false, None, &prefs);
        assert_eq!(
            bold.font,
            Some(iced::Font {
                weight: iced::font::Weight::Bold,
                ..regular_font
            })
        );
        assert_eq!(
            bold.color,
            Some(prefs.resolve(Color::Ansi {
                color: AnsiColor::Red,
                bold: false,
            }))
        );

        prefs.bold_mode = TerminalBoldMode::Bright;
        let bright = make_span("bold", style, false, None, &prefs);
        assert_eq!(bright.font, Some(regular_font));
        assert_eq!(
            bright.color,
            Some(prefs.resolve(Color::Ansi {
                color: AnsiColor::Red,
                bold: true,
            }))
        );

        prefs.bold_mode = TerminalBoldMode::BoldAndBright;
        let both = make_span("bold", style, false, None, &prefs);
        assert_eq!(
            both.font,
            Some(iced::Font {
                weight: iced::font::Weight::Bold,
                ..regular_font
            })
        );
        assert_eq!(both.color, bright.color);

        prefs.bold_mode = TerminalBoldMode::Bold;
        let explicit_bright = make_span(
            "bright",
            Style {
                fg: Color::Ansi {
                    color: AnsiColor::Red,
                    bold: true,
                },
                ..style
            },
            false,
            None,
            &prefs,
        );
        assert_eq!(explicit_bright.color, bright.color);
    }

    #[test]
    fn make_span_renders_sgr_attributes_and_reverse_colors() {
        let prefs = crate::prefs::current();
        let style = Style {
            fg: Color::Rgb {
                r: 10,
                g: 20,
                b: 30,
            },
            bg: Color::Rgb {
                r: 40,
                g: 50,
                b: 60,
            },
            attributes: TextAttributes {
                faint: true,
                italic: true,
                underline: Underline::Double,
                blink: Blink::Fast,
                crossed_out: true,
                reverse: true,
                ..TextAttributes::DEFAULT
            },
        };
        let span = make_span("styled", style, false, None, &prefs);
        let mut reversed_fg = prefs.resolve(style.bg);
        reversed_fg.a *= 0.5;
        assert_eq!(span.color, Some(reversed_fg));
        assert_eq!(
            span.highlight.map(|highlight| highlight.background),
            Some(Background::Color(prefs.resolve(style.fg)))
        );
        assert_eq!(
            span.font.map(|font| font.style),
            Some(iced::font::Style::Italic)
        );
        assert!(span.underline);
        assert!(span.strikethrough);
        assert_eq!(
            span.link,
            Some(SpanMetadata {
                blink: Blink::Fast,
                underline: LinkDecoration::Double,
                strikethrough: LinkDecoration::Solid,
                ..SpanMetadata::default()
            })
        );
    }

    #[test]
    fn to_spans_splits_at_link_boundaries_with_chip() {
        let line = linked_line("go north now", 3, 8);
        let prefs = crate::prefs::current();
        let spans = to_spans(&line, &prefs);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "go ");
        assert_eq!(spans[1].text, "north");
        assert_eq!(spans[2].text, " now");

        // Only the linked segment is underlined, over a wash of its own foreground;
        // the segments around it keep the plain background.
        assert!(!spans[0].underline && !spans[2].underline);
        assert!(spans[1].underline);
        let fg = prefs.resolve(Color::Rgb {
            r: 200,
            g: 10,
            b: 10,
        });
        assert_eq!(
            spans[1].highlight.map(|h| h.background),
            Some(Background::Color(iced::Color {
                a: LINK_WASH_ALPHA,
                ..fg
            }))
        );
        assert_ne!(
            spans[0].highlight.map(|h| h.background),
            spans[1].highlight.map(|h| h.background)
        );
    }

    #[test]
    fn authored_osc_style_suppresses_fallback_link_affordance() {
        let prefs = crate::prefs::current();
        let authored = LinkStyle::default();
        let span = make_span("link", Style::DEFAULT, true, Some(&authored), &prefs);
        assert!(
            !span.underline,
            "an empty authored style is not auto-underlined"
        );
        assert!(
            span.highlight.is_none(),
            "an authored style gets no fallback wash"
        );
    }

    #[test]
    fn protocol_concealment_suppresses_every_visual_affordance() {
        let prefs = crate::prefs::current();
        let resolved = LinkRenderStyle {
            authored: false,
            style: LinkTextStyle::default(),
            spoiler_concealed: false,
            hidden: true,
        };
        let span = make_resolved_span(
            "hidden",
            Style::DEFAULT,
            true,
            Some(&resolved),
            false,
            &prefs,
        );
        assert_eq!(span.text, "      ");
        assert_eq!(span.color, Some(iced::Color::TRANSPARENT));
        assert!(span.highlight.is_none());
        assert!(!span.underline);
        assert!(!span.strikethrough);
    }

    #[test]
    fn concealed_spoiler_replaces_each_grapheme_with_a_space() {
        let prefs = crate::prefs::current();
        let resolved = LinkRenderStyle {
            authored: false,
            style: LinkTextStyle::default(),
            spoiler_concealed: true,
            hidden: false,
        };
        let span = make_resolved_span(
            "🔮💀🗝️",
            Style::DEFAULT,
            true,
            Some(&resolved),
            false,
            &prefs,
        );
        assert_eq!(span.text, "   ");
        assert!(span.highlight.is_none());
        assert!(!span.underline);
    }

    #[test]
    fn concealed_spoiler_offsets_map_back_to_the_source_line() {
        let prefs = crate::prefs::current();
        let line = linked_line("A🗝️B", 1, 8);
        let rendered = to_spans_with(&line, &prefs, false, |_| LinkRenderStyle {
            authored: false,
            style: LinkTextStyle::default(),
            spoiler_concealed: true,
            hidden: false,
        });
        let text: String = rendered
            .spans
            .iter()
            .flat_map(|span| span.text.chars())
            .collect();

        assert_eq!(text, "A B");
        assert_eq!(rendered.offsets.source_to_rendered(1), 1);
        assert_eq!(rendered.offsets.source_to_rendered(8), 2);
        assert_eq!(rendered.offsets.rendered_to_source(1), 1);
        assert_eq!(rendered.offsets.rendered_to_source(2), 8);
        assert_eq!(rendered.offsets.rendered_to_source(3), 9);
    }

    #[test]
    fn unchanged_spans_keep_the_offset_builder_in_identity_mode() {
        let mut offsets = RenderedOffsetsBuilder::default();
        let first = "plain text 🔮";
        let second = " and more";

        offsets.push(first, first, false);
        offsets.push(second, second, false);

        assert!(matches!(
            &offsets,
            RenderedOffsetsBuilder::Identity { end }
                if *end == first.len() + second.len()
        ));
        assert!(matches!(offsets.finish(), RenderedOffsets::Identity));
    }

    #[test]
    fn mapped_offsets_preserve_an_identity_prefix_and_suffix() {
        let prefix = "pré🔮";
        let spoiler = "🗝️";
        let concealed = " ";
        let suffix = "尾x";
        let mut offsets = RenderedOffsetsBuilder::default();

        offsets.push(prefix, prefix, false);
        offsets.push(spoiler, concealed, true);
        offsets.push(suffix, suffix, false);
        match &offsets {
            RenderedOffsetsBuilder::Mapped {
                identity_prefix,
                source,
                rendered,
                ..
            } => {
                assert_eq!(*identity_prefix, prefix.len());
                assert_eq!(source.first(), Some(&prefix.len()));
                assert_eq!(rendered.first(), Some(&prefix.len()));
            }
            RenderedOffsetsBuilder::Identity { .. } => {
                panic!("rewritten text should initialize offset mapping")
            }
        }
        let offsets = offsets.finish();

        let source_spoiler_end = prefix.len() + spoiler.len();
        let rendered_spoiler_end = prefix.len() + concealed.len();
        for offset in 0..=prefix.len() {
            assert_eq!(offsets.source_to_rendered(offset), offset);
            assert_eq!(offsets.rendered_to_source(offset), offset);
        }
        assert_eq!(offsets.source_to_rendered(prefix.len() + 1), prefix.len());
        assert_eq!(
            offsets.source_to_rendered(source_spoiler_end),
            rendered_spoiler_end
        );
        assert_eq!(
            offsets.rendered_to_source(rendered_spoiler_end),
            source_spoiler_end
        );
        assert_eq!(
            offsets.source_to_rendered(source_spoiler_end + 1),
            rendered_spoiler_end + 1
        );
        assert_eq!(
            offsets.rendered_to_source(rendered_spoiler_end + 1),
            source_spoiler_end + 1
        );
        assert_eq!(
            offsets.source_to_rendered(source_spoiler_end + suffix.len()),
            rendered_spoiler_end + suffix.len()
        );
        assert_eq!(offsets.source_to_rendered(usize::MAX), usize::MAX);
        assert_eq!(offsets.rendered_to_source(usize::MAX), usize::MAX);
    }

    #[test]
    fn authored_osc_false_values_override_active_sgr_attributes() {
        use smudgy_core::session::styled_line::{LinkTextStyle, TextAttributes};

        let prefs = crate::prefs::current();
        let authored = LinkStyle {
            base: LinkTextStyle {
                bold: Some(false),
                italic: Some(false),
                underline: Some(LinkDecoration::None),
                strikethrough: Some(LinkDecoration::None),
                ..LinkTextStyle::default()
            },
            ..LinkStyle::default()
        };
        let span = make_span(
            "link",
            Style {
                attributes: TextAttributes {
                    bold: true,
                    italic: true,
                    underline: Underline::Single,
                    crossed_out: true,
                    ..TextAttributes::DEFAULT
                },
                ..Style::DEFAULT
            },
            true,
            Some(&authored),
            &prefs,
        );
        assert_ne!(
            span.font.map(|font| font.weight),
            Some(iced::font::Weight::Bold)
        );
        assert_ne!(
            span.font.map(|font| font.style),
            Some(iced::font::Style::Italic)
        );
        assert!(!span.underline);
        assert!(!span.strikethrough);
    }

    #[test]
    fn to_spans_keeps_explicit_background_under_a_link() {
        use smudgy_core::session::styled_line::{LinkSpan, VtSpan};
        let style = Style {
            fg: Color::Rgb {
                r: 200,
                g: 10,
                b: 10,
            },
            bg: Color::Rgb { r: 1, g: 2, b: 3 },
            ..Style::DEFAULT
        };
        let mut line = StyledLine::new(
            "north",
            vec![VtSpan {
                style,
                begin_pos: 0,
                end_pos: 5,
            }],
        );
        line.links.push(LinkSpan {
            begin_pos: 0,
            end_pos: 5,
            action: LinkAction::Send(Arc::from("north")),
            tooltip: None,
            style: None,
        });
        let prefs = crate::prefs::current();
        let spans = to_spans(&Arc::new(line), &prefs);
        assert_eq!(spans.len(), 1);
        // The author's background wins over the wash; the underline stays.
        assert!(spans[0].underline);
        assert_eq!(
            spans[0].highlight.map(|h| h.background),
            Some(Background::Color(prefs.resolve(Color::Rgb {
                r: 1,
                g: 2,
                b: 3
            })))
        );
    }

    #[test]
    fn link_at_resolves_by_absolute_line_and_column() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl("plain"));
        buffer.push_line(linked_line("go north now", 3, 8));

        // Inside the link.
        assert_eq!(
            buffer.link_at(2, 5),
            Some(LinkAction::Send(Arc::from("north")))
        );
        // Boundary semantics: begin inclusive, end exclusive.
        assert_eq!(
            buffer.link_at(2, 3),
            Some(LinkAction::Send(Arc::from("north")))
        );
        assert_eq!(buffer.link_at(2, 8), None);
        // Off-link text, another line, and out-of-window numbers all miss.
        assert_eq!(buffer.link_at(2, 0), None);
        assert_eq!(buffer.link_at(1, 5), None);
        assert_eq!(buffer.link_at(0, 5), None);
        assert_eq!(buffer.link_at(99, 5), None);

        let keys: Vec<_> = buffer.link_keys().collect();
        assert_eq!(keys.len(), 1);
        assert_eq!(
            buffer.link_span(keys[0]).map(|link| link.action.clone()),
            Some(LinkAction::Send(Arc::from("north")))
        );
    }

    #[test]
    fn terminal_text_search_is_case_insensitive_and_newest_first() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl("alpha older Alpha"));
        buffer.push_line(sl("new ALPHA"));

        assert_eq!(
            buffer.find_text_matches("alpha"),
            vec![
                TerminalTextMatch {
                    line: 2,
                    start: 4,
                    end: 9,
                },
                TerminalTextMatch {
                    line: 1,
                    start: 12,
                    end: 17,
                },
                TerminalTextMatch {
                    line: 1,
                    start: 0,
                    end: 5,
                },
            ]
        );
    }

    #[test]
    fn terminal_text_search_maps_expanding_lowercase_to_source_graphemes() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl("İstanbul"));

        assert_eq!(
            buffer.find_text_matches("i\u{307}"),
            vec![TerminalTextMatch {
                line: 1,
                start: 0,
                end: 2,
            }]
        );
    }

    #[test]
    fn find_recent_word_logic() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl("hello world"));
        buffer.push_line(sl("test another one"));
        buffer.push_line(sl("prefix_found here"));
        buffer.push_line(sl("try prefix_again"));

        // Test basic prefix matching
        assert_eq!(
            buffer.find_recent_word_by_prefix("pref", None, &[], 4),
            Some("prefix_again".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("pref", None, &[], 2),
            Some("prefix_again".to_string())
        ); // Only search last 2 lines
        assert_eq!(
            buffer.find_recent_word_by_prefix("anot", None, &[], 4),
            Some("another".to_string())
        );

        // Test case-insensitivity
        assert_eq!(
            buffer.find_recent_word_by_prefix("PREFIX", None, &[], 4),
            Some("prefix_again".to_string())
        );

        // Test not found
        assert_eq!(
            buffer.find_recent_word_by_prefix("nonexistent", None, &[], 4),
            None
        );

        // Test with skip_words
        let mut skip_set = HashSet::new();
        skip_set.insert("prefix_again".to_string());
        assert_eq!(
            buffer.find_recent_word_by_prefix("pref", Some(&skip_set), &[], 4),
            Some("prefix_found".to_string())
        );

        skip_set.insert("prefix_found".to_string());
        assert_eq!(
            buffer.find_recent_word_by_prefix("pref", Some(&skip_set), &[], 4),
            None
        ); // All "pref" words skipped

        // Test n_recent_lines
        assert_eq!(
            buffer.find_recent_word_by_prefix("hello", None, &[], 1),
            None
        ); // "hello" is not in the last line
        assert_eq!(
            buffer.find_recent_word_by_prefix("hello", None, &[], 4),
            Some("hello".to_string())
        ); // "hello" is in the last 4 lines
    }

    #[test]
    fn find_recent_word_handles_colon_segments() {
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(sl(
            "[SC:Order] [Rr'Kar:Awful] guard:Awful Mem:2 T:40 Exits:N(S)W>",
        ));
        buffer.push_line(sl("An alert militia guard misses Zurek with his slash."));

        assert_eq!(
            buffer.find_recent_word_by_prefix("sc", None, &[], 5),
            Some("SC".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("sc:", None, &[], 5),
            Some("SC:Order".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("rr", None, &[], 5),
            Some("Rr'Kar".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("gu", None, &[], 5),
            Some("guard".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("guard:", None, &[], 5),
            Some("guard:Awful".to_string())
        );

        buffer.push_line(sl("Half-orc's strike leaves a scratch-!"));
        assert_eq!(
            buffer.find_recent_word_by_prefix("half", None, &[], 5),
            Some("Half-orc".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("half-orc'", None, &[], 5),
            Some("Half-orc's".to_string())
        );
        assert_eq!(
            buffer.find_recent_word_by_prefix("scr", None, &[], 5),
            Some("scratch".to_string())
        );
    }

    fn protocol_line(
        selection: Option<smudgy_core::session::styled_line::LinkSelection>,
        visibility: Option<LinkVisibility>,
        spoiler: bool,
    ) -> (Arc<StyledLine>, LinkAction) {
        use smudgy_core::session::styled_line::LinkProtocol;

        let action = LinkAction::Configured {
            primary: Some(Box::new(LinkAction::ServerSend(Arc::from("look")))),
            disabled: false,
            primary_enabled: true,
            menu: None,
            menu_on_left_click: false,
            protocol: Some(LinkProtocol {
                selection,
                visibility,
                spoiler,
            }),
        };
        let mut line = StyledLine::new("link", Vec::new());
        line.links.push(LinkSpan {
            begin_pos: 0,
            end_pos: 4,
            action: action.clone(),
            tooltip: None,
            style: None,
        });
        (Arc::new(line), action)
    }

    fn matcher_link(action: LinkAction, begin: usize) -> LiveLink {
        LiveLink {
            address: LinkAddress {
                line: 1,
                begin,
                end: begin + 1,
            },
            registration: LinkRegistration {
                action,
                selection: None,
                spoiler: false,
                visibility: None,
            },
        }
    }

    #[test]
    fn exact_link_matcher_prefers_the_closest_duplicate() {
        let action = LinkAction::Send(Arc::from("look"));
        let old = vec![(LinkKey::test(0), matcher_link(action.clone(), 10))];
        let new = vec![matcher_link(action.clone(), 0), matcher_link(action, 10)];

        assert_eq!(BufferLinkState::monotonic_matches(&old, &new), [(0, 1)]);
    }

    #[test]
    fn huge_shifted_link_line_uses_bounded_matching() {
        const LINK_COUNT: usize = 50_000;

        let action = LinkAction::Send(Arc::from("look"));
        let old = (0..LINK_COUNT)
            .map(|index| {
                (
                    LinkKey::test(u64::try_from(index).expect("test index fits u64")),
                    matcher_link(action.clone(), index * 2),
                )
            })
            .collect::<Vec<_>>();
        let new = (0..LINK_COUNT)
            .map(|index| matcher_link(action.clone(), index * 2 + 1))
            .collect::<Vec<_>>();

        let matches = BufferLinkState::monotonic_matches(&old, &new);
        assert_eq!(matches.len(), LINK_COUNT);
        assert_eq!(matches.first(), Some(&(0, 0)));
        assert_eq!(matches.last(), Some(&(LINK_COUNT - 1, LINK_COUNT - 1)));
    }

    #[test]
    fn huge_unrelated_link_lines_do_not_do_quadratic_work() {
        const LINK_COUNT: usize = 20_000;

        let old_action = LinkAction::Send(Arc::from("old"));
        let new_action = LinkAction::Send(Arc::from("new"));
        let old = (0..LINK_COUNT)
            .map(|index| {
                (
                    LinkKey::test(u64::try_from(index).expect("test index fits u64")),
                    matcher_link(old_action.clone(), index * 2),
                )
            })
            .collect::<Vec<_>>();
        let new = (0..LINK_COUNT)
            .map(|index| matcher_link(new_action.clone(), index * 2 + 1))
            .collect::<Vec<_>>();

        assert!(BufferLinkState::monotonic_matches(&old, &new).is_empty());
    }

    #[test]
    fn selection_and_visited_state_is_shared_across_routed_buffers() {
        use smudgy_core::session::styled_line::LinkSelection;

        let shared = Rc::new(RefCell::new(LinkProtocolState::default()));
        let mut main =
            TerminalBuffer::new_with_protocol_state(NonZeroUsize::new(10).unwrap(), shared.clone());
        let mut routed =
            TerminalBuffer::new_with_protocol_state(NonZeroUsize::new(10).unwrap(), shared.clone());
        let first = LinkSelection {
            group: Arc::from("stance"),
            value: Arc::from("attack"),
            toggle: false,
            selected: false,
            exclusive: true,
            disabled: false,
        };
        let second = LinkSelection {
            value: Arc::from("defend"),
            ..first.clone()
        };
        let (main_line, main_action) = protocol_line(Some(first.clone()), None, false);
        let (routed_line, routed_action) = protocol_line(Some(second.clone()), None, false);
        main.push_line(main_line);
        routed.push_line(routed_line);

        assert_eq!(
            shared.borrow_mut().toggle_link_selection(&main_action),
            Some(true)
        );
        assert!(shared.borrow().selected(&first));
        assert!(!shared.borrow().selected(&second));
        assert_eq!(
            shared.borrow_mut().toggle_link_selection(&routed_action),
            Some(true)
        );
        assert!(!shared.borrow().selected(&first));
        assert!(shared.borrow().selected(&second));

        shared.borrow_mut().mark_visited(&main_action);
        assert!(shared.borrow().visited(&main_action));
    }

    #[test]
    fn selection_lifecycle_refcounts_retire_and_reinitialize_groups() {
        use smudgy_core::session::styled_line::LinkSelection;

        let shared = Rc::new(RefCell::new(LinkProtocolState::default()));
        let mut first_buffer =
            TerminalBuffer::new_with_protocol_state(NonZeroUsize::new(2).unwrap(), shared.clone());
        let mut second_buffer =
            TerminalBuffer::new_with_protocol_state(NonZeroUsize::new(2).unwrap(), shared.clone());
        let selection = LinkSelection {
            group: Arc::from("answer"),
            value: Arc::from("yes"),
            toggle: true,
            selected: false,
            exclusive: true,
            disabled: false,
        };
        let (line, action) = protocol_line(Some(selection.clone()), None, false);
        first_buffer.push_line(line.clone());
        second_buffer.push_line(line);
        assert_eq!(shared.borrow().selection_references(&selection), 2);
        assert_eq!(
            shared.borrow_mut().toggle_link_selection(&action),
            Some(true)
        );

        first_buffer.clear_lines();
        assert_eq!(shared.borrow().selection_references(&selection), 1);
        drop(second_buffer);
        assert_eq!(shared.borrow().selection_references(&selection), 0);

        let selected = LinkSelection {
            selected: true,
            ..selection.clone()
        };
        let (line, _) = protocol_line(Some(selected.clone()), None, false);
        first_buffer.push_line(line);
        assert_eq!(shared.borrow().selection_references(&selected), 1);
        assert!(shared.borrow().selected(&selected));

        first_buffer.push_line(sl("plain"));
        first_buffer.push_line(sl("evicts the linked line"));
        assert_eq!(shared.borrow().selection_references(&selected), 0);
    }

    #[test]
    fn byte_shifting_edit_preserves_link_identity_and_instance_state() {
        use smudgy_core::session::styled_line::{LinkVisibility, LinkVisibilityExpire};

        let visibility = LinkVisibility {
            action: LinkVisibilityAction::Conceal,
            delay_ms: Some(5_000),
            expire: LinkVisibilityExpire {
                input: false,
                prompt: false,
                output: false,
                output_delay_ms: 0,
            },
            whole_line: false,
        };
        let (line, _) = protocol_line(None, Some(visibility), true);
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(line);
        let key = buffer.link_keys().next().expect("link key");
        let link_state = buffer.link_state();
        assert!(link_state.borrow_mut().reveal_spoiler(key));
        let activated = Instant::now();
        link_state.borrow_mut().activate_visibility(key, activated);
        let generation = link_state.borrow().visual_generation();

        buffer.perform_line_operation(
            1,
            LineOperation::Insert {
                str: Arc::new("prefix ".to_string()),
                begin: 0,
                end: 0,
                style: Style::DEFAULT.into(),
            },
        );

        let moved = buffer.link_keys().next().expect("moved link key");
        assert_eq!(moved, key);
        assert!(link_state.borrow().spoiler_revealed(moved));
        assert_eq!(
            link_state
                .borrow()
                .visibility
                .get(&moved)
                .and_then(|state| state.activated),
            Some(activated)
        );
        assert_eq!(
            link_state.borrow().visual_generation(),
            generation,
            "moving a link must not invalidate every dynamic-link paragraph"
        );
    }

    #[test]
    fn carriage_return_replacement_preserves_semantic_link_state() {
        let (line, _) = protocol_line(None, None, true);
        let replacement = Arc::new(line.insert("shifted ", 0, 0, Style::DEFAULT));
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.extend_line(line);
        let key = buffer.link_keys().next().expect("link key");
        let link_state = buffer.link_state();
        assert!(link_state.borrow_mut().reveal_spoiler(key));

        buffer.begin_open_line_replacement();
        buffer.finish_open_line_replacement(Some(replacement));

        let moved = buffer.link_keys().next().expect("replacement link key");
        assert_eq!(moved, key);
        assert!(link_state.borrow().spoiler_revealed(moved));
        assert_eq!(buffer.last_line_number(), 1);
        assert_eq!(buffer.len(), 1);
    }

    #[test]
    fn carriage_return_identity_survives_intervening_committed_output() {
        let (line, _) = protocol_line(None, None, true);
        let replacement = Arc::new(line.insert("shifted ", 0, 0, Style::DEFAULT));
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.extend_line(line);
        let key = buffer.link_keys().next().expect("link key");
        let link_state = buffer.link_state();
        assert!(link_state.borrow_mut().reveal_spoiler(key));

        buffer.begin_open_line_replacement();
        buffer.push_line(sl("trigger output"));
        buffer.finish_open_line_replacement(Some(replacement));

        let moved = buffer.link_keys().next().expect("replacement link key");
        assert_eq!(moved, key);
        assert!(link_state.borrow().spoiler_revealed(moved));
        assert_eq!(buffer.last_line_number(), 2);
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn ordinary_retraction_does_not_transfer_link_instance_state() {
        let (line, _) = protocol_line(None, None, true);
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.extend_line(line.clone());
        let retired = buffer.link_keys().next().expect("link key");
        let link_state = buffer.link_state();
        assert!(link_state.borrow_mut().reveal_spoiler(retired));

        buffer.retract_open_line();
        buffer.extend_line(line);

        let fresh = buffer.link_keys().next().expect("fresh link key");
        assert_ne!(fresh, retired);
        assert!(!link_state.borrow().spoiler_revealed(fresh));
    }

    #[test]
    fn identical_links_keep_ordered_identity_after_prefix_insert() {
        let (_, action) = protocol_line(None, None, true);
        let mut line = StyledLine::new("link------link", Vec::new());
        for (begin_pos, end_pos) in [(0, 4), (10, 14)] {
            line.links.push(LinkSpan {
                begin_pos,
                end_pos,
                action: action.clone(),
                tooltip: None,
                style: None,
            });
        }
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(Arc::new(line));
        let original: Vec<_> = buffer.link_keys().collect();
        let link_state = buffer.link_state();
        assert!(link_state.borrow_mut().reveal_spoiler(original[1]));

        buffer.perform_line_operation(
            1,
            LineOperation::Insert {
                str: Arc::new("fifteen bytes--".to_string()),
                begin: 0,
                end: 0,
                style: Style::DEFAULT.into(),
            },
        );

        let shifted: Vec<_> = buffer.link_keys().collect();
        assert_eq!(shifted, original);
        assert!(!link_state.borrow().spoiler_revealed(shifted[0]));
        assert!(link_state.borrow().spoiler_revealed(shifted[1]));
    }

    #[test]
    fn retired_selection_cannot_be_toggled_by_a_stale_menu_source() {
        use smudgy_core::session::styled_line::LinkSelection;

        let shared = Rc::new(RefCell::new(LinkProtocolState::default()));
        let selection = LinkSelection {
            group: Arc::from("answer"),
            value: Arc::from("yes"),
            toggle: true,
            selected: false,
            exclusive: true,
            disabled: false,
        };
        let (line, action) = protocol_line(Some(selection.clone()), None, false);
        let mut buffer =
            TerminalBuffer::new_with_protocol_state(NonZeroUsize::new(1).unwrap(), shared.clone());
        buffer.push_line(line);
        buffer.push_line(sl("evicts the menu source"));

        let generation = shared.borrow().visual_generation();
        assert_eq!(shared.borrow().selection_references(&selection), 0);
        assert_eq!(shared.borrow_mut().toggle_link_selection(&action), None);
        assert_eq!(shared.borrow().visual_generation(), generation);
    }

    #[test]
    fn spoiler_disclosure_is_buffer_shared_but_not_session_global() {
        let shared = Rc::new(RefCell::new(LinkProtocolState::default()));
        let mut first =
            TerminalBuffer::new_with_protocol_state(NonZeroUsize::new(10).unwrap(), shared.clone());
        let mut routed =
            TerminalBuffer::new_with_protocol_state(NonZeroUsize::new(10).unwrap(), shared);
        let (line, _) = protocol_line(None, None, true);
        first.push_line(line.clone());
        routed.push_line(line);
        let first_state = first.link_state();
        let split_half_state = first.link_state();
        let routed_state = routed.link_state();
        let key = first.link_keys().next().expect("first link key");
        let routed_key = routed.link_keys().next().expect("routed link key");

        assert!(Rc::ptr_eq(&first_state, &split_half_state));
        assert!(first_state.borrow_mut().reveal_spoiler(key));
        assert!(split_half_state.borrow().spoiler_revealed(key));
        assert!(!routed_state.borrow().spoiler_revealed(routed_key));
    }

    #[test]
    fn visibility_initial_state_and_expiry_live_on_the_buffer() {
        use smudgy_core::session::styled_line::LinkVisibilityExpire;

        let expire = LinkVisibilityExpire {
            input: true,
            prompt: false,
            output: false,
            output_delay_ms: 500,
        };
        let conceal_config = LinkVisibility {
            action: LinkVisibilityAction::Conceal,
            delay_ms: None,
            expire: expire.clone(),
            whole_line: false,
        };
        let mut conceal = VisibilityState::new(&conceal_config);
        assert!(!conceal.concealed);
        assert!(conceal.apply_expiry());
        assert!(conceal.concealed);

        let reveal_config = LinkVisibility {
            action: LinkVisibilityAction::Reveal,
            delay_ms: None,
            expire,
            whole_line: false,
        };
        let mut reveal = VisibilityState::new(&reveal_config);
        assert!(reveal.concealed);
        assert!(reveal.apply_expiry());
        assert!(!reveal.concealed);

        let cycle = VisibilityState::new(&LinkVisibility {
            action: LinkVisibilityAction::RevealThenConceal,
            delay_ms: Some(0),
            expire: LinkVisibilityExpire {
                input: false,
                prompt: false,
                output: false,
                output_delay_ms: 0,
            },
            whole_line: false,
        });
        assert!(cycle.concealed);
        assert!(!cycle.revealed_phase);
    }

    #[test]
    fn visibility_expiry_is_click_armed_and_skips_the_first_response() {
        use smudgy_core::session::styled_line::LinkVisibilityExpire;

        let visibility = LinkVisibility {
            action: LinkVisibilityAction::Conceal,
            delay_ms: None,
            expire: LinkVisibilityExpire {
                input: false,
                prompt: true,
                output: true,
                output_delay_ms: 10,
            },
            whole_line: false,
        };
        let (line, _) = protocol_line(None, Some(visibility), false);
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(line);
        let key = buffer.link_keys().next().expect("link key");
        let link_state = buffer.link_state();
        let created = Instant::now();

        link_state.borrow_mut().note_prompt();
        link_state.borrow_mut().note_output(created);
        link_state
            .borrow_mut()
            .note_output(created + Duration::from_millis(20));
        assert!(!link_state.borrow().concealed(key));

        link_state.borrow_mut().activate_visibility(key, created);
        link_state.borrow_mut().note_prompt();
        assert!(!link_state.borrow().concealed(key));
        link_state
            .borrow_mut()
            .note_output(created + Duration::from_millis(40));
        assert!(!link_state.borrow().concealed(key));

        link_state.borrow_mut().note_prompt();
        assert!(link_state.borrow().concealed(key));
    }

    #[test]
    fn copied_text_masks_concealed_link_content() {
        use super::selection::{BufferPosition, Selection};
        use smudgy_core::session::styled_line::LinkVisibilityExpire;

        let visibility = LinkVisibility {
            action: LinkVisibilityAction::Conceal,
            delay_ms: None,
            expire: LinkVisibilityExpire {
                input: false,
                prompt: false,
                output: false,
                output_delay_ms: 0,
            },
            whole_line: false,
        };
        let (line, _) = protocol_line(None, Some(visibility), false);
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(line);
        let key = buffer.link_keys().next().expect("link key");
        buffer
            .link_state()
            .borrow_mut()
            .activate_visibility(key, Instant::now());

        let selection = Selection::Selected {
            from: BufferPosition { line: 1, column: 0 },
            to: BufferPosition {
                line: 1,
                column: usize::MAX,
            },
        };
        assert_eq!(buffer.selected_text(&selection), "    ");
        assert!(
            buffer.find_text_matches("link").is_empty(),
            "terminal search must not expose concealed link text"
        );
    }

    #[test]
    fn whole_line_concealment_tombstones_content_and_retires_colocated_state() {
        use super::selection::{BufferPosition, Selection};
        use smudgy_core::session::styled_line::{LinkSelection, LinkVisibilityExpire};

        let selection_state = LinkSelection {
            group: Arc::from("poll"),
            value: Arc::from("yes"),
            toggle: false,
            selected: true,
            exclusive: false,
            disabled: false,
        };
        let visibility = LinkVisibility {
            action: LinkVisibilityAction::Conceal,
            delay_ms: None,
            expire: LinkVisibilityExpire {
                input: false,
                prompt: false,
                output: false,
                output_delay_ms: 0,
            },
            whole_line: true,
        };
        let shared = Rc::new(RefCell::new(LinkProtocolState::default()));
        let (line, _) = protocol_line(Some(selection_state.clone()), Some(visibility), false);
        let mut buffer =
            TerminalBuffer::new_with_protocol_state(NonZeroUsize::new(10).unwrap(), shared.clone());
        buffer.push_line(line);
        let key = buffer.link_keys().next().expect("link key");
        let link_state = buffer.link_state();

        link_state
            .borrow_mut()
            .activate_visibility(key, Instant::now());
        assert!(link_state.borrow().line_concealed(1));
        assert!(buffer.link_keys().next().is_none());
        assert_eq!(shared.borrow().selection_references(&selection_state), 1);

        let selection = Selection::Selected {
            from: BufferPosition { line: 1, column: 0 },
            to: BufferPosition {
                line: 1,
                column: usize::MAX,
            },
        };
        assert!(buffer.selected_text(&selection).is_empty());

        link_state.borrow_mut().retire_deleted_line_registrations();
        assert!(!link_state.borrow().contains(key));
        assert!(link_state.borrow().line_concealed(1));
        assert_eq!(shared.borrow().selection_references(&selection_state), 0);

        buffer.perform_line_operation(
            1,
            LineOperation::Insert {
                str: Arc::new("edited ".to_string()),
                begin: 0,
                end: 0,
                style: Style::DEFAULT.into(),
            },
        );
        assert!(buffer.link_keys().next().is_none());
        assert_eq!(shared.borrow().selection_references(&selection_state), 0);
        assert_eq!(
            buffer.find_recent_word_by_prefix("edit", None, &[], 10),
            None
        );
    }

    #[test]
    fn temporarily_hidden_whole_line_can_reveal() {
        use smudgy_core::session::styled_line::LinkVisibilityExpire;

        let visibility = LinkVisibility {
            action: LinkVisibilityAction::Reveal,
            delay_ms: Some(0),
            expire: LinkVisibilityExpire {
                input: false,
                prompt: false,
                output: false,
                output_delay_ms: 0,
            },
            whole_line: true,
        };
        let (line, _) = protocol_line(None, Some(visibility), false);
        let mut buffer = TerminalBuffer::new_with_max_lines(NonZeroUsize::new(10).unwrap());
        buffer.push_line(line);
        let link_state = buffer.link_state();
        let key = link_state.borrow().by_line[&1][0];
        assert!(link_state.borrow().line_concealed(1));

        link_state
            .borrow_mut()
            .update_visibility_timers(Instant::now() + Duration::from_millis(1));
        assert!(!link_state.borrow().line_concealed(1));
        assert!(link_state.borrow().contains(key));
    }
}
