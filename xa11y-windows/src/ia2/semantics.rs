//! IA2 object attributes and relations, using the shared document contracts.
use super::*;
use xa11y_core::table::index_from_one_based;
use xa11y_core::{CollectionParts, Landmark, LiveRegionParts, Politeness, RelationKind};

pub(super) fn object_attributes(node: &Node) -> Result<HashMap<String, String>> {
    let Some(a) = &node.ia2 else {
        return Ok(HashMap::new());
    };
    let mut value = BSTR::default();
    unsafe {
        (a.vtable().attributes)(a.as_raw(), &mut value)
            .ok()
            .map_err(native)?;
    }
    if value.len() > 65536 {
        return Err(platform("IA2 object attributes exceed 65536 UTF-16 units"));
    }
    rich::parse_attributes(&value.to_string())
}

pub(super) fn apply(
    data: &mut ElementData,
    attrs: &HashMap<String, String>,
    ia2_role: i32,
) -> Result<()> {
    let get = |key: &str| attrs.get(key).map(String::as_str);
    data.web_role = get("xml-roles")
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    data.landmark_type = data
        .web_role
        .as_deref()
        .and_then(|v| v.split_whitespace().find_map(Landmark::from_aria_role));
    if data.landmark_type.is_some() {
        data.role = Role::Landmark;
    }
    if ia2_role == 0x414 {
        data.role = Role::Heading;
    }
    if data.role == Role::Heading {
        data.heading_level = number(get("level"), "heading level")?;
    }
    let live = LiveRegionParts {
        politeness: get("live")
            .or_else(|| get("container-live"))
            .and_then(Politeness::parse),
        atomic: match get("atomic").or_else(|| get("container-atomic")) {
            Some("true") => Some(true),
            Some("false") => Some(false),
            None => None,
            Some(value) => return Err(platform(format!("invalid IA2 atomic attribute {value:?}"))),
        },
        relevant: get("relevant")
            .or_else(|| get("container-relevant"))
            .map(str::to_string),
    }
    .into();
    data.live_region = if xa11y_core::LiveRegionInfo::is_empty(&live) {
        None
    } else {
        Some(live)
    };
    // Read collection position for the roles every backend reports it on
    // (`is_collection_role`); the attributes are already in hand, so this
    // costs no calls.
    if crate::uia::is_collection_role(data.role) {
        let position = one_based(get("posinset"), "position in set")?;
        let size = match get("setsize") {
            Some(v) => {
                Some(Count::from_signed(v.parse().map_err(|_| {
                    platform(format!("invalid IA2 set size: {v:?}"))
                })?))
            }
            None => None,
        };
        let level = one_based(get("level"), "collection level")?;
        if position.is_some() || size.is_some() || level.is_some() {
            data.collection = Some(
                CollectionParts {
                    index: position,
                    size,
                    depth: level,
                }
                .into(),
            );
        }
    }
    data.raw.insert(
        "ia2_attributes".into(),
        serde_json::to_value(attrs).map_err(|e| platform(e.to_string()))?,
    );
    Ok(())
}
/// A 1-based IA2 attribute as a 0-based index. `0` is "not reported"
/// (`index_from_one_based`); only a value that is not a number is an error.
fn one_based(value: Option<&str>, field: &str) -> Result<Option<u32>> {
    match value {
        None => Ok(None),
        Some(v) => v
            .trim()
            .parse::<i64>()
            .map(index_from_one_based)
            .map_err(|_| platform(format!("invalid IA2 {field}: {v:?}"))),
    }
}
fn number(value: Option<&str>, field: &str) -> Result<Option<u32>> {
    value
        .map(|v| {
            v.parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| platform(format!("invalid IA2 {field}: {v:?}")))
        })
        .transpose()
}

pub(super) fn relation_name(kind: RelationKind) -> Result<&'static str> {
    Ok(match kind {
        RelationKind::LabelledBy => "labelledBy",
        RelationKind::LabelFor => "labelFor",
        RelationKind::DescribedBy => "describedBy",
        RelationKind::DescriptionFor => "descriptionFor",
        RelationKind::Details => "details",
        RelationKind::DetailsFor => "detailsFor",
        RelationKind::Controls => "controllerFor",
        RelationKind::ControlledBy => "controlledBy",
        RelationKind::FlowsTo => "flowsTo",
        RelationKind::FlowsFrom => "flowsFrom",
        RelationKind::ErrorMessage => "error",
        RelationKind::ErrorFor => "errorFor",
        RelationKind::MemberOf => "memberOf",
        RelationKind::Owns | RelationKind::Linked => {
            return Err(Error::Unsupported {
                feature: format!(
                "IA2 relation {kind}: ownership is in the tree; controls and flows are separate"
            ),
            })
        }
    })
}
impl State {
    pub(super) fn relations(
        &mut self,
        e: &ElementData,
        kinds: &[RelationKind],
    ) -> Result<Vec<(RelationKind, Vec<ElementData>)>> {
        let wanted = kinds
            .iter()
            .map(|kind| relation_name(*kind))
            .collect::<Result<Vec<_>>>()?;
        let node = self.node(e)?;
        let a = node
            .ia2
            .as_ref()
            .ok_or_else(|| unsupported(e, "IA2 relations"))?;
        let mut count = 0;
        unsafe {
            (a.vtable().n_relations)(a.as_raw(), &mut count)
                .ok()
                .map_err(native)?;
        }
        let count = bounded(count, 256, "relation count")?;
        let mut out: Vec<_> = kinds.iter().map(|k| (*k, Vec::new())).collect();
        let mut total = 0;
        for i in 0..count {
            self.check_deadline()?;
            let mut raw = std::ptr::null_mut();
            let hr = unsafe { (a.vtable().relation)(a.as_raw(), i as i32, &mut raw) };
            let relation = if raw.is_null() {
                None
            } else {
                Some(unsafe { AccessibleRelation::from_raw(raw) })
            };
            hr.ok().map_err(native)?;
            let relation = relation.ok_or_else(|| platform("IA2 returned a null relation"))?;
            let mut name = BSTR::default();
            unsafe {
                (relation.vtable().relationType)(relation.as_raw(), &mut name)
                    .ok()
                    .map_err(native)?;
            }
            if name.len() > 256 {
                return Err(platform("IA2 relation name exceeds 256 units"));
            }
            let name = name.to_string();
            let Some(index) = wanted.iter().position(|n| *n == name) else {
                continue;
            };
            let mut targets = 0;
            unsafe {
                (relation.vtable().nTargets)(relation.as_raw(), &mut targets)
                    .ok()
                    .map_err(native)?;
            }
            let targets = bounded(targets, 1024usize.saturating_sub(total), "relation targets")?;
            total += targets;
            for j in 0..targets {
                self.check_deadline()?;
                let mut raw = std::ptr::null_mut();
                let hr =
                    unsafe { (relation.vtable().target)(relation.as_raw(), j as i32, &mut raw) };
                let object = if raw.is_null() {
                    None
                } else {
                    Some(unsafe { IUnknown::from_raw(raw) })
                };
                hr.ok().map_err(native)?;
                let object = object
                    .ok_or_else(|| platform("IA2 returned a null relation target"))?
                    .cast()
                    .map_err(native)?;
                let target = Node::new(object, 0, node.owner.clone(), 0)?;
                out[index].1.push(self.snapshot(target)?);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn attributes_map_to_shared_fields_and_serialize() {
        let mut data = ElementData::for_role(Role::Group);
        let attrs = rich::parse_attributes(
            "xml-roles:main;live:polite;atomic:true;relevant:additions text;",
        )
        .unwrap();
        apply(&mut data, &attrs, 0).unwrap();
        assert_eq!(data.role, Role::Landmark);
        assert_eq!(data.landmark_type, Some(Landmark::Main));
        assert_eq!(
            data.live_region.as_ref().unwrap().politeness,
            Some(Politeness::Polite)
        );
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["web_role"], "main");
        apply(
            &mut data,
            &rich::parse_attributes("level:3;").unwrap(),
            0x414,
        )
        .unwrap();
        assert_eq!(data.heading_level, Some(3));
        assert!(apply(
            &mut data,
            &rich::parse_attributes("level:0;").unwrap(),
            0x414
        )
        .is_err());
    }
    #[test]
    fn collection_position_is_read_only_for_collection_roles() {
        let mut data = ElementData::for_role(Role::Button);
        apply(
            &mut data,
            &rich::parse_attributes("posinset:3;setsize:5;").unwrap(),
            0,
        )
        .unwrap();
        assert!(data.collection.is_none());
    }
    #[test]
    fn collection_position_is_zero_based_and_zero_means_unreported() {
        let mut data = ElementData::for_role(Role::Group);
        apply(
            &mut data,
            &rich::parse_attributes("posinset:3;setsize:5;level:2;").unwrap(),
            0,
        )
        .unwrap();
        let c = data.collection.as_ref().unwrap();
        assert_eq!(c.index, Some(2));
        assert_eq!(c.size, Some(Count::Known(5)));
        assert_eq!(c.depth, Some(1));

        let mut data = ElementData::for_role(Role::ListItem);
        apply(
            &mut data,
            &rich::parse_attributes("posinset:0;level:0;").unwrap(),
            0,
        )
        .unwrap();
        assert!(data.collection.is_none());

        let mut data = ElementData::for_role(Role::ListItem);
        assert!(apply(
            &mut data,
            &rich::parse_attributes("posinset:abc;").unwrap(),
            0
        )
        .is_err());
    }
    #[test]
    fn relations_never_alias_ownership_or_linked() {
        assert_eq!(
            relation_name(RelationKind::Controls).unwrap(),
            "controllerFor"
        );
        assert_eq!(relation_name(RelationKind::ErrorMessage).unwrap(), "error");
        assert!(relation_name(RelationKind::Owns).is_err());
        assert!(relation_name(RelationKind::Linked).is_err());
    }
}
