//! Symbolic transfer audiences. Fidelity: Cloudflare's authz/source-policy.ts
//! and authz/disclosure.ts. Groups describe future members, never today's roster.

use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use super::clans::{ClanGrantScope, ClanRecipient, ClanRecord};
use super::state::{AreaRecord, GrantRecord, MockState, SecretRecord};

pub const ACTIONS: [&str; 14] = [
    "read",
    "add",
    "edit",
    "remove",
    "manage_access",
    "copy",
    "rename",
    "delete",
    "manage_ownership",
    "share_read",
    "share_add",
    "share_edit",
    "share_remove",
    "share_copy",
];
const SHARED: [&str; 5] = ["read", "add", "edit", "remove", "copy"];
const MAP: [(&str, &str); 7] = [
    ("read", "area.read"),
    ("add", "area.add"),
    ("edit", "area.edit"),
    ("remove", "area.remove_content"),
    ("copy", "area.copy"),
    ("rename", "area.rename"),
    ("delete", "area.delete"),
];
const COLLECTION: [(&str, &str); 6] = [
    ("secret.read", "read"),
    ("secret.add", "add"),
    ("secret.edit", "edit"),
    ("secret.remove_content", "remove"),
    ("secret.manage_access", "manage_access"),
    ("secret.copy", "copy"),
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Atom {
    Member,
    Group(Uuid),
    User(Uuid),
}
pub type Audience = Vec<BTreeSet<Atom>>;
pub type Policy = BTreeMap<String, Audience>;

pub fn canonical(clauses: Audience) -> Audience {
    let mut result: Audience = Vec::new();
    for mut clause in clauses {
        if clause.iter().filter(|a| matches!(a, Atom::User(_))).count() > 1 {
            continue;
        }
        if clause.iter().any(|a| matches!(a, Atom::Group(_))) {
            clause.insert(Atom::Member);
        }
        if result.iter().any(|old| old.is_subset(&clause)) {
            continue;
        }
        result.retain(|old| !clause.is_subset(old));
        result.push(clause);
    }
    result.sort();
    result
}
pub fn union(a: &Audience, b: &Audience) -> Audience {
    canonical(a.iter().chain(b).cloned().collect())
}
pub fn intersection(a: &Audience, b: &Audience) -> Audience {
    canonical(
        a.iter()
            .flat_map(|x| b.iter().map(move |y| x.union(y).cloned().collect()))
            .collect(),
    )
}
pub fn increases(before: &Audience, after: &Audience) -> Audience {
    canonical(after.clone())
        .into_iter()
        .filter(|a| !before.iter().any(|b| b.is_subset(a)))
        .collect()
}
fn person(id: Uuid, member: bool) -> Audience {
    let mut clause = BTreeSet::from([Atom::User(id)]);
    if member {
        clause.insert(Atom::Member);
    }
    vec![clause]
}
fn group(clan: &ClanRecord, id: Uuid) -> Audience {
    clan.groups
        .iter()
        .find(|g| g.id == id)
        .map_or_else(Vec::new, |g| {
            canonical(vec![BTreeSet::from([if g.builtin == Some("members") {
                Atom::Member
            } else {
                Atom::Group(id)
            }])])
        })
}
fn recipient(clan: &ClanRecord, to: ClanRecipient) -> Audience {
    match to {
        ClanRecipient::User(id) => person(id, true),
        ClanRecipient::Group(id) => group(clan, id),
    }
}
fn owners(clan: &ClanRecord) -> Audience {
    canonical(
        clan.groups
            .iter()
            .filter(|g| g.builtin == Some("owners"))
            .map(|g| BTreeSet::from([Atom::Group(g.id)]))
            .collect(),
    )
}
fn give(policy: &mut Policy, who: &Audience, actions: &[&str]) {
    for action in actions {
        let old = policy.entry((*action).into()).or_default();
        *old = union(old, who);
    }
}
fn require(mut policy: Policy, prerequisite: &Audience) -> Policy {
    for who in policy.values_mut() {
        *who = intersection(who, prerequisite);
    }
    policy
}
pub fn audience(policy: &Policy, action: &str) -> Audience {
    policy.get(action).cloned().unwrap_or_default()
}
pub fn unfenced(st: &MockState, owner: Uuid, grantor: Uuid, grantee: Uuid) -> bool {
    st.are_friends(grantor, grantee)
        && !st.blocked_pair(grantor, grantee)
        && !st.blocked_pair(owner, grantee)
}
pub fn live(st: &MockState, grant: &GrantRecord) -> bool {
    let mut next = Some(grant);
    let mut seen = BTreeSet::new();
    while let Some(row) = next {
        if !seen.insert(row.id) || !unfenced(st, row.owner_id, row.grantor_id, row.grantee_id) {
            return false;
        }
        next = match row.parent_grant_id {
            None => None,
            Some(id) => match st.grant(id) {
                Some(parent) => Some(parent),
                None => return false,
            },
        };
    }
    true
}
fn personal_map(st: &MockState, area: &AreaRecord, atlas: Option<Uuid>) -> Policy {
    let mut result = Policy::new();
    give(&mut result, &person(area.user_id, false), &ACTIONS);
    for grant in st
        .grants
        .iter()
        .filter(|g| g.area_id == Some(area.id) || (atlas.is_some() && g.atlas_id == atlas))
        .filter(|g| live(st, g))
    {
        let who = person(grant.grantee_id, false);
        give(&mut result, &who, &["read"]);
        if grant.can_edit || grant.can_admin {
            give(&mut result, &who, &["add", "edit", "remove"]);
        }
        if grant.can_copy || grant.can_admin {
            give(&mut result, &who, &["copy"]);
        }
        if grant.can_admin {
            give(&mut result, &who, &["rename", "delete", "manage_ownership"]);
        }
        if grant.can_reshare || grant.can_admin {
            give(&mut result, &who, &["manage_access", "share_read"]);
            if grant.can_edit || grant.can_admin {
                give(
                    &mut result,
                    &who,
                    &["share_add", "share_edit", "share_remove"],
                );
            }
            if grant.can_copy || grant.can_admin {
                give(&mut result, &who, &["share_copy"]);
            }
        }
    }
    result
}
fn shared_secret(policy: &mut Policy) {
    for action in SHARED {
        policy.insert(
            format!("share_{action}"),
            intersection(
                &audience(policy, "manage_access"),
                &audience(policy, action),
            ),
        );
    }
}
fn clan_map(
    st: &MockState,
    clan: &ClanRecord,
    area: &AreaRecord,
    atlas: Option<Uuid>,
) -> (Policy, Policy) {
    let mut actions = Policy::new();
    let mut content = Policy::new();
    if clan.dissolved {
        return (actions, content);
    }
    let owner = if let Some(record) = &area.member_owned {
        if record.frozen {
            Vec::new()
        } else {
            canonical(
                record
                    .owners
                    .iter()
                    .flat_map(|(id, _)| person(*id, true))
                    .collect(),
            )
        }
    } else {
        owners(clan)
    };
    let mut placed = area.clone();
    placed.atlas_id = atlas;
    let resource = super::clan_maps::resource(&placed);
    if area.member_owned.is_some() {
        give(
            &mut actions,
            &owner,
            &super::clan_maps::MEMBER_OWNED_ACTIONS,
        );
    } else {
        for (_, action) in MAP {
            give(&mut actions, &owner, &[action]);
        }
        for (action, _) in COLLECTION {
            give(&mut actions, &owner, &[action]);
        }
        give(
            &mut actions,
            &owner,
            &["grant.manage", "grant.inspect", "area.share_external"],
        );
    }
    for grant in &clan.grants {
        if area.member_owned.is_some()
            && grant.scope != ClanGrantScope::Areas(BTreeSet::from([area.id]))
        {
            continue;
        }
        let who = recipient(clan, grant.recipient);
        let contribution = clan.contribution(grant, resource);
        for action in &contribution {
            give(&mut actions, &who, &[action]);
        }
        if contribution.contains("grant.manage") {
            for (action, right) in MAP.into_iter().filter(|(a, _)| SHARED.contains(a)) {
                if grant.may_grant.contains(right) {
                    give(&mut content, &who, &[&format!("share_{action}")]);
                }
            }
        }
    }
    for (action, right) in MAP {
        give(&mut content, &audience(&actions, right), &[action]);
    }
    give(&mut content, &owner, &ACTIONS);
    give(
        &mut content,
        &audience(&actions, "area.share_external"),
        &["share_read", "manage_access"],
    );
    give(
        &mut content,
        &audience(&actions, "grant.manage"),
        &["manage_access"],
    );
    for grant in st
        .grants
        .iter()
        .filter(|g| g.area_id == Some(area.id) && live(st, g))
    {
        let held = clan.actions_on(grant.grantor_id, resource);
        if held.contains("area.read") && held.contains("area.share_external") {
            give(&mut content, &person(grant.grantee_id, false), &["read"]);
        }
    }
    let read = audience(&content, "read");
    (actions, require(content, &read))
}

/// `None` is Map. Private is introduced through its author's source policy.
pub fn policy(
    st: &MockState,
    area: &AreaRecord,
    source: Option<&SecretRecord>,
    atlas: Option<Uuid>,
) -> Policy {
    let Some(clan) = area.clan_id.and_then(|id| st.clans.clans.get(&id)) else {
        let map = personal_map(st, area, atlas);
        let Some(secret) = source else {
            return map;
        };
        let mut result = Policy::new();
        give(&mut result, &person(area.user_id, false), &ACTIONS);
        for grant in secret
            .grants
            .iter()
            .filter(|g| unfenced(st, area.user_id, g.grantor_id, g.grantee_id))
        {
            let who = person(grant.grantee_id, false);
            give(&mut result, &who, &["read"]);
            for action in &grant.actions {
                give(&mut result, &who, &[action]);
            }
        }
        shared_secret(&mut result);
        return require(result, &audience(&map, "read"));
    };
    let (map, content) = clan_map(st, clan, area, atlas);
    let Some(secret) = source else {
        return content;
    };
    let mut result = Policy::new();
    let Some(record) = &secret.clan else {
        return result;
    };
    for grant in &record.grants {
        let who = recipient(clan, grant.recipient);
        give(&mut result, &who, &["read"]);
        if !record.frozen {
            for action in &grant.actions {
                give(&mut result, &who, &[action]);
            }
        }
    }
    if record.ownership == "clan" {
        for (collection, action) in COLLECTION {
            if !record.frozen || action == "read" {
                give(&mut result, &audience(&map, collection), &[action]);
            }
        }
    }
    let owner = if record.ownership == "clan" {
        owners(clan)
    } else {
        canonical(
            record
                .owners
                .iter()
                .flat_map(|(id, _)| person(*id, true))
                .collect(),
        )
    };
    give(&mut result, &owner, &ACTIONS);
    shared_secret(&mut result);
    let read = audience(&result, "read");
    require(require(result, &read), &audience(&map, "area.read"))
}

pub fn private(st: &MockState, area: &AreaRecord, author: Uuid, atlas: Option<Uuid>) -> Policy {
    let mut result = Policy::new();
    let read = if let Some(clan) = area.clan_id.and_then(|id| st.clans.clans.get(&id)) {
        give(&mut result, &person(author, true), &ACTIONS);
        audience(&clan_map(st, clan, area, atlas).0, "area.read")
    } else {
        give(&mut result, &person(author, false), &ACTIONS);
        audience(&personal_map(st, area, atlas), "read")
    };
    require(result, &read)
}

pub fn holds(st: &MockState, area: &AreaRecord, user: Uuid, policy: &Policy, action: &str) -> bool {
    let mut facts = BTreeSet::from([Atom::User(user)]);
    if let Some(clan) = area.clan_id.and_then(|id| st.clans.clans.get(&id)) {
        if clan.has_member(user) {
            facts.insert(Atom::Member);
        }
        facts.extend(clan.member_groups(user).into_iter().map(Atom::Group));
    }
    audience(policy, action)
        .iter()
        .any(|clause| clause.is_subset(&facts))
}
pub fn owns(st: &MockState, area: &AreaRecord, source: Option<&SecretRecord>, user: Uuid) -> bool {
    let Some(clan) = area.clan_id.and_then(|id| st.clans.clans.get(&id)) else {
        return area.user_id == user;
    };
    match source {
        None => super::clan_maps::has_authority(st, user, area),
        Some(secret) => secret.clan.as_ref().is_some_and(|s| {
            clan.has_member(user)
                && if s.ownership == "clan" {
                    clan.has_owner(user)
                } else {
                    s.owners.iter().any(|(id, _)| *id == user)
                }
        }),
    }
}

pub fn inspects(
    st: &MockState,
    area: &AreaRecord,
    source: Option<&SecretRecord>,
    user: Uuid,
    atlas: Option<Uuid>,
) -> bool {
    let p = policy(st, area, source, area.atlas_id);
    if !holds(st, area, user, &p, "read") {
        return false;
    }
    let map = if let Some(clan) = area.clan_id.and_then(|id| st.clans.clans.get(&id)) {
        let mut placed = area.clone();
        placed.atlas_id = atlas;
        super::clan_maps::active_owner(st, user, area)
            || clan
                .actions_on(user, super::clan_maps::resource(&placed))
                .contains("grant.inspect")
    } else {
        st.caps(user, area.id).is_some_and(|c| c.can_admin)
    };
    map && (source.is_none() || holds(st, area, user, &p, "manage_access"))
}

pub fn may_disclose(
    st: &MockState,
    area: &AreaRecord,
    source: Option<&SecretRecord>,
    user: Uuid,
    before: &Policy,
    after: &Policy,
    selected: bool,
) -> bool {
    let gains: Vec<_> = ACTIONS
        .into_iter()
        .map(|action| {
            (
                action,
                increases(&audience(before, action), &audience(after, action)),
            )
        })
        .filter(|(_, who)| !who.is_empty())
        .collect();
    if gains.is_empty() || (selected && holds(st, area, user, before, "copy")) {
        return true;
    }
    if !holds(st, area, user, before, "read") {
        return false;
    }
    let owner = owns(st, area, source, user);
    let rights: BTreeSet<_> = gains.iter().map(|(a, _)| *a).chain(["read"]).collect();
    if let Some(clan) = area.clan_id.and_then(|id| st.clans.clans.get(&id)) {
        if owner {
            return true;
        }
        if source.is_none() {
            if area.member_owned.is_some() {
                return false;
            }
            return rights.iter().all(|right| {
                let Some((_, action)) = MAP.iter().find(|(candidate, _)| candidate == right) else {
                    return false;
                };
                SHARED.contains(right)
                    && clan
                        .delegation_through(
                            user,
                            &ClanGrantScope::Areas(BTreeSet::from([area.id])),
                            action,
                            &|id| super::clan_maps::placement(st, clan.id, id).and_then(|p| p.0),
                        )
                        .is_some()
            });
        }
        if !holds(st, area, user, before, "manage_access")
            || rights
                .iter()
                .any(|a| !SHARED.contains(a) || !holds(st, area, user, before, a))
        {
            return false;
        }
        let Some(record) = source.and_then(|s| s.clan.as_ref()) else {
            return false;
        };
        let protected: Vec<_> = record
            .grants
            .iter()
            .filter(|g| g.actions.contains("manage_access"))
            .map(|g| g.recipient)
            .collect();
        let reachable = canonical(
            clan.groups
                .iter()
                .filter(|g| !protected.contains(&ClanRecipient::Group(g.id)))
                .flat_map(|g| group(clan, g.id))
                .chain(
                    clan.members
                        .keys()
                        .filter(|id| {
                            clan.has_member(**id) && !protected.contains(&ClanRecipient::User(**id))
                        })
                        .flat_map(|id| person(*id, true)),
                )
                .collect(),
        );
        return gains
            .iter()
            .all(|(_, who)| increases(&reachable, who).is_empty());
    }
    if !owner {
        if !holds(st, area, user, before, "manage_access") {
            return false;
        }
        if source.is_some() {
            if rights
                .iter()
                .any(|a| !SHARED.contains(a) || !holds(st, area, user, before, a))
            {
                return false;
            }
        } else {
            let caps = st.caps(user, area.id).expect("existing map");
            if rights
                .iter()
                .any(|a| ["rename", "delete", "manage_ownership"].contains(a))
                || (!caps.can_admin
                    && rights
                        .iter()
                        .any(|a| *a == "manage_access" || a.starts_with("share_")))
                || (!caps.can_edit && rights.iter().any(|a| ["add", "edit", "remove"].contains(a)))
                || (!caps.can_copy && rights.contains("copy"))
            {
                return false;
            }
        }
    }
    let people: BTreeSet<_> = gains
        .iter()
        .flat_map(|(_, who)| who.iter().flatten())
        .filter_map(|a| {
            if let Atom::User(id) = a {
                Some(*id)
            } else {
                None
            }
        })
        .collect();
    for person in people {
        if person == user || person == area.user_id || !unfenced(st, area.user_id, user, person) {
            return false;
        }
        if !owner {
            if let Some(secret) = source {
                if secret.grants.iter().any(|g| {
                    g.grantor_id == user
                        && g.grantee_id == person
                        && g.actions.contains("manage_access")
                }) {
                    return false;
                }
            } else if !st.caps(user, area.id).is_some_and(|c| c.can_admin) {
                let needed: Vec<_> = gains
                    .iter()
                    .filter(|(_, who)| who.iter().any(|c| c.contains(&Atom::User(person))))
                    .map(|(a, _)| *a)
                    .collect();
                if !st.grants.iter().any(|g| {
                    g.grantee_id == user
                        && g.covers_area(area)
                        && g.can_reshare
                        && live(st, g)
                        && (!needed.iter().any(|a| ["add", "edit", "remove"].contains(a))
                            || g.can_edit)
                        && (!needed.contains(&"copy") || g.can_copy)
                }) {
                    return false;
                }
            }
        }
    }
    true
}
