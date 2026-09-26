//! The folder tree: turning a flat list of groups into something safe to draw, whatever
//! two devices re-parenting at once left behind.

use std::collections::{HashMap, HashSet};

use crate::Group;

/// Safe parent -> children map, keyed by parent id (`""` = top level) with children sorted
/// by name.
///
/// Concurrent re-parenting on two devices can produce a cycle (A -> B, B -> A); the CRDT
/// keeps both. Rendering that as-is would recurse forever, so any group whose ancestor
/// chain never reaches the root is lifted to the top level.
pub fn group_children(groups: &[Group]) -> HashMap<String, Vec<Group>> {
    let by_id: HashMap<&str, &Group> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut out: HashMap<String, Vec<Group>> = HashMap::new();
    for g in groups {
        let parent = if reaches_root(&by_id, g) {
            g.parent_id.clone()
        } else {
            String::new()
        };
        out.entry(parent).or_default().push(g.clone());
    }
    for children in out.values_mut() {
        children.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    }
    out
}

/// True when the ancestor chain reaches the top level; false on a cycle or missing parent.
fn reaches_root(by_id: &HashMap<&str, &Group>, g: &Group) -> bool {
    let mut seen: HashSet<&str> = HashSet::from([g.id.as_str()]);
    let mut cur = g.parent_id.as_str();
    while !cur.is_empty() {
        if !seen.insert(cur) {
            return false; // cycle
        }
        match by_id.get(cur) {
            Some(parent) => cur = parent.parent_id.as_str(),
            None => return false, // parent is gone
        }
    }
    true
}

/// Whether `id` is `ancestor` itself or below it — used to reject a drop that would put a
/// group inside its own subtree.
pub fn is_descendant(groups: &[Group], id: &str, ancestor: &str) -> bool {
    if id == ancestor {
        return true;
    }
    let by_id: HashMap<&str, &Group> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cur = id;
    while let Some(g) = by_id.get(cur) {
        if !seen.insert(cur) {
            return false; // cycle
        }
        if g.parent_id == ancestor {
            return true;
        }
        if g.parent_id.is_empty() {
            return false;
        }
        cur = g.parent_id.as_str();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DEFAULT_COLOR;

    fn group_at(id: &str, name: &str, parent: &str) -> Group {
        Group {
            id: id.into(),
            name: name.into(),
            parent_id: parent.into(),
            color: DEFAULT_COLOR.into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn group_children_nests_and_sorts() {
        let groups = vec![
            group_at("b", "Work", ""),
            group_at("a", "Home", ""),
            group_at("c", "Inner", "a"),
        ];
        let tree = group_children(&groups);
        let roots: Vec<&str> = tree[""].iter().map(|g| g.id.as_str()).collect();
        assert_eq!(roots, vec!["a", "b"]); // by name: Home < Work
        assert_eq!(tree["a"].len(), 1);
        assert_eq!(tree["a"][0].id, "c");
    }

    /// Two devices making each other the parent forms a cycle; both must be rescued to the
    /// top level so the tree stays renderable.
    #[test]
    fn group_children_breaks_parent_cycles() {
        let groups = vec![group_at("a", "A", "b"), group_at("b", "B", "a")];
        let tree = group_children(&groups);
        let mut roots: Vec<&str> = tree[""].iter().map(|g| g.id.as_str()).collect();
        roots.sort();
        assert_eq!(roots, vec!["a", "b"]); // both rescued
    }

    /// A group whose parent was deleted elsewhere surfaces at the top level, not nowhere.
    #[test]
    fn group_children_rescues_orphans() {
        let groups = vec![group_at("a", "A", "missing-parent")];
        let tree = group_children(&groups);
        assert_eq!(tree[""].len(), 1);
        assert_eq!(tree[""][0].id, "a");
    }

    #[test]
    fn is_descendant_detects_self_and_nested() {
        let groups = vec![group_at("a", "A", ""), group_at("b", "B", "a"), group_at("c", "C", "b")];
        assert!(is_descendant(&groups, "a", "a")); // itself
        assert!(is_descendant(&groups, "c", "a")); // grandchild
        assert!(!is_descendant(&groups, "a", "c")); // not the other way
    }
}
