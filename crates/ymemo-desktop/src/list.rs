//! Model for the memo list window: flattens the group tree into rows and applies dragged
//! rows back to the core.

use ymemo_core::diag;
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
            diag!("could not read the list: {e}");
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
                diag!("could not read the groups: {e}");
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
        diag!("move failed: {e}");
        return;
    }
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
}

/// Drops a memo into a gap between two rows, which is how a folder gets arranged by hand.
///
/// The gap says both things at once — which folder the memo lands in, and where in it:
///
/// - The folder is the one that owns the row **above** the gap: that memo's folder, or the
///   folder whose own row it is (dropping right under an open folder puts the memo inside it,
///   at the top). Above the first row means the top level.
/// - The neighbours are the memos of that folder either side of the gap, skipping the one
///   being dragged — it is still in the list while it is being moved.
///
/// Folders never get here; `move_row` still handles those. What is being arranged is the
/// memos inside a folder, and a folder's own place comes from the tree it is in.
pub(crate) fn reorder_row(ctx: &Ctx, src: i32, gap: i32) {
    use slint::Model;
    let rows = &ctx.model;
    let Some(source) = usize::try_from(src).ok().and_then(|i| rows.row_data(i)) else {
        return;
    };
    if source.is_group {
        return;
    }

    let mut guard = ctx.vault.borrow_mut();
    let Some(v) = guard.as_mut() else { return };
    let count = rows.row_count() as i32;
    let gap = gap.clamp(0, count);

    // The folder of a row, or None for a row that is not a memo.
    let memo_group = |v: &Vault, id: &str| -> Option<String> {
        v.store().get(id).ok().flatten().map(|m| m.group_id)
    };

    // Walk up from the gap for the first row that is not the one being dragged.
    let mut dest = String::new();
    for i in (0..gap).rev() {
        let Some(row) = rows.row_data(i as usize) else { continue };
        if row.id == source.id {
            continue;
        }
        dest = if row.is_group {
            if row.expanded {
                row.id.to_string() // just under an open folder: inside it
            } else {
                // A closed folder shows nothing of its contents, so a drop under it belongs
                // beside it rather than inside, where it would vanish.
                v.store().get_group(row.id.as_str()).ok().flatten().map(|g| g.parent_id).unwrap_or_default()
            }
        } else {
            memo_group(v, row.id.as_str()).unwrap_or_default()
        };
        break;
    }

    let neighbour = |v: &Vault, i: i32| -> Option<String> {
        let row = rows.row_data(i as usize)?;
        if row.is_group || row.id == source.id {
            return None;
        }
        (memo_group(v, row.id.as_str())? == dest).then(|| row.id.to_string())
    };
    let after = (0..gap).rev().find_map(|i| neighbour(v, i));
    let before = (gap..count).find_map(|i| neighbour(v, i));

    if let Err(e) = v.move_memo(source.id.as_str(), &dest, after.as_deref(), before.as_deref()) {
        diag!("could not rearrange the memo: {e}");
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
        diag!("could not change the colour: {e}");
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
            // A restored version is a different note; show it from its first line rather
            // than at whatever offset the previous one had been left at.
            entry.window.invoke_body_to_top();
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

    use std::cell::{Cell, RefCell};
    use std::collections::{HashMap, HashSet};
    use std::rc::Rc;
    use std::time::Instant;
    use ymemo_core::{vault::Vault, Memo, Store};

    /// A `Ctx` around a real vault, with the row model the list window would be showing.
    ///
    /// No window is involved: everything the drop logic reads is the row model and the vault,
    /// so the part that cannot be driven by hand — which folder a gap belongs to, and which
    /// memos are its neighbours — is exactly the part this can check.
    fn ctx_with(titles: &[&str]) -> (Ctx, Vec<String>) {
        // A directory nothing else is using; the process id and a counter is enough here.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ymemo-list-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut vault = Vault::create(&dir, b"pw", Store::open_in_memory().unwrap()).unwrap();
        let mut ids = Vec::new();
        for (i, title) in titles.iter().enumerate() {
            let mut m = Memo::new(*title, "");
            m.updated_at = 1_000 + i as i64;
            vault.upsert(&m).unwrap();
            ids.push(m.id);
        }
        let model = Rc::new(slint::VecModel::from(Vec::<ListRow>::new()));
        refresh_list(&vault, &model, &HashSet::new());
        let ctx = Ctx {
            vault: Rc::new(RefCell::new(Some(vault))),
            model,
            stickies: Rc::new(RefCell::new(HashMap::new())),
            collapsed: Rc::new(RefCell::new(HashSet::new())),
            dir: Rc::new(dir),
            settings: Rc::new(RefCell::new(crate::settings::Settings::default())),
            last_activity: Rc::new(Cell::new(Instant::now())),
            has_tray: Rc::new(Cell::new(false)),
        };
        (ctx, ids)
    }

    fn titles(ctx: &Ctx) -> Vec<String> {
        use slint::Model;
        ctx.model.iter().map(|r| r.title.to_string()).collect()
    }

    /// Dropping a memo in the gap above everything puts it first, and leaves the rest alone.
    #[test]
    fn a_memo_dropped_at_the_top_goes_first() {
        let (ctx, _) = ctx_with(&["a", "b", "c", "d"]);
        assert_eq!(titles(&ctx), ["d", "c", "b", "a"]); // newest first, unarranged
        reorder_row(&ctx, 3, 0); // drag "a" to the gap above "d"
        assert_eq!(titles(&ctx), ["a", "d", "c", "b"]);
    }

    /// And in the gap past the last row, last.
    #[test]
    fn a_memo_dropped_at_the_bottom_goes_last() {
        let (ctx, _) = ctx_with(&["a", "b", "c", "d"]);
        reorder_row(&ctx, 0, 4); // drag "d" past the end
        assert_eq!(titles(&ctx), ["c", "b", "a", "d"]);
    }

    /// The two gaps touching a memo are where it already is. The UI does not send those, but
    /// nothing may move if one arrives — a drop that changes nothing must write nothing.
    #[test]
    fn dropping_a_memo_back_where_it_was_changes_nothing() {
        let (ctx, _) = ctx_with(&["a", "b", "c"]);
        let before = titles(&ctx);
        reorder_row(&ctx, 1, 1);
        assert_eq!(titles(&ctx), before);
        reorder_row(&ctx, 1, 2);
        assert_eq!(titles(&ctx), before);
    }

    /// An arrangement is not an edit, so it must not restamp the memo and float it to the top
    /// of every "most recently changed" list in the app.
    #[test]
    fn arranging_from_the_list_leaves_the_timestamp_alone() {
        let (ctx, ids) = ctx_with(&["a", "b", "c"]);
        reorder_row(&ctx, 2, 0); // "a" to the top
        let guard = ctx.vault.borrow();
        let v = guard.as_ref().unwrap();
        assert_eq!(v.store().get(&ids[0]).unwrap().unwrap().updated_at, 1_000);
    }

    /// Folders are dropped *on* rows, never between them; `move_row` owns that.
    #[test]
    fn a_folder_is_not_reordered_by_a_gap() {
        let (ctx, _) = ctx_with(&["a", "b"]);
        {
            let mut guard = ctx.vault.borrow_mut();
            let v = guard.as_mut().unwrap();
            v.upsert_group(&group("g", "folder", "")).unwrap();
            refresh_list(v, &ctx.model, &HashSet::new());
        }
        let before = titles(&ctx);
        let folder_row = titles(&ctx).iter().position(|t| t == "folder").unwrap() as i32;
        reorder_row(&ctx, folder_row, 0);
        assert_eq!(titles(&ctx), before);
    }

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
