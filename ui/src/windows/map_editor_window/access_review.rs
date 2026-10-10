//! Permission changes supplied by the service, without expanding group rosters.
use std::collections::{BTreeMap, BTreeSet};

use iced::widget::{column, text};
use smudgy_cloud::access_review::{AccessReview, ReviewRecipient};

use super::Message;
use crate::theme::Element as ThemedElement;

fn recipient(recipient: &ReviewRecipient) -> String {
    match recipient {
        ReviewRecipient::Group { name, .. } => crate::i18n::t!("move-review-group", "name" => name),
        ReviewRecipient::User { id, nickname } => {
            nickname.clone().unwrap_or_else(|| id.to_string())
        }
        ReviewRecipient::Members => crate::i18n::t!("move-review-members"),
    }
}

fn action(action: &str) -> String {
    if let Some(held) = action.strip_prefix("share_") {
        return crate::i18n::t!("move-review-share-right", "action" => self::action(held));
    }
    crate::i18n::translate(&format!("move-review-right-{}", action.replace('_', "-")))
}

pub(super) fn content(review: &AccessReview) -> ThemedElement<'_, Message> {
    let mut rows = BTreeMap::<(String, String), (BTreeSet<String>, BTreeSet<String>)>::new();
    for change in &review.changes {
        for (gaining, audiences) in [(true, &change.may_gain), (false, &change.may_lose)] {
            for audience in audiences {
                let audience = audience
                    .iter()
                    .map(recipient)
                    .collect::<Vec<_>>()
                    .join(&crate::i18n::t!("move-review-and"));
                let entry = rows.entry((change.name.clone(), audience)).or_default();
                if gaining {
                    entry.0.insert(action(&change.action));
                } else {
                    entry.1.insert(action(&change.action));
                }
            }
        }
    }
    let mut body = column![].spacing(12);
    if !rows.is_empty() {
        body = body.push(text(crate::i18n::t!("move-review-overlap")).size(12));
    }
    for ((source, audience), (gained, lost)) in rows {
        let mut row = column![text(format!("{source} · {audience}")).size(14)].spacing(4);
        if !gained.is_empty() {
            row = row.push(text(crate::i18n::t!("move-review-gain", "actions" => gained.into_iter().collect::<Vec<_>>().join(", "))).size(13));
        }
        if !lost.is_empty() {
            row = row.push(text(crate::i18n::t!("move-review-loss", "actions" => lost.into_iter().collect::<Vec<_>>().join(", "))).size(13));
        }
        body = body.push(row);
    }
    body.into()
}
