use std::collections::{BTreeSet, HashMap};

pub(crate) fn parse_list(
    list_first_by_node: &HashMap<u32, BTreeSet<u32>>,
    list_rest_by_node: &HashMap<u32, BTreeSet<u32>>,
    rdf_nil_id: u32,
    head_id: u32,
) -> Result<Vec<u32>, &'static str> {
    let mut members = Vec::new();
    let mut current_id = head_id;
    let mut visited = BTreeSet::new();

    loop {
        if !visited.insert(current_id) {
            return Err("list contains a cycle");
        }

        let member_id = single_list_value(list_first_by_node, current_id, "rdf:first")?;
        members.push(member_id);
        let next_id = single_list_value(list_rest_by_node, current_id, "rdf:rest")?;
        if next_id == rdf_nil_id {
            return Ok(members);
        }

        current_id = next_id;
    }
}

fn single_list_value(
    map: &HashMap<u32, BTreeSet<u32>>,
    node_id: u32,
    field: &'static str,
) -> Result<u32, &'static str> {
    let Some(values) = map.get(&node_id) else {
        return Err(match field {
            "rdf:first" => "missing rdf:first entry",
            "rdf:rest" => "missing rdf:rest entry",
            _ => "missing list entry",
        });
    };

    if values.len() != 1 {
        return Err(match field {
            "rdf:first" => "rdf:first entry must be singular",
            "rdf:rest" => "rdf:rest entry must be singular",
            _ => "list entry must be singular",
        });
    }

    values.iter().next().copied().ok_or("empty list entry")
}
