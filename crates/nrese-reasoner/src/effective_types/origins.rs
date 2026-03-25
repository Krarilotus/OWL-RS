use crate::dataset_index::IndexedDataset;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectTypeOrigin {
    Asserted,
    Domain {
        via_property_id: u32,
    },
    Range {
        via_property_id: u32,
    },
    SameAs {
        source_instance_id: u32,
        source_origin: Box<DirectTypeOrigin>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveTypeOrigin {
    Direct(DirectTypeOrigin),
    Inherited {
        from_class_id: u32,
        from_origin: DirectTypeOrigin,
    },
}

pub(crate) fn direct_origin_rank(origin: &DirectTypeOrigin) -> u8 {
    match origin {
        DirectTypeOrigin::Asserted => 0,
        DirectTypeOrigin::SameAs { source_origin, .. } => 1 + direct_origin_rank(source_origin),
        DirectTypeOrigin::Domain { .. } => 10,
        DirectTypeOrigin::Range { .. } => 20,
    }
}

pub(crate) fn effective_origin_rank(origin: &EffectiveTypeOrigin) -> u8 {
    match origin {
        EffectiveTypeOrigin::Direct(direct) => direct_origin_rank(direct),
        EffectiveTypeOrigin::Inherited { from_origin, .. } => 10 + direct_origin_rank(from_origin),
    }
}

pub(crate) fn describe_origin(
    index: &IndexedDataset,
    origin: &EffectiveTypeOrigin,
) -> Option<String> {
    match origin {
        EffectiveTypeOrigin::Direct(DirectTypeOrigin::Asserted) => Some("asserted".to_owned()),
        EffectiveTypeOrigin::Direct(DirectTypeOrigin::Domain { via_property_id }) => Some(format!(
            "domain-derived via {}",
            index.symbols().resolve(*via_property_id)?
        )),
        EffectiveTypeOrigin::Direct(DirectTypeOrigin::Range { via_property_id }) => Some(format!(
            "range-derived via {}",
            index.symbols().resolve(*via_property_id)?
        )),
        EffectiveTypeOrigin::Direct(DirectTypeOrigin::SameAs {
            source_instance_id,
            source_origin,
        }) => Some(format!(
            "equality-derived via {} ({})",
            index.symbols().resolve(*source_instance_id)?,
            describe_origin(
                index,
                &EffectiveTypeOrigin::Direct((**source_origin).clone())
            )?
        )),
        EffectiveTypeOrigin::Inherited {
            from_class_id,
            from_origin,
        } => Some(format!(
            "subclass-derived from {} ({})",
            index.symbols().resolve(*from_class_id)?,
            describe_origin(index, &EffectiveTypeOrigin::Direct(from_origin.clone()))?
        )),
    }
}
