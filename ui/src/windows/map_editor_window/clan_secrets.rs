//! Clan Secrets in the editor: who owns a new Secret (Me, Members or the
//! Clan, as the server's actions allow), and a Clan Secret's page sections:
//! its badge, who reads it and why, its owners, and offers of its
//! ownership.
//!
//! Everything shown comes from the server as the caller may see it: a
//! reader learns the Secret's badge and their own actions; who else reads it
//! needs `manage_access`, and offering ownership, removing an owner, giving
//! up one's own ownership and making a Member-owned Secret Clan-owned need
//! `manage_ownership` (clans.md §8.5).

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use iced::alignment::Vertical;
use iced::widget::{Column, button, checkbox, column, container, row, space, text};
use iced::{Length, Task};
use smudgy_cloud::clan_maps::MAX_OFFER_RECIPIENTS;
use smudgy_cloud::clan_secrets::{
    AccessReason, NewSecretOwner, OfferRequest, OwnershipOffer, SecretAccess, SecretOwner,
    SecretReader, authority_action, creation_action, ownership,
};
use smudgy_cloud::cloud_api::secret_action;
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{AreaId, CloudApiClient, CloudError, SourceBundle, SourceId, Uuid};

use crate::components::cloud_errors::display_error;
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::secrets::SecretsMessage;
use super::{Event, MapEditorWindow, Message};

// ===========================================================================
// Who owns a new Secret
// ===========================================================================

/// What one clan lets the viewer create on the map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClanOwners {
    pub clan_id: Uuid,
    pub name: String,
    /// `secret.create_member_owned` on the map.
    pub members: bool,
    /// `secret.create_clan_owned` on the map.
    pub clan: bool,
}

/// Who a new Secret belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    Me,
    Members(Uuid),
    Clan(Uuid),
}

/// One choice of the "Owner" picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerOption {
    pub kind: OwnerKind,
    pub label: String,
}

impl fmt::Display for OwnerOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

/// The owners a new Secret may have, in picker order: Me where the viewer
/// owns the user's map, then each clan's Members and Clan. A clan's name
/// joins the label only when more than one clan is on offer.
#[must_use]
pub fn owner_options(me: bool, clans: &[ClanOwners]) -> Vec<OwnerOption> {
    let mut options = Vec::new();
    if me {
        options.push(OwnerOption {
            kind: OwnerKind::Me,
            label: crate::i18n::t!("mapper-secret-owner-me"),
        });
    }
    let named = clans
        .iter()
        .filter(|clan| clan.members || clan.clan)
        .count()
        > 1;
    for clan in clans {
        if clan.members {
            options.push(OwnerOption {
                kind: OwnerKind::Members(clan.clan_id),
                label: if named {
                    crate::i18n::t!("mapper-secret-owner-members-of", "clan" => clan.name.clone())
                } else {
                    crate::i18n::t!("mapper-secret-owner-members")
                },
            });
        }
        if clan.clan {
            options.push(OwnerOption {
                kind: OwnerKind::Clan(clan.clan_id),
                label: if named {
                    crate::i18n::t!("mapper-secret-owner-clan-named", "clan" => clan.name.clone())
                } else {
                    crate::i18n::t!("mapper-secret-owner-clan")
                },
            });
        }
    }
    options
}

/// The owner picker's state for the New Secret form.
#[derive(Debug, Default)]
pub struct OwnerPick {
    /// The map the choices were asked for.
    pub area: Option<AreaId>,
    /// Whether the viewer may make an owner Secret here.
    pub me: bool,
    /// What each clan allows; `None` while loading.
    pub clans: Option<Result<Vec<ClanOwners>, String>>,
    pub picked: Option<OwnerKind>,
}

impl OwnerPick {
    #[must_use]
    pub fn options(&self) -> Vec<OwnerOption> {
        let clans = match &self.clans {
            Some(Ok(clans)) => clans.as_slice(),
            _ => &[],
        };
        owner_options(self.me, clans)
    }

    /// The picked owner while it is still on offer, else the first choice:
    /// an owner Secret where the viewer may make one.
    #[must_use]
    pub fn chosen(&self) -> Option<OwnerOption> {
        let options = self.options();
        options
            .iter()
            .find(|option| Some(option.kind) == self.picked)
            .or_else(|| options.first())
            .cloned()
    }

    /// What the server is asked to create; `None` while the choices load
    /// or when there is none.
    #[must_use]
    pub fn new_owner(&self) -> Option<NewSecretOwner> {
        match self.chosen()?.kind {
            OwnerKind::Me => Some(NewSecretOwner::Me),
            OwnerKind::Members(clan_id) => Some(NewSecretOwner::Members { clan_id }),
            OwnerKind::Clan(clan_id) => Some(NewSecretOwner::Clan { clan_id }),
        }
    }

    /// Whether the choices are still loading.
    #[must_use]
    pub fn loading(&self) -> bool {
        self.clans.is_none()
    }
}

/// Whether the viewer may start a new Secret on the map: the owner of a
/// user's map, or a member holding a clan's creation action there.
#[must_use]
pub fn can_create(area: &AreaCache) -> bool {
    let meta = area.meta();
    (area.is_owned() && meta.clan_id.is_none())
        || meta.actions.as_ref().is_some_and(|actions| {
            actions.contains(creation_action::MEMBER_OWNED)
                || actions.contains(creation_action::CLAN_OWNED)
        })
}

/// Asks each clan with a say over the map what the viewer may create there:
/// a clan's own map answers from its header; a map filed in clans by link
/// asks each clan's access index. Clans that refuse are left out.
async fn load_owners(
    client: CloudApiClient,
    area_id: AreaId,
    clans: Vec<(Uuid, String, Option<BTreeSet<String>>)>,
) -> Result<Vec<ClanOwners>, String> {
    let mut found = Vec::new();
    for (clan_id, name, known) in clans {
        let actions = match known {
            Some(actions) => actions,
            None => match client.clan_map_resources(clan_id).await {
                Ok(rows) => rows
                    .into_iter()
                    .find(|row| row.id == area_id)
                    .map(|row| row.actions)
                    .unwrap_or_default(),
                Err(CloudError::NotFoundOrNoAccess) => continue,
                Err(error) => return Err(display_error(&error)),
            },
        };
        let members = actions.contains(creation_action::MEMBER_OWNED);
        let clan = actions.contains(creation_action::CLAN_OWNED);
        if members || clan {
            found.push(ClanOwners {
                clan_id,
                name,
                members,
                clan,
            });
        }
    }
    Ok(found)
}

// ===========================================================================
// A Clan Secret's page
// ===========================================================================

/// What a Clan Secret's page shows beyond the Secret itself.
#[derive(Debug, Clone)]
pub struct PageData {
    /// Who reads it and why, as the caller may see it.
    pub access: Option<SecretAccess>,
    /// Pending ownership offers, for holders of `manage_ownership`.
    pub offers: Vec<OwnershipOffer>,
    /// Group names, for access reasons.
    pub groups: HashMap<Uuid, String>,
    /// A Member-owned Secret's recorded owners, oldest first, for holders
    /// of `manage_ownership`.
    pub owners: Vec<SecretOwner>,
    /// The clan's owners the viewer can see (its member directory needs
    /// `clan.read_members`), who may accept it for the clan.
    pub clan_owners: BTreeSet<Uuid>,
}

/// The open Clan Secret page.
#[derive(Debug)]
pub struct SecretPage {
    pub area_id: AreaId,
    pub source: SourceId,
    /// The viewer's actions and the Secret's badge when the page loaded;
    /// the page loads again when either changes.
    pub seen: (BTreeSet<String>, Option<String>),
    /// `None` while loading.
    pub data: Option<Result<PageData, String>>,
    pub offering: Option<OfferDraft>,
    /// An ownership change waiting for the viewer to confirm it.
    pub confirming: Option<Confirm>,
    pub busy: bool,
    pub error: Option<String>,
}

/// An ownership change the page asks about before it happens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Another recorded owner stops owning it.
    RemoveOwner(Uuid),
    /// The viewer stops owning it.
    GiveUp,
    /// It becomes Clan-owned once the picked clan owner accepts; the
    /// viewer, when they are a clan owner, accepts at once.
    MakeClanOwned(Option<Uuid>),
}

/// The "Offer ownership…" form.
#[derive(Debug, Default)]
pub struct OfferDraft {
    pub picked: BTreeSet<Uuid>,
    /// Between members: the recipients replace the current owners.
    pub replace: bool,
}

#[derive(Debug, Clone)]
pub enum PageMessage {
    Loaded(AreaId, SourceId, Box<Result<PageData, String>>),
    OfferStarted,
    OfferToggled(Uuid, bool),
    ReplaceToggled(bool),
    OfferSubmitted,
    OfferClosed,
    Withdraw(Uuid),
    RemoveOwnerStarted(Uuid),
    GiveUpStarted,
    MakeClanOwnedStarted,
    ClanOwnerPicked(Uuid),
    ConfirmCancelled,
    Confirmed,
    /// An offer, withdrawal or ownership change finished; the page reloads.
    Changed(AreaId, SourceId, Result<(), String>),
}

fn page_message(message: PageMessage) -> Message {
    Message::Secrets(SecretsMessage::Clan(message))
}

/// A Clan Secret's bundle on the map.
fn clan_bundle(area: &AreaCache, source: SourceId) -> Option<&SourceBundle> {
    area.meta()
        .sources
        .iter()
        .find(|bundle| bundle.source == source && bundle.clan_id.is_some())
}

/// Loads a Clan Secret's page. `offers` (`manage_ownership`) adds its
/// pending offers; `owners`, on a Member-owned one, its recorded owners and
/// the clan's owners who may accept it for the clan.
fn load_page(
    client: CloudApiClient,
    area_id: AreaId,
    source: SourceId,
    clan_id: Uuid,
    offers: bool,
    owners: bool,
) -> Task<Message> {
    Task::perform(
        async move {
            let access = match client.secret_access(&source).await {
                Ok(access) => Some(access),
                Err(CloudError::NotFoundOrNoAccess) => None,
                Err(error) => return Err(display_error(&error)),
            };
            let offers = if offers {
                match client.secret_offers(&source).await {
                    Ok(offers) => offers,
                    Err(CloudError::NotFoundOrNoAccess) => Vec::new(),
                    Err(error) => return Err(display_error(&error)),
                }
            } else {
                Vec::new()
            };
            let groups = client
                .clan_groups(clan_id)
                .await
                .map(|groups| {
                    groups
                        .into_iter()
                        .map(|group| (group.id, group.name))
                        .collect()
                })
                .unwrap_or_default();
            let (owners, clan_owners) = if owners {
                let recorded = match client.secret_owners(&source).await {
                    Ok(owners) => owners,
                    Err(CloudError::NotFoundOrNoAccess) => Vec::new(),
                    Err(error) => return Err(display_error(&error)),
                };
                // The directory needs `clan.read_members`; without it the
                // viewer knows only whether they are a clan owner.
                let clan_owners = client
                    .clan_members(clan_id)
                    .await
                    .map(|members| {
                        members
                            .into_iter()
                            .filter(|member| member.is_owner)
                            .map(|member| member.user_id)
                            .collect()
                    })
                    .unwrap_or_default();
                (recorded, clan_owners)
            } else {
                (Vec::new(), BTreeSet::new())
            };
            Ok(PageData {
                access,
                offers,
                groups,
                owners,
                clan_owners,
            })
        },
        move |result| page_message(PageMessage::Loaded(area_id, source, Box::new(result))),
    )
}

impl MapEditorWindow {
    /// Starts the New Secret form's owner choices for `area`.
    pub(super) fn start_owner_pick(&mut self, area: &AreaCache) -> Task<Message> {
        let area_id = *area.get_id();
        let meta = area.meta();
        let me = area.is_owned() && meta.clan_id.is_none();
        let clans: Vec<(Uuid, String, Option<BTreeSet<String>>)> = match meta.clan_id {
            Some(clan_id) => vec![(
                clan_id,
                self.clans.name(clan_id).unwrap_or_default(),
                Some(meta.actions.clone().unwrap_or_default()),
            )],
            None => Vec::new(),
        };
        let loading = !clans.is_empty();
        self.secrets.owners = OwnerPick {
            area: Some(area_id),
            me,
            clans: (!loading).then(|| Ok(Vec::new())),
            picked: None,
        };
        if !loading {
            return Task::none();
        }
        let client = self.cloud.client.clone();
        Task::perform(load_owners(client, area_id, clans), move |result| {
            Message::Secrets(SecretsMessage::OwnersLoaded(area_id, result))
        })
    }

    /// Opens the Clan parts of Secret `source`'s page and loads them; a
    /// Secret that is not a Clan Secret has none.
    pub(super) fn open_secret_page(&mut self, area_id: AreaId, source: SourceId) -> Task<Message> {
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&area_id) else {
            self.secrets.page = None;
            return Task::none();
        };
        let Some(bundle) = clan_bundle(&area, source) else {
            self.secrets.page = None;
            return Task::none();
        };
        let clan_id = bundle.clan_id.expect("a Clan Secret's bundle");
        let offers = bundle.can(authority_action::MANAGE_OWNERSHIP);
        let owners = offers && bundle.ownership.as_deref() == Some(ownership::MEMBERS);
        self.secrets.page = Some(SecretPage {
            area_id,
            source,
            seen: (bundle.actions.clone(), bundle.ownership.clone()),
            data: None,
            offering: None,
            confirming: None,
            busy: false,
            error: None,
        });
        load_page(
            self.cloud.client.clone(),
            area_id,
            source,
            clan_id,
            offers,
            owners,
        )
    }

    /// Keeps the open Clan Secret page in step with the map: it closes when
    /// the Secret goes, and loads again when the viewer's actions on it or
    /// its badge change (an offer accepted, a grant changed).
    pub(super) fn refresh_secret_page(&mut self) -> Task<Message> {
        let Some(page) = self.secrets.page.as_ref().filter(|page| !page.busy) else {
            return Task::none();
        };
        let (area_id, source) = (page.area_id, page.source);
        let now = {
            let atlas = self.mapper.get_current_atlas();
            atlas.get_area(&area_id).map(|area| {
                clan_bundle(&area, source)
                    .map(|bundle| (bundle.actions.clone(), bundle.ownership.clone()))
            })
        };
        match now {
            // The map is not at hand right now; nothing to compare.
            None => Task::none(),
            Some(None) => {
                self.secrets.page = None;
                Task::none()
            }
            Some(Some(seen)) if seen != page.seen => self.open_secret_page(area_id, source),
            Some(Some(_)) => Task::none(),
        }
    }

    pub(super) fn update_clan_page(&mut self, message: PageMessage) -> Update<Message, Event> {
        match message {
            PageMessage::Loaded(area_id, source, result) => {
                if let Some(page) = self.secrets.page.as_mut()
                    && page.area_id == area_id
                    && page.source == source
                {
                    page.data = Some(*result);
                    page.busy = false;
                }
            }
            PageMessage::Changed(area_id, source, result) => {
                let open = self
                    .secrets
                    .page
                    .as_ref()
                    .is_some_and(|page| page.area_id == area_id && page.source == source);
                if !open {
                    return Update::none();
                }
                match result {
                    Ok(()) => {
                        // What the viewer may do there changed: the map's
                        // projection follows on the next sync.
                        self.mapper.sync_now();
                        let task = self.open_secret_page(area_id, source);
                        return Update::with_task(task);
                    }
                    Err(error) => {
                        if let Some(page) = self.secrets.page.as_mut() {
                            page.busy = false;
                            page.error = Some(error);
                        }
                    }
                }
            }
            PageMessage::OfferStarted => {
                if let Some(page) = self.secrets.page.as_mut() {
                    page.offering = Some(OfferDraft::default());
                    page.confirming = None;
                    page.error = None;
                }
            }
            PageMessage::RemoveOwnerStarted(user) => self.start_confirm(Confirm::RemoveOwner(user)),
            PageMessage::GiveUpStarted => self.start_confirm(Confirm::GiveUp),
            PageMessage::MakeClanOwnedStarted => {
                let only = self
                    .clan_owner_candidates()
                    .and_then(|candidates| (candidates.len() == 1).then(|| candidates[0].0));
                self.start_confirm(Confirm::MakeClanOwned(only));
            }
            PageMessage::ClanOwnerPicked(user) => {
                if let Some(page) = self.secrets.page.as_mut()
                    && matches!(page.confirming, Some(Confirm::MakeClanOwned(_)))
                {
                    page.confirming = Some(Confirm::MakeClanOwned(Some(user)));
                }
            }
            PageMessage::ConfirmCancelled => {
                if let Some(page) = self.secrets.page.as_mut() {
                    page.confirming = None;
                    page.error = None;
                }
            }
            PageMessage::Confirmed => return self.confirm_ownership_change(),
            PageMessage::OfferClosed => {
                if let Some(page) = self.secrets.page.as_mut() {
                    page.offering = None;
                    page.error = None;
                }
            }
            PageMessage::OfferToggled(user, on) => {
                if let Some(draft) = self
                    .secrets
                    .page
                    .as_mut()
                    .and_then(|page| page.offering.as_mut())
                {
                    super::clan_map_share::toggle_recipient(&mut draft.picked, user, on);
                }
            }
            PageMessage::ReplaceToggled(replace) => {
                if let Some(draft) = self
                    .secrets
                    .page
                    .as_mut()
                    .and_then(|page| page.offering.as_mut())
                {
                    draft.replace = replace;
                }
            }
            PageMessage::OfferSubmitted => {
                let Some(page) = self.secrets.page.as_mut() else {
                    return Update::none();
                };
                let Some(draft) = page
                    .offering
                    .as_ref()
                    .filter(|draft| !draft.picked.is_empty())
                else {
                    return Update::none();
                };
                if page.busy {
                    return Update::none();
                }
                let request =
                    OfferRequest::to_members(draft.picked.iter().copied().collect(), draft.replace);
                page.busy = true;
                page.error = None;
                page.offering = None;
                let (area_id, source) = (page.area_id, page.source);
                let client = self.cloud.client.clone();
                return Update::with_task(Task::perform(
                    async move {
                        client
                            .offer_secret_ownership(&source, &request)
                            .await
                            .map(|_| ())
                            .map_err(|error| display_error(&error))
                    },
                    move |result| page_message(PageMessage::Changed(area_id, source, result)),
                ));
            }
            PageMessage::Withdraw(offer_id) => {
                let Some(page) = self.secrets.page.as_mut().filter(|page| !page.busy) else {
                    return Update::none();
                };
                page.busy = true;
                page.error = None;
                let (area_id, source) = (page.area_id, page.source);
                let client = self.cloud.client.clone();
                return Update::with_task(Task::perform(
                    async move {
                        client
                            .withdraw_secret_offer(&source, offer_id)
                            .await
                            .map_err(|error| display_error(&error))
                    },
                    move |result| page_message(PageMessage::Changed(area_id, source, result)),
                ));
            }
        }
        Update::none()
    }

    fn start_confirm(&mut self, confirm: Confirm) {
        if let Some(page) = self.secrets.page.as_mut().filter(|page| !page.busy) {
            page.offering = None;
            page.error = None;
            page.confirming = Some(confirm);
        }
    }

    /// Whether the viewer is one of the Secret's clan's owners.
    fn is_clan_owner(&self, clan_id: Uuid) -> bool {
        self.clans
            .clans
            .iter()
            .any(|clan| clan.id == clan_id && clan.is_owner)
    }

    /// The clan owners who may accept the open page's Member-owned Secret
    /// for the clan, with their names; `None` while it loads.
    fn clan_owner_candidates(&self) -> Option<Vec<(Uuid, String)>> {
        let page = self.secrets.page.as_ref()?;
        let Some(Ok(data)) = &page.data else {
            return None;
        };
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(&page.area_id)?;
        let clan_id = clan_bundle(&area, page.source)?.clan_id?;
        Some(clan_owner_candidates(
            data.access.as_ref(),
            &data.clan_owners,
            self.me(),
            self.is_clan_owner(clan_id),
        ))
    }

    /// Carries out the confirmed ownership change on the open page.
    fn confirm_ownership_change(&mut self) -> Update<Message, Event> {
        let Some(me) = self.me() else {
            return Update::none();
        };
        let Some(page) = self.secrets.page.as_mut().filter(|page| !page.busy) else {
            return Update::none();
        };
        let Some(confirm) = page.confirming.clone() else {
            return Update::none();
        };
        let (area_id, source) = (page.area_id, page.source);
        let name = {
            let atlas = self.mapper.get_current_atlas();
            atlas
                .get_area(&area_id)
                .and_then(|area| clan_bundle(&area, source).and_then(|bundle| bundle.name.clone()))
                .unwrap_or_default()
        };
        let Some(page) = self.secrets.page.as_mut() else {
            return Update::none();
        };
        let change = match confirm {
            Confirm::RemoveOwner(user) => OwnershipChange::Remove(user),
            Confirm::GiveUp => OwnershipChange::Remove(me),
            Confirm::MakeClanOwned(Some(recipient)) => OwnershipChange::ToClan {
                recipient,
                accept: recipient == me,
            },
            Confirm::MakeClanOwned(None) => return Update::none(),
        };
        page.busy = true;
        page.error = None;
        page.confirming = None;
        let client = self.cloud.client.clone();
        Update::with_task(Task::perform(
            async move {
                change
                    .run(&client, &source)
                    .await
                    .map_err(|error| ownership_error(&error, &name))
            },
            move |result| page_message(PageMessage::Changed(area_id, source, result)),
        ))
    }

    /// The signed-in user's ID.
    fn me(&self) -> Option<Uuid> {
        self.cloud
            .snapshot
            .get()
            .profile
            .as_ref()
            .map(|profile| profile.id)
    }
}

/// An ownership change the server carries out.
#[derive(Debug, Clone, Copy)]
enum OwnershipChange {
    /// A recorded owner, the viewer included, stops owning it.
    Remove(Uuid),
    /// An offer to make it Clan-owned, accepted at once when the viewer
    /// is the clan owner it names.
    ToClan { recipient: Uuid, accept: bool },
}

impl OwnershipChange {
    async fn run(self, client: &CloudApiClient, source: &SourceId) -> Result<(), CloudError> {
        match self {
            Self::Remove(user) => client.remove_secret_owner(source, user).await,
            Self::ToClan { recipient, accept } => {
                let offer = client
                    .offer_secret_ownership(source, &OfferRequest::to_clan(recipient))
                    .await?;
                if accept {
                    client.accept_secret_offer(source, offer.id).await?;
                }
                Ok(())
            }
        }
    }
}

/// An ownership change's refusal in words: the last owner can't go, and
/// anything else the server won't do (an owner or offer that changed
/// meanwhile, someone who no longer qualifies) is one plain line.
fn ownership_error(error: &CloudError, secret: &str) -> String {
    match error {
        CloudError::LastOwner => crate::i18n::t!("mapper-owner-last"),
        CloudError::NotFoundOrNoAccess => {
            crate::i18n::t!("mapper-ownership-refused", "secret" => secret)
        }
        other => display_error(other),
    }
}

/// Whether the viewer may remove `user` as an owner: another recorded
/// owner, while the Secret keeps one.
#[must_use]
pub fn may_remove_owner(owners: &[SecretOwner], me: Option<Uuid>, user: Uuid) -> bool {
    Some(user) != me && owners.len() > 1 && owners.iter().any(|owner| owner.user_id == user)
}

/// Whether the viewer may give up their own ownership: they are a recorded
/// owner, and not the last.
#[must_use]
pub fn may_give_up(owners: &[SecretOwner], me: Option<Uuid>) -> bool {
    owners.len() > 1 && owners.iter().any(|owner| Some(owner.user_id) == me)
}

/// Who may accept a Member-owned Secret for its clan: its readers the
/// viewer sees who are clan owners, the viewer first when they are one.
#[must_use]
pub fn clan_owner_candidates(
    access: Option<&SecretAccess>,
    clan_owners: &BTreeSet<Uuid>,
    me: Option<Uuid>,
    me_clan_owner: bool,
) -> Vec<(Uuid, String)> {
    let mut candidates: Vec<(Uuid, String)> = access
        .into_iter()
        .flat_map(|access| &access.members)
        .filter(|reader| {
            clan_owners.contains(&reader.user_id) || (Some(reader.user_id) == me && me_clan_owner)
        })
        .map(|reader| (reader.user_id, reader_name(reader, me)))
        .collect();
    candidates.sort_by_key(|(user, _)| Some(*user) != me);
    candidates
}

// ===========================================================================
// Views
// ===========================================================================

fn muted(theme: &crate::Theme) -> text::Style {
    builtins::text::muted(theme)
}

/// The New Secret form's owner line: a picker when there is a choice, the
/// one owner otherwise, and what the chosen owner means.
pub fn owner_line(window: &MapEditorWindow) -> Option<ThemedElement<'_, Message>> {
    let pick = &window.secrets.owners;
    if pick.loading() {
        return Some(
            row![
                text(crate::i18n::t!("mapper-secret-owner"))
                    .size(12)
                    .style(muted),
                text(crate::i18n::t!("state-loading")).size(12).style(muted),
            ]
            .spacing(8)
            .into(),
        );
    }
    if let Some(Err(error)) = &pick.clans {
        return Some(
            text(error.clone())
                .size(12)
                .style(builtins::text::danger)
                .into(),
        );
    }
    let options = pick.options();
    let Some(chosen) = pick.chosen() else {
        return Some(
            text(crate::i18n::t!("mapper-secret-owner-none"))
                .size(12)
                .style(muted)
                .into(),
        );
    };
    // An owner Secret where it is the only choice reads as before.
    if options.len() == 1 && chosen.kind == OwnerKind::Me {
        return None;
    }
    let owner: ThemedElement<'_, Message> = if options.len() > 1 {
        iced::widget::pick_list(options, Some(chosen.clone()), |option| {
            Message::Secrets(SecretsMessage::OwnerPicked(option))
        })
        .text_size(12)
        .padding([3, 8])
        .into()
    } else {
        text(chosen.label.clone()).size(12).into()
    };
    let mut lines = column![
        row![
            text(crate::i18n::t!("mapper-secret-owner"))
                .size(12)
                .style(muted),
            owner,
        ]
        .spacing(8)
        .align_y(Vertical::Center)
    ]
    .spacing(4);
    let hint = match chosen.kind {
        OwnerKind::Me => None,
        OwnerKind::Members(_) => Some(crate::i18n::t!("mapper-secret-owner-members-help")),
        OwnerKind::Clan(_) => Some(crate::i18n::t!("mapper-secret-owner-clan-help")),
    };
    if let Some(hint) = hint {
        lines = lines.push(text(hint).size(11).style(muted));
    }
    Some(lines.into())
}

/// The lines under a Secret's name on its page: a Clan Secret's badge, and
/// a reader who may not change it is told so.
pub fn page_header<'a>(
    window: &MapEditorWindow,
    area: &AreaCache,
    source: SourceId,
) -> Column<'a, Message, crate::Theme> {
    let mut lines = Column::new().spacing(4);
    let Some(bundle) = area
        .meta()
        .sources
        .iter()
        .find(|bundle| bundle.source == source)
    else {
        return lines;
    };
    if let Some(clan_id) = bundle.clan_id {
        let clan = window.clans.name(clan_id).unwrap_or_default();
        let badge = if bundle.ownership.as_deref() == Some(ownership::CLAN) {
            crate::i18n::t!("mapper-secret-clan-owned", "clan" => clan)
        } else {
            crate::i18n::t!("mapper-secret-member-owned", "clan" => clan)
        };
        lines = lines.push(text(badge).size(12).style(muted));
    }
    if !super::secrets::can_write(area, source) {
        lines = lines.push(
            text(crate::i18n::t!("mapper-secret-read-only"))
                .size(12)
                .style(muted),
        );
    }
    lines
}

/// The level a set of Secret actions reads as: the grant presets' names,
/// with access management and copying said apart. `copy` is in no preset,
/// so it never changes the name it follows.
fn level(actions: &[String]) -> String {
    let has = |action: &str| actions.iter().any(|held| held == action);
    let content = if has("remove") {
        Some(crate::i18n::t!("mapper-secret-level-editor"))
    } else if has("add") || has("edit") {
        Some(crate::i18n::t!("mapper-secret-level-contributor"))
    } else {
        None
    };
    let level = match (content, has("manage_access")) {
        (Some(content), true) => crate::i18n::t!("mapper-secret-level-manages", "level" => content),
        (Some(content), false) => content,
        (None, true) => crate::i18n::t!("mapper-secret-level-access-manager"),
        (None, false) => crate::i18n::t!("mapper-secret-level-reader"),
    };
    if has(secret_action::COPY) {
        crate::i18n::t!("mapper-secret-level-copies", "level" => level)
    } else {
        level
    }
}

/// Why a reader reads the Secret, in words.
pub(super) fn reason_label(
    reason: &AccessReason,
    groups: &HashMap<Uuid, String>,
) -> Option<String> {
    Some(match reason {
        AccessReason::Owner => crate::i18n::t!("mapper-access-owner"),
        AccessReason::ClanOwner => crate::i18n::t!("mapper-access-clan-owner"),
        AccessReason::Grant { actions, .. } => {
            crate::i18n::t!("mapper-access-grant", "level" => level(actions))
        }
        AccessReason::Group {
            actions, group_id, ..
        } => match groups.get(group_id) {
            Some(group) => crate::i18n::t!(
                "mapper-access-group",
                "level" => level(actions),
                "group" => group.clone()
            ),
            None => crate::i18n::t!("mapper-access-group-unnamed", "level" => level(actions)),
        },
        AccessReason::ClanGrants { actions } => {
            crate::i18n::t!("mapper-access-clan-grants", "level" => level(actions))
        }
        AccessReason::Other => return None,
    })
}

pub(super) fn reader_name(reader: &SecretReader, me: Option<Uuid>) -> String {
    let name = reader
        .nickname
        .clone()
        .unwrap_or_else(|| crate::i18n::t!("social-no-nickname"));
    if Some(reader.user_id) == me {
        crate::i18n::t!("mapper-access-you", "name" => name)
    } else {
        name
    }
}

fn heading<'a>(label: String) -> ThemedElement<'a, Message> {
    text(label).size(13).into()
}

/// The Access and Ownership sections of a Clan Secret's page.
pub fn sections<'a>(
    window: &'a MapEditorWindow,
    area: &AreaCache,
    source: SourceId,
) -> Column<'a, Message, crate::Theme> {
    let mut content = Column::new().spacing(10);
    let Some(bundle) = clan_bundle(area, source) else {
        return content;
    };
    let Some(page) = window
        .secrets
        .page
        .as_ref()
        .filter(|page| page.area_id == *area.get_id() && page.source == source)
    else {
        return content;
    };
    let data = match &page.data {
        None => {
            return content.push(text(crate::i18n::t!("state-loading")).size(12).style(muted));
        }
        Some(Err(error)) => {
            return content.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(data)) => data,
    };
    let me = window.me();
    let names: HashMap<Uuid, String> = data
        .access
        .iter()
        .flat_map(|access| &access.members)
        .filter_map(|reader| reader.nickname.clone().map(|name| (reader.user_id, name)))
        .collect();

    if let Some(access) = &data.access {
        let mut list = Column::new()
            .spacing(4)
            .push(heading(crate::i18n::t!("mapper-access")));
        for reader in &access.members {
            let reasons: Vec<String> = reader
                .reasons
                .iter()
                .filter_map(|reason| reason_label(reason, &data.groups))
                .collect();
            list = list.push(
                column![
                    text(reader_name(reader, me)).size(13),
                    text(reasons.join(" \u{00B7} ")).size(11).style(muted),
                ]
                .spacing(1),
            );
        }
        if access.members.len() == 1
            && access.members[0].user_id == me.unwrap_or_default()
            && !bundle.can("manage_access")
        {
            list = list.push(
                text(crate::i18n::t!("mapper-access-only-yours"))
                    .size(11)
                    .style(muted),
            );
        }
        if bundle.can("manage_access") {
            list = list.push(row![
                button(text(crate::i18n::t!("area-list-share-action")).size(12))
                    .style(builtins::button::subtle)
                    .on_press(Message::ShareSecretRequested(source)),
                space::horizontal(),
            ]);
        }
        content = content.push(list);
    }

    if !bundle.can(authority_action::MANAGE_OWNERSHIP) {
        return content;
    }
    let mut ownership_section = Column::new()
        .spacing(6)
        .push(heading(crate::i18n::t!("mapper-ownership")));
    let name_of = |user: Uuid, nickname: Option<&String>| {
        nickname
            .cloned()
            .or_else(|| names.get(&user).cloned())
            .unwrap_or_else(|| crate::i18n::t!("social-no-nickname"))
    };
    for offer in &data.offers {
        let people: Vec<String> = offer
            .recipients
            .iter()
            .map(|recipient| {
                let name = name_of(recipient.user_id, recipient.nickname.as_ref());
                if recipient.accepted {
                    crate::i18n::t!("mapper-offer-accepted", "name" => name)
                } else {
                    crate::i18n::t!("mapper-offer-waiting", "name" => name)
                }
            })
            .collect();
        let what = if offer.ownership == ownership::CLAN {
            crate::i18n::t!("mapper-offer-to-clan")
        } else if offer.replace {
            crate::i18n::t!("mapper-offer-replace")
        } else {
            crate::i18n::t!("mapper-offer-add")
        };
        ownership_section = ownership_section.push(
            container(
                row![
                    column![
                        text(what).size(12),
                        text(people.join(&crate::i18n::t!("mapper-multi-list-separator")))
                            .size(11)
                            .style(muted),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    button(text(crate::i18n::t!("mapper-offer-cancel")).size(12))
                        .style(builtins::button::secondary)
                        .on_press_maybe(
                            (!page.busy).then_some(page_message(PageMessage::Withdraw(offer.id)))
                        ),
                ]
                .spacing(8)
                .align_y(Vertical::Center),
            )
            .padding(6)
            .style(builtins::container::card),
        );
    }

    let member_owned = bundle.ownership.as_deref() != Some(ownership::CLAN);
    if member_owned && !data.owners.is_empty() {
        ownership_section = ownership_section.push(owners_list(page, data, me, &names));
    }
    let clan_id = bundle.clan_id.unwrap_or_default();
    // A Member-owned map takes no Clan-owned Secret.
    let on_member_map = area.meta().clan_ownership.ownership
        == Some(smudgy_cloud::clan_maps::MapOwnership::Members);
    let candidates = if member_owned && !on_member_map {
        clan_owner_candidates(
            data.access.as_ref(),
            &data.clan_owners,
            me,
            window.is_clan_owner(clan_id),
        )
    } else {
        Vec::new()
    };
    let secret = bundle.name.clone().unwrap_or_default();
    match (&page.offering, &page.confirming) {
        (Some(draft), _) => {
            ownership_section =
                ownership_section.push(offer_form(page, draft, data, me, member_owned));
        }
        (None, Some(confirm)) => {
            ownership_section = ownership_section.push(confirm_card(
                page,
                confirm,
                &secret,
                &candidates,
                me,
                |user| {
                    data.owners
                        .iter()
                        .find(|owner| owner.user_id == user)
                        .and_then(|owner| owner.nickname.clone())
                        .or_else(|| names.get(&user).cloned())
                        .unwrap_or_else(|| crate::i18n::t!("social-no-nickname"))
                },
            ));
        }
        (None, None) => {
            let mut actions = row![
                button(text(crate::i18n::t!("mapper-offer-ownership")).size(12))
                    .style(builtins::button::subtle)
                    .on_press_maybe(
                        (!page.busy).then_some(page_message(PageMessage::OfferStarted))
                    ),
            ]
            .spacing(8);
            if !candidates.is_empty() {
                actions = actions.push(
                    button(text(crate::i18n::t!("mapper-make-clan-owned")).size(12))
                        .style(builtins::button::subtle)
                        .on_press_maybe(
                            (!page.busy).then_some(page_message(PageMessage::MakeClanOwnedStarted)),
                        ),
                );
            }
            ownership_section = ownership_section.push(actions.push(space::horizontal()));
        }
    }
    if let Some(error) = &page.error {
        ownership_section =
            ownership_section.push(text(error.clone()).size(12).style(builtins::text::danger));
    }
    content.push(ownership_section)
}

/// A Member-owned Secret's recorded owners: Remove on each other owner and
/// "Give up ownership…" on the viewer's own row while another owner stays;
/// the viewer as its only owner is told how to hand it on.
fn owners_list<'a>(
    page: &'a SecretPage,
    data: &'a PageData,
    me: Option<Uuid>,
    names: &HashMap<Uuid, String>,
) -> ThemedElement<'a, Message> {
    let mut list = Column::new()
        .spacing(4)
        .push(text(crate::i18n::t!("mapper-owners")).size(12).style(muted));
    for owner in &data.owners {
        let name = owner
            .nickname
            .clone()
            .or_else(|| names.get(&owner.user_id).cloned())
            .unwrap_or_else(|| crate::i18n::t!("social-no-nickname"));
        let mut label = if Some(owner.user_id) == me {
            crate::i18n::t!("mapper-access-you", "name" => name)
        } else {
            name
        };
        if !owner.active {
            label = crate::i18n::t!("mapper-owner-inactive", "name" => label);
        }
        let mut line = row![text(label).size(13).width(Length::Fill)]
            .spacing(8)
            .align_y(Vertical::Center);
        if may_remove_owner(&data.owners, me, owner.user_id) {
            line =
                line.push(
                    button(text(crate::i18n::t!("mapper-owner-remove")).size(12))
                        .style(builtins::button::secondary)
                        .on_press_maybe((!page.busy).then_some(page_message(
                            PageMessage::RemoveOwnerStarted(owner.user_id),
                        ))),
                );
        } else if Some(owner.user_id) == me && may_give_up(&data.owners, me) {
            line = line.push(
                button(text(crate::i18n::t!("mapper-owner-give-up")).size(12))
                    .style(builtins::button::secondary)
                    .on_press_maybe(
                        (!page.busy).then_some(page_message(PageMessage::GiveUpStarted)),
                    ),
            );
        }
        list = list.push(line);
    }
    let only_me = data.owners.len() == 1 && Some(data.owners[0].user_id) == me;
    if only_me {
        list = list.push(
            text(crate::i18n::t!("mapper-owner-only-you"))
                .size(11)
                .style(muted),
        );
    }
    list.into()
}

/// The card asking to confirm an ownership change, saying plainly what
/// happens.
fn confirm_card<'a>(
    page: &'a SecretPage,
    confirm: &Confirm,
    secret: &str,
    candidates: &[(Uuid, String)],
    me: Option<Uuid>,
    owner_name: impl Fn(Uuid) -> String,
) -> ThemedElement<'a, Message> {
    let mut card = Column::new().spacing(8);
    let (question, action, ready) = match confirm {
        Confirm::RemoveOwner(user) => (
            crate::i18n::t!(
                "mapper-owner-remove-confirm",
                "name" => owner_name(*user),
                "secret" => secret
            ),
            crate::i18n::t!("mapper-owner-remove-action"),
            true,
        ),
        Confirm::GiveUp => (
            crate::i18n::t!("mapper-owner-give-up-confirm", "secret" => secret),
            crate::i18n::t!("mapper-owner-give-up-action"),
            true,
        ),
        Confirm::MakeClanOwned(picked) => {
            if candidates.len() > 1 {
                card = card.push(
                    text(crate::i18n::t!("mapper-make-clan-owned-pick"))
                        .size(12)
                        .style(muted),
                );
                for (user, name) in candidates {
                    let user = *user;
                    card = card.push(
                        iced::widget::radio(name.clone(), user, *picked, |user| {
                            page_message(PageMessage::ClanOwnerPicked(user))
                        })
                        .size(14)
                        .text_size(13),
                    );
                }
            }
            let question = match picked {
                Some(user) if Some(*user) == me => crate::i18n::t!(
                    "mapper-make-clan-owned-confirm-self",
                    "secret" => secret
                ),
                Some(user) => crate::i18n::t!(
                    "mapper-make-clan-owned-confirm",
                    "secret" => secret,
                    "name" => candidates
                        .iter()
                        .find(|(candidate, _)| candidate == user)
                        .map(|(_, name)| name.clone())
                        .unwrap_or_default()
                ),
                None => String::new(),
            };
            let action = if picked.is_some() && *picked == me {
                crate::i18n::t!("mapper-make-clan-owned-action")
            } else {
                crate::i18n::t!("mapper-offer-send")
            };
            (question, action, picked.is_some())
        }
    };
    if !question.is_empty() {
        card = card.push(text(question).size(13));
    }
    card = card.push(
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(12))
                .style(builtins::button::secondary)
                .on_press(page_message(PageMessage::ConfirmCancelled)),
            button(text(action).size(12))
                .style(builtins::button::primary)
                .on_press_maybe(
                    (ready && !page.busy).then_some(page_message(PageMessage::Confirmed))
                ),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );
    container(card)
        .padding(8)
        .style(builtins::container::card)
        .into()
}

/// Who may receive an offer: the members the caller sees reading the
/// Secret, but not the caller, and on a Member-owned Secret not its owners.
fn candidates(access: Option<&SecretAccess>, me: Option<Uuid>) -> Vec<&SecretReader> {
    access
        .into_iter()
        .flat_map(|access| &access.members)
        .filter(|reader| Some(reader.user_id) != me)
        .filter(|reader| !reader.reasons.contains(&AccessReason::Owner))
        .collect()
}

fn offer_form<'a>(
    page: &'a SecretPage,
    draft: &'a OfferDraft,
    data: &'a PageData,
    me: Option<Uuid>,
    member_owned: bool,
) -> ThemedElement<'a, Message> {
    let mut form = Column::new()
        .spacing(6)
        .push(text(crate::i18n::t!("mapper-offer-pick")).size(12));
    let people = candidates(data.access.as_ref(), me);
    if people.is_empty() {
        form = form.push(
            text(crate::i18n::t!("mapper-offer-nobody"))
                .size(12)
                .style(muted),
        );
    }
    let full = draft.picked.len() >= MAX_OFFER_RECIPIENTS;
    for reader in people {
        let user = reader.user_id;
        let checked = draft.picked.contains(&user);
        form = form.push(
            checkbox(checked)
                .label(reader_name(reader, me))
                .size(14)
                .text_size(13)
                .on_toggle_maybe(
                    (checked || !full)
                        .then_some(move |on| page_message(PageMessage::OfferToggled(user, on))),
                ),
        );
    }
    if full {
        form = form.push(
            text(crate::i18n::t!(
                "mapper-offer-recipient-limit",
                "count" => MAX_OFFER_RECIPIENTS
            ))
            .size(11)
            .style(muted),
        );
    }
    let effect = if member_owned {
        form = form.push(
            checkbox(draft.replace)
                .label(crate::i18n::t!("mapper-offer-replace-toggle"))
                .size(14)
                .text_size(12)
                .on_toggle(|on| page_message(PageMessage::ReplaceToggled(on))),
        );
        if draft.replace {
            crate::i18n::t!("mapper-offer-replace-help")
        } else {
            crate::i18n::t!("mapper-offer-add-help")
        }
    } else {
        crate::i18n::t!("mapper-offer-from-clan-help")
    };
    form = form.push(text(effect).size(11).style(muted));
    if draft.picked.len() > 1 {
        form = form.push(
            text(crate::i18n::t!("mapper-offer-joint-help"))
                .size(11)
                .style(muted),
        );
    }
    let ready = !draft.picked.is_empty() && !page.busy;
    form = form.push(
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(12))
                .style(builtins::button::secondary)
                .on_press(page_message(PageMessage::OfferClosed)),
            button(text(crate::i18n::t!("mapper-offer-send")).size(12))
                .style(builtins::button::primary)
                .on_press_maybe(ready.then_some(page_message(PageMessage::OfferSubmitted))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );
    container(form)
        .padding(8)
        .style(builtins::container::card)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clan(id: u128, members: bool, clan: bool) -> ClanOwners {
        ClanOwners {
            clan_id: Uuid::from_u128(id),
            name: format!("Clan {id}"),
            members,
            clan,
        }
    }

    fn kinds(options: &[OwnerOption]) -> Vec<OwnerKind> {
        options.iter().map(|option| option.kind).collect()
    }

    #[test]
    fn owners_follow_what_the_server_allows() {
        let one = Uuid::from_u128(1);
        // A user's own map, in no clan: only Me.
        assert_eq!(kinds(&owner_options(true, &[])), [OwnerKind::Me]);
        // A clan map: no Me; Clan only with its creation action.
        assert_eq!(
            kinds(&owner_options(false, &[clan(1, true, false)])),
            [OwnerKind::Members(one)]
        );
        assert_eq!(
            kinds(&owner_options(false, &[clan(1, true, true)])),
            [OwnerKind::Members(one), OwnerKind::Clan(one)]
        );
        // A user's map filed in a clan: Me first.
        assert_eq!(
            kinds(&owner_options(true, &[clan(1, false, true)])),
            [OwnerKind::Me, OwnerKind::Clan(one)]
        );
    }

    #[test]
    fn the_default_stays_an_owner_secret_where_allowed() {
        let mut pick = OwnerPick {
            me: true,
            clans: Some(Ok(vec![clan(1, true, true)])),
            ..OwnerPick::default()
        };
        assert_eq!(pick.new_owner(), Some(NewSecretOwner::Me));
        pick.picked = Some(OwnerKind::Clan(Uuid::from_u128(1)));
        assert_eq!(
            pick.new_owner(),
            Some(NewSecretOwner::Clan {
                clan_id: Uuid::from_u128(1),
            })
        );
        // A pick no longer on offer falls back to the first choice.
        pick.picked = Some(OwnerKind::Clan(Uuid::from_u128(9)));
        assert_eq!(pick.new_owner(), Some(NewSecretOwner::Me));
        // Nothing is created while the choices load.
        pick.clans = None;
        pick.me = false;
        assert_eq!(pick.new_owner(), None);
    }

    #[test]
    fn several_clans_name_themselves() {
        let options = owner_options(false, &[clan(1, true, false), clan(2, true, false)]);
        assert_eq!(options.len(), 2);
        assert_ne!(options[0].label, options[1].label);
    }

    fn recorded(id: u128, active: bool) -> SecretOwner {
        SecretOwner {
            user_id: Uuid::from_u128(id),
            nickname: None,
            active,
            added_at: chrono::DateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn owners_go_while_another_stays() {
        let me = Some(Uuid::from_u128(1));
        let two = [recorded(1, true), recorded(2, false)];
        // Another owner, dormant ones included, can be removed; the viewer
        // gives up their own instead.
        assert!(may_remove_owner(&two, me, Uuid::from_u128(2)));
        assert!(!may_remove_owner(&two, me, Uuid::from_u128(1)));
        assert!(may_give_up(&two, me));
        // Someone who isn't an owner is nobody to remove.
        assert!(!may_remove_owner(&two, me, Uuid::from_u128(3)));
        // The last owner stays.
        let one = [recorded(1, true)];
        assert!(!may_give_up(&one, me));
        assert!(!may_remove_owner(&one, None, Uuid::from_u128(1)));
        // A manager who doesn't own it has nothing to give up.
        assert!(!may_give_up(&two, Some(Uuid::from_u128(9))));
    }

    #[test]
    fn a_clan_owner_who_reads_it_may_accept_it_for_the_clan() {
        let me = Uuid::from_u128(1);
        let reader = |id: u128| SecretReader {
            user_id: Uuid::from_u128(id),
            nickname: Some(format!("user{id}")),
            actions: vec!["read".to_string()],
            reasons: vec![AccessReason::Owner],
        };
        let access = SecretAccess {
            secret_id: Uuid::from_u128(9),
            area_id: AreaId(Uuid::from_u128(8)),
            clan_id: Uuid::from_u128(7),
            ownership: ownership::MEMBERS.to_string(),
            members: vec![reader(2), reader(1), reader(3)],
        };
        let owners: BTreeSet<Uuid> = [Uuid::from_u128(3), Uuid::from_u128(4)].into();
        // Clan owners among its readers; one who doesn't read it can't.
        let ids = |candidates: Vec<(Uuid, String)>| -> Vec<Uuid> {
            candidates.into_iter().map(|(user, _)| user).collect()
        };
        assert_eq!(
            ids(clan_owner_candidates(
                Some(&access),
                &owners,
                Some(me),
                false
            )),
            [Uuid::from_u128(3)]
        );
        // The viewer, a clan owner, comes first even without the directory.
        assert_eq!(
            ids(clan_owner_candidates(
                Some(&access),
                &BTreeSet::new(),
                Some(me),
                true
            )),
            [me]
        );
        assert_eq!(
            ids(clan_owner_candidates(
                Some(&access),
                &owners,
                Some(me),
                true
            )),
            [me, Uuid::from_u128(3)]
        );
        assert!(clan_owner_candidates(None, &owners, Some(me), true).is_empty());
    }

    #[test]
    fn ownership_refusals_read_plainly_in_every_language() {
        let refused = ownership_error(&CloudError::NotFoundOrNoAccess, "Bookcase");
        assert!(refused.contains("Bookcase"), "{refused}");
        assert_eq!(
            ownership_error(&CloudError::LastOwner, "Bookcase"),
            crate::i18n::t!("mapper-owner-last")
        );
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            let checks = [
                smudgy_i18n::t!(translator, "mapper-owner-remove-confirm", "name" => "Arun", "secret" => "Bookcase"),
                smudgy_i18n::t!(translator, "mapper-owner-give-up-confirm", "secret" => "Bookcase"),
                smudgy_i18n::t!(translator, "mapper-make-clan-owned-confirm", "secret" => "Bookcase", "name" => "Mira"),
                smudgy_i18n::t!(translator, "mapper-make-clan-owned-confirm-self", "secret" => "Bookcase"),
                smudgy_i18n::t!(translator, "mapper-ownership-refused", "secret" => "Bookcase"),
            ];
            for text in checks {
                assert!(
                    text.contains("Bookcase") && !text.contains('⟦'),
                    "{}: {text}",
                    catalog.tag
                );
            }
            for key in [
                "mapper-owners",
                "mapper-owner-remove",
                "mapper-owner-remove-action",
                "mapper-owner-give-up",
                "mapper-owner-give-up-action",
                "mapper-owner-only-you",
                "mapper-owner-last",
                "mapper-make-clan-owned",
                "mapper-make-clan-owned-pick",
                "mapper-make-clan-owned-action",
            ] {
                let text = translator.translate(key);
                assert!(
                    !text.is_empty() && !text.contains('⟦'),
                    "{}: {key}",
                    catalog.tag
                );
                if catalog.tag != "en-US" {
                    assert_ne!(
                        text,
                        smudgy_i18n::Translator::default().translate(key),
                        "{} must translate {key}",
                        catalog.tag
                    );
                }
            }
        }
    }

    #[test]
    fn offers_go_to_other_readers_who_do_not_own_it() {
        let me = Uuid::from_u128(1);
        let reader = |id: u128, reasons: Vec<AccessReason>| SecretReader {
            user_id: Uuid::from_u128(id),
            nickname: None,
            actions: vec!["read".to_string()],
            reasons,
        };
        let access = SecretAccess {
            secret_id: Uuid::from_u128(9),
            area_id: AreaId(Uuid::from_u128(8)),
            clan_id: Uuid::from_u128(7),
            ownership: ownership::MEMBERS.to_string(),
            members: vec![
                reader(1, vec![AccessReason::Owner]),
                reader(2, vec![AccessReason::Owner]),
                reader(3, vec![AccessReason::ClanOwner]),
                reader(
                    4,
                    vec![AccessReason::ClanGrants {
                        actions: vec!["read".to_string()],
                    }],
                ),
            ],
        };
        let picked: Vec<Uuid> = candidates(Some(&access), Some(me))
            .into_iter()
            .map(|reader| reader.user_id)
            .collect();
        assert_eq!(picked, [Uuid::from_u128(3), Uuid::from_u128(4)]);
    }

    /// `copy` is in no preset: a grant of a preset and `copy` keeps the
    /// preset's name and says copying apart.
    #[test]
    fn copying_is_said_apart_from_the_preset() {
        let level_of = |names: &[&str]| {
            level(
                &names
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<String>>(),
            )
        };
        let copies =
            |level: String| crate::i18n::t!("mapper-secret-level-copies", "level" => level);
        assert_eq!(
            level_of(&["read", "add", "edit"]),
            crate::i18n::t!("mapper-secret-level-contributor")
        );
        assert_eq!(
            level_of(&["read", "add", "edit", "copy"]),
            copies(crate::i18n::t!("mapper-secret-level-contributor"))
        );
        assert_eq!(
            level_of(&["read", "copy"]),
            copies(crate::i18n::t!("mapper-secret-level-reader"))
        );
        assert_eq!(
            level_of(&["read", "manage_access", "copy"]),
            copies(crate::i18n::t!("mapper-secret-level-access-manager"))
        );
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            let text =
                smudgy_i18n::t!(translator, "mapper-secret-level-copies", "level" => "Editor");
            assert!(
                text.contains("Editor") && !text.contains('⟦'),
                "{}: {text}",
                catalog.tag
            );
            for key in ["mapper-secret-can-copy", "mapper-copy-secrets-none"] {
                let text = translator.translate(key);
                assert!(
                    !text.is_empty() && !text.contains('⟦'),
                    "{}: {key}",
                    catalog.tag
                );
                if catalog.tag != "en-US" {
                    assert_ne!(
                        text,
                        smudgy_i18n::Translator::default().translate(key),
                        "{} must translate {key}",
                        catalog.tag
                    );
                }
            }
        }
    }
}
