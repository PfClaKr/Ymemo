//! Model for the memo list window: flattens the group tree into rows and applies dragged
//! rows back to the core.

use std::collections::{HashMap, HashSet};

use slint::{ComponentHandle, SharedString, VecModel};
use ymemo_core::{now_millis, vault::Vault, Memo};

use crate::state::Ctx;
use crate::ListRow;

/// Flattens the cached groups and memos into the list model.
///
/// Slint has no tree view, so the tree is walked depth-first into rows carrying a `depth`.
/// A collapsed group's contents produce no rows at all.
pub(crate) fn refresh_list(vault: &Vault, model: &VecModel<ListRow>, collapsed: &HashSet<String>) {
    let (groups, memos) = match (vault.store().list_groups(), vault.store().list()) {
        (Ok(g), Ok(m)) => (g, m),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("could not read the list: {e}");
            return;
        }
    };
    // The core lifts cyclic and orphaned groups to the top level.
    let children = ymemo_core::group_children(&groups);
    let valid: HashSet<&str> = groups.iter().map(|g| g.id.as_str()).collect();

    let mut rows = Vec::new();
    push_group_rows("", 0, &children, &memos, collapsed, &mut rows);
    // Memos with no group, or whose group is gone, sit at the top level.
    for m in memos.iter().filter(|m| !valid.contains(m.group_id.as_str())) {
        rows.push(memo_row(m, 0));
    }
    model.set_vec(rows);
}

/// Moves row `src` into the group implied by row `dst`.
///
/// - Dropped on a group: into that group.
/// - Dropped on a memo: into that memo's group, i.e. beside it.
/// - Dropped past either end of the list: out to the top level.
pub(crate) fn move_row(ctx: &Ctx, src: i32, dst: i32) {
    use slint::Model;
    let rows = &ctx.model;
    let Some(source) = usize::try_from(src).ok().and_then(|i| rows.row_data(i)) else {
        return;
    };

    let mut guard = ctx.vault.borrow_mut();
    let Some(v) = guard.as_mut() else { return };

    // Derive the new parent from the drop target; out of range means top level.
    let target_parent = match usize::try_from(dst).ok().and_then(|i| rows.row_data(i)) {
        Some(t) if t.is_group => t.id.to_string(),
        Some(t) => match v.store().get(t.id.as_str()) {
            Ok(Some(m)) => m.group_id,
            _ => String::new(),
        },
        None => String::new(), // dropped past the list
    };

    let res = if source.is_group {
        let id = source.id.to_string();
        // Never into itself or its own subtree; that would make the tree cyclic.
        let groups = match v.store().list_groups() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("could not read the groups: {e}");
                return;
            }
        };
        if ymemo_core::is_descendant(&groups, &target_parent, &id) {
            return; // ignored, so the drop simply looks like it did not take
        }
        match v.store().get_group(&id) {
            Ok(Some(mut g)) if g.parent_id != target_parent => {
                g.parent_id = target_parent;
                g.updated_at = now_millis();
                v.upsert_group(&g)
            }
            _ => return,
        }
    } else {
        match v.store().get(source.id.as_str()) {
            Ok(Some(mut m)) if m.group_id != target_parent => {
                m.group_id = target_parent;
                m.updated_at = now_millis();
                v.upsert(&m)
            }
            _ => return,
        }
    };
    if let Err(e) = res {
        eprintln!("move failed: {e}");
        return;
    }
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
}

/// Recursively emits the groups under `parent`: subgroups first, then that group's memos.
pub(crate) fn push_group_rows(
    parent: &str,
    depth: i32,
    children: &HashMap<String, Vec<ymemo_core::Group>>,
    memos: &[Memo],
    collapsed: &HashSet<String>,
    out: &mut Vec<ListRow>,
) {
    let Some(groups) = children.get(parent) else { return };
    for g in groups {
        let child_groups = children.get(&g.id).map_or(0, |v| v.len());
        let child_memos = memos.iter().filter(|m| m.group_id == g.id).count();
        let is_collapsed = collapsed.contains(&g.id);
        out.push(ListRow {
            id: SharedString::from(g.id.clone()),
            title: SharedString::from(g.name.clone()),
            color: SharedString::from(g.color.clone()),
            depth,
            is_group: true,
            expanded: !is_collapsed,
            child_count: (child_groups + child_memos) as i32,
        });
        if is_collapsed {
            continue;
        }
        push_group_rows(&g.id, depth + 1, children, memos, collapsed, out);
        for m in memos.iter().filter(|m| m.group_id == g.id) {
            out.push(memo_row(m, depth + 1));
        }
    }
}

/// Recolours a row from the list window. Folders and memos both carry a palette key, and
/// both are synced, so one entry point covers them.
pub(crate) fn set_row_color(ctx: &Ctx, id: &str, is_group: bool, color: &str) {
    let mut guard = ctx.vault.borrow_mut();
    let Some(v) = guard.as_mut() else { return };

    let result = if is_group {
        match v.store().get_group(id) {
            Ok(Some(mut g)) => {
                g.color = color.to_string();
                g.updated_at = now_millis();
                v.upsert_group(&g)
            }
            Ok(None) => return,
            Err(e) => Err(e),
        }
    } else {
        match v.store().get(id) {
            Ok(Some(mut m)) => {
                m.color = color.to_string();
                m.updated_at = now_millis();
                v.upsert(&m)
            }
            Ok(None) => return,
            Err(e) => Err(e),
        }
    };
    if let Err(e) = result {
        eprintln!("could not change the colour: {e}");
        return;
    }
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
}

/// Redraws the list, and an open sticky, after a history restore put old values back.
///
/// The borrow is opened here rather than passed in: the caller has just finished writing
/// through its own, and reaching through `Ctx` while one is still live is what used to kill
/// the app (see `sync::start_merge_timer`).
pub(crate) fn refresh_after_restore(ctx: &Ctx, entity: ymemo_core::history::Entity, id: &str) {
    let guard = ctx.vault.borrow();
    let Some(v) = guard.as_ref() else { return };
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow());

    if entity == ymemo_core::history::Entity::Memo {
        if let (Ok(Some(memo)), Some(entry)) = (v.store().get(id), ctx.stickies.borrow().get(id)) {
            entry.window.set_memo_text(crate::sticky::sticky_text(&memo).into());
            entry.window.set_memo_title(memo.title.into());
            entry.window.set_sticky_color(memo.color.into());
            entry.window.set_sticky_opacity(memo.opacity as f32);
            // The restore is the current text now, so nothing is waiting to be saved.
            entry.dirty.set(false);
            entry.window.window().request_redraw();
        }
    }
}

pub(crate) fn memo_row(memo: &Memo, depth: i32) -> ListRow {
    ListRow {
        id: SharedString::from(memo.id.clone()),
        title: SharedString::from(memo.title.clone()),
        color: SharedString::from(memo.color.clone()),
        depth,
        is_group: false,
        expanded: false,
        child_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(id: &str, name: &str, parent: &str) -> ymemo_core::Group {
        ymemo_core::Group {
            id: id.into(),
            name: name.into(),
            parent_id: parent.into(),
            color: ymemo_core::DEFAULT_COLOR.into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn memo_in(id: &str, group_id: &str) -> Memo {
        let mut m = Memo::new(id, "");
        m.id = id.into();
        m.group_id = group_id.into();
        m
    }

    /// The tree flattens depth-first and nested groups are indented.
    #[test]
    fn flattens_nested_groups_depth_first() {
        let groups = vec![group("outer", "Outer", ""), group("inner", "Inner", "outer")];
        let memos = vec![memo_in("m-in", "inner"), memo_in("m-out", "outer")];
        let children = ymemo_core::group_children(&groups);

        let mut rows = Vec::new();
        push_group_rows("", 0, &children, &memos, &HashSet::new(), &mut rows);

        let got: Vec<(&str, i32, bool)> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.depth, r.is_group))
            .collect();
        // Group, then its subgroups recursively, then its own memos.
        assert_eq!(
            got,
            vec![
                ("outer", 0, true),
                ("inner", 1, true),
                ("m-in", 2, false),
                ("m-out", 1, false),
            ]
        );
        // The outer group counts one subgroup plus one memo.
        assert_eq!(rows[0].child_count, 2);
    }

    /// A collapsed group emits no rows for its contents.
    #[test]
    fn collapsed_group_hides_its_contents() {
        let groups = vec![group("outer", "Outer", ""), group("inner", "Inner", "outer")];
        let memos = vec![memo_in("m-out", "outer")];
        let children = ymemo_core::group_children(&groups);
        let collapsed = HashSet::from(["outer".to_string()]);

        let mut rows = Vec::new();
        push_group_rows("", 0, &children, &memos, &collapsed, &mut rows);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id.as_str(), "outer");
        assert!(!rows[0].expanded);
        assert_eq!(rows[0].child_count, 2); // the count still shows while collapsed
    }
}
