//! Document semantics carried on [`ElementData`] (#476): landmarks, live
//! regions and relations between elements.
//!
//! The scalar properties (`heading_level`, `landmark_type`, `web_role`,
//! `link_url`, `live_region`) are fields on [`ElementData`], read in the same
//! batch as the rest of an element where the platform allows. Relations point
//! at other elements, so they are a method
//! ([`Element::relations`](crate::Element::relations)) rather than a field:
//! reading them on every node would cost a platform call per node and put
//! cycles in a serialized tree.
//!
//! # Absent versus unsupported
//!
//! A field is `None` when the element has no such property, or its toolkit
//! did not publish it. Whether a platform can ever report one is a backend
//! fact, answered by [`capabilities()`](crate::Capabilities): a `None` heading
//! level on a heading in a toolkit the table marks `partial` means "not
//! published here", not "level 0".
//!
//! # Relations are reported as the platform reports them
//!
//! The platforms do not map ARIA relations one to one (Core-AAM 1.2 §3.5).
//! macOS `AXLinkedUIElements` carries both `aria-controls` and
//! `aria-flowto`, so it is reported as [`RelationKind::Linked`] rather than
//! guessed apart; UIA `DescribedBy` carries `aria-details`, so it is reported
//! as [`RelationKind::Details`]. A kind a platform cannot report is an
//! `Unsupported` error, and a supported kind with no targets is an empty
//! list, so the two are never confused.
//!
//! [`ElementData`]: crate::element::ElementData

use serde::{Deserialize, Serialize};

use crate::element::reader_writer_pair;
use crate::error::{Error, Result};

/// The kind of landmark a [`Role::Landmark`](crate::Role::Landmark) element
/// is. Spelled as the ARIA landmark roles are.
///
/// `#[non_exhaustive]`: backends map platform facts *to* a landmark, never the
/// reverse, so a new kind needs no work in a backend that cannot report it,
/// and [`Unknown`](Self::Unknown) is the fallback for one a platform names
/// but xa11y does not.
///
/// Serializes with the same spelling as [`Self::to_snake_case`]
/// (`"contentinfo"`, not `"ContentInfo"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Landmark {
    /// `banner`: the site or app header.
    Banner,
    /// `complementary`: supporting content.
    Complementary,
    /// `contentinfo`: the footer.
    ContentInfo,
    /// `form` (a named form region).
    Form,
    /// `main`: the main content.
    Main,
    /// `navigation`.
    Navigation,
    /// `region` (a named generic region).
    Region,
    /// `search`.
    Search,
    /// A landmark the platform reports without a kind xa11y recognises.
    Unknown,
}

impl Landmark {
    /// The name this landmark crosses the bindings, the CLI, MCP and
    /// selectors as: the ARIA role (`"main"`, `"contentinfo"`, ...).
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Banner => "banner",
            Self::Complementary => "complementary",
            Self::ContentInfo => "contentinfo",
            Self::Form => "form",
            Self::Main => "main",
            Self::Navigation => "navigation",
            Self::Region => "region",
            Self::Search => "search",
            Self::Unknown => "unknown",
        }
    }

    /// The landmark an ARIA role names (`xml-roles`, `AriaRole`), or `None`
    /// for a role that is not a landmark.
    pub fn from_aria_role(role: &str) -> Option<Self> {
        Some(match role.trim().to_ascii_lowercase().as_str() {
            "banner" => Self::Banner,
            "complementary" => Self::Complementary,
            "contentinfo" => Self::ContentInfo,
            "form" => Self::Form,
            "main" => Self::Main,
            "navigation" => Self::Navigation,
            "region" => Self::Region,
            "search" => Self::Search,
            _ => return None,
        })
    }
}

impl std::fmt::Display for Landmark {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

/// How urgently a live region's changes should be announced.
#[allow(
    clippy::exhaustive_enums,
    reason = "Closed domain: ARIA, UIA LiveSetting, AX AXARIALive and AT-SPI \
              `live` all define exactly off / polite / assertive."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Politeness {
    /// Changes are not announced.
    Off,
    /// Announced when the user is idle.
    Polite,
    /// Announced at once.
    Assertive,
}

impl Politeness {
    /// `"off"`, `"polite"` or `"assertive"`.
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Polite => "polite",
            Self::Assertive => "assertive",
        }
    }

    /// Parse the ARIA / AT-SPI / AX spelling. Unknown spellings are `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "polite" => Some(Self::Polite),
            "assertive" => Some(Self::Assertive),
            _ => None,
        }
    }
}

reader_writer_pair! {
    /// Live-region settings of an element, as the platform reports them.
    ///
    /// Set on the region itself and, where the platform propagates it (AT-SPI
    /// `container-live`), on its descendants. Every field is `None` when not
    /// reported; the struct is `None` on an element with no live-region
    /// settings at all.
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct LiveRegionInfo;

    /// Every field a provider must decide on for a live region. Not public API.
    #[allow(
        clippy::exhaustive_structs,
        reason = "Completeness guard for LiveRegionInfo; see ElementParts."
    )]
    #[derive(Debug, Clone)]
    pub struct LiveRegionParts;

    fields {
        /// `off`, `polite` or `assertive`.
        pub politeness: Option<Politeness>,
        /// Whether the whole region is announced when part of it changes
        /// (`aria-atomic`).
        pub atomic: Option<bool>,
        /// Which changes are announced, as the platform spells it
        /// (`"additions text"`, the `aria-relevant` token list).
        pub relevant: Option<String>,
    }
}

impl LiveRegionInfo {
    /// Whether no field is set, so a backend can report `None` instead.
    pub fn is_empty(&self) -> bool {
        self.politeness.is_none() && self.atomic.is_none() && self.relevant.is_none()
    }
}

/// A relation from one element to others.
///
/// Exhaustive: `Provider::relations` takes the kinds to read, and each backend
/// turns every variant into a platform query (or an `Unsupported` answer), so
/// a new kind must break their build.
#[allow(
    clippy::exhaustive_enums,
    reason = "Capability enum: backends translate each variant into a \
              platform relation query, so growth must break the build."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    /// The elements that label this one (`aria-labelledby`).
    LabelledBy,
    /// The elements this one labels.
    LabelFor,
    /// The elements that describe this one (`aria-describedby`).
    DescribedBy,
    /// The elements this one describes.
    DescriptionFor,
    /// Extended details for this element (`aria-details`).
    Details,
    /// The elements this one provides details for.
    DetailsFor,
    /// The elements this one controls (`aria-controls`).
    Controls,
    /// The elements that control this one.
    ControlledBy,
    /// The next elements in reading order (`aria-flowto`).
    FlowsTo,
    /// The previous elements in reading order.
    FlowsFrom,
    /// The element holding this one's error message (`aria-errormessage`).
    ErrorMessage,
    /// The elements this one is the error message for.
    ErrorFor,
    /// Elements this one owns outside its own subtree (`aria-owns`).
    Owns,
    /// The group this element is a member of.
    MemberOf,
    /// macOS `AXLinkedUIElements`: `aria-controls` and `aria-flowto` targets,
    /// which AX does not tell apart.
    Linked,
}

impl RelationKind {
    /// Every kind, in declaration order.
    pub const ALL: [RelationKind; 15] = [
        Self::LabelledBy,
        Self::LabelFor,
        Self::DescribedBy,
        Self::DescriptionFor,
        Self::Details,
        Self::DetailsFor,
        Self::Controls,
        Self::ControlledBy,
        Self::FlowsTo,
        Self::FlowsFrom,
        Self::ErrorMessage,
        Self::ErrorFor,
        Self::Owns,
        Self::MemberOf,
        Self::Linked,
    ];

    /// The name this kind crosses the bindings, the CLI and MCP as
    /// (`"labelled_by"`, `"flows_to"`, ...).
    pub fn to_snake_case(self) -> &'static str {
        match self {
            Self::LabelledBy => "labelled_by",
            Self::LabelFor => "label_for",
            Self::DescribedBy => "described_by",
            Self::DescriptionFor => "description_for",
            Self::Details => "details",
            Self::DetailsFor => "details_for",
            Self::Controls => "controls",
            Self::ControlledBy => "controlled_by",
            Self::FlowsTo => "flows_to",
            Self::FlowsFrom => "flows_from",
            Self::ErrorMessage => "error_message",
            Self::ErrorFor => "error_for",
            Self::Owns => "owns",
            Self::MemberOf => "member_of",
            Self::Linked => "linked",
        }
    }

    /// Parse a name as [`to_snake_case`](Self::to_snake_case) spells it.
    /// Fails with [`Error::InvalidActionData`] naming the valid names.
    pub fn parse(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|k| k.to_snake_case() == name)
            .ok_or_else(|| Error::InvalidActionData {
                message: format!(
                    "unknown relation kind {name:?}; expected one of {}",
                    Self::ALL
                        .iter()
                        .map(|k| k.to_snake_case())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            })
    }

    /// The capability-table feature name for this kind (`relation:labelled_by`).
    pub fn feature_name(self) -> String {
        format!("relation:{}", self.to_snake_case())
    }
}

impl std::fmt::Display for RelationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_snake_case())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for kind in RelationKind::ALL {
            assert_eq!(RelationKind::parse(kind.to_snake_case()).ok(), Some(kind));
        }
        assert!(RelationKind::parse("labeled_by").is_err());
        for name in ["main", "banner", "contentinfo", "region", "search"] {
            let landmark = Landmark::from_aria_role(name).expect("landmark");
            assert_eq!(landmark.to_snake_case(), name);
        }
        assert_eq!(Landmark::from_aria_role("Main"), Some(Landmark::Main));
        assert_eq!(Landmark::from_aria_role("button"), None);
        assert_eq!(Politeness::parse("Assertive"), Some(Politeness::Assertive));
        assert_eq!(Politeness::parse("rude"), None);
    }

    #[test]
    fn landmark_serde_spelling_is_the_snake_case_name() {
        let all = [
            Landmark::Banner,
            Landmark::Complementary,
            Landmark::ContentInfo,
            Landmark::Form,
            Landmark::Main,
            Landmark::Navigation,
            Landmark::Region,
            Landmark::Search,
            Landmark::Unknown,
        ];
        // Exhaustive (no `_` arm): a new variant fails to compile here, which
        // points whoever adds it at `all`.
        for kind in all {
            match kind {
                Landmark::Banner
                | Landmark::Complementary
                | Landmark::ContentInfo
                | Landmark::Form
                | Landmark::Main
                | Landmark::Navigation
                | Landmark::Region
                | Landmark::Search
                | Landmark::Unknown => {}
            }
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(json, format!("\"{}\"", kind.to_snake_case()));
            assert_eq!(
                serde_json::from_str::<Landmark>(&json).expect("deserialize"),
                kind
            );
        }
    }
}
