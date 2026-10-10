//! Server-reviewed changes to content audiences. A review acknowledges a
//! particular operation; it never grants permission to perform that operation.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{AreaId, AtlasId, MovedContent, SourceId, mutation::MoveRequest};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewRecipient {
    Group { id: Uuid, name: String },
    User { id: Uuid, nickname: Option<String> },
    Members,
}

/// Each inner list is a conjunction; different lists are alternatives. A
/// group includes future members and does not disclose its current roster.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessChange {
    pub source: SourceId,
    pub name: String,
    pub action: String,
    pub may_gain: Vec<Vec<ReviewRecipient>>,
    pub may_lose: Vec<Vec<ReviewRecipient>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessReview {
    pub token: String,
    pub requires_confirmation: bool,
    pub destination_notice: bool,
    pub changes: Vec<AccessChange>,
    #[serde(default)]
    pub property_conflicts: Vec<crate::mutation::PropertyConflict>,
    #[serde(default = "default_preserves_undo")]
    pub preserves_undo: bool,
}

fn default_preserves_undo() -> bool {
    true
}

/// A request and its review, bound to the account and exact source versions.
/// Preparing it releases the edit fence; committing rechecks those versions.
/// Cancellation simply drops this value and leaves the original untouched.
#[derive(Debug, Clone)]
pub struct ReviewedMove {
    pub review: AccessReview,
    pub(crate) area_id: AreaId,
    pub(crate) content: MovedContent,
    pub(crate) request: MoveRequest,
    pub(crate) generation: u64,
}

#[derive(Debug, Clone)]
pub struct ReviewedFiling {
    pub review: AccessReview,
    pub(crate) area_id: AreaId,
    pub(crate) atlas_id: Option<AtlasId>,
    pub(crate) generation: Option<u64>,
}

impl ReviewedFiling {
    /// The map whose filing was reviewed.
    #[must_use]
    pub fn area_id(&self) -> AreaId {
        self.area_id
    }

    /// The destination covered by this review.
    #[must_use]
    pub fn atlas_id(&self) -> Option<AtlasId> {
        self.atlas_id
    }
}

impl ReviewedMove {
    /// Whether all conflicts have a permitted explicit choice in this request.
    #[must_use]
    pub fn properties_resolved(&self) -> bool {
        self.review.property_conflicts.iter().all(|conflict| {
            self.request.property_resolutions.iter().any(|choice| {
                choice.property == conflict.property
                    && (choice.keep == crate::mutation::PropertyChoice::Destination
                        || conflict.can_replace)
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::{PropertyAddress, PropertyChoice, PropertyConflict, PropertyResolution};

    #[test]
    fn property_review_requires_an_explicit_permitted_choice() {
        let property = PropertyAddress {
            name: "note".into(),
            room_number: Some(crate::RoomNumber(2)),
            room_source: SourceId::Map,
        };
        let mut reviewed = ReviewedMove {
            area_id: AreaId(Uuid::new_v4()),
            content: MovedContent::default(),
            generation: 0,
            review: AccessReview {
                token: "review".into(),
                requires_confirmation: true,
                destination_notice: false,
                changes: vec![],
                preserves_undo: false,
                property_conflicts: vec![PropertyConflict {
                    property: property.clone(),
                    source_value: "new".into(),
                    destination_value: "existing".into(),
                    can_replace: false,
                }],
            },
            request: MoveRequest {
                operation_id: Uuid::new_v4(),
                from: SourceId::Map,
                to: SourceId::Private,
                preconditions: vec![],
                rooms: vec![],
                connections: vec![],
                labels: vec![],
                shapes: vec![],
                access_review: None,
                properties: vec![property.clone()],
                property_resolutions: vec![],
            },
        };
        assert!(!reviewed.properties_resolved());
        reviewed
            .request
            .property_resolutions
            .push(PropertyResolution {
                property,
                keep: PropertyChoice::Source,
            });
        assert!(
            !reviewed.properties_resolved(),
            "Add does not permit replacing a value"
        );
        reviewed.request.property_resolutions[0].keep = PropertyChoice::Destination;
        assert!(reviewed.properties_resolved());
        reviewed.request.property_resolutions[0].keep = PropertyChoice::Source;
        reviewed.review.property_conflicts[0].can_replace = true;
        assert!(reviewed.properties_resolved());
        let wire = serde_json::to_value(&reviewed.request).unwrap();
        assert!(wire["properties"][0].get("room_source").is_none());
        let decoded: MoveRequest = serde_json::from_value(wire).unwrap();
        assert_eq!(
            decoded.property_resolutions,
            reviewed.request.property_resolutions
        );
    }
}
