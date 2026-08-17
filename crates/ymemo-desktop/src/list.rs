//! 메모 목록 창의 모델 만들기: 그룹 트리를 평탄화해 행으로 내보내고,
//! 드래그로 옮긴 행을 코어에 반영한다.

use std::collections::{HashMap, HashSet};

use slint::{SharedString, VecModel};
use ymemo_core::{now_millis, vault::Vault, Memo};

use crate::state::Ctx;
use crate::ListRow;

/// vault 캐시의 그룹 트리 + 메모를 평탄화해 목록 모델에 반영한다.
///
/// Slint 에 트리 뷰가 없으므로 여기서 깊이 우선으로 펼쳐 `depth` 를 붙인 행 목록을
/// 만든다. 접힌 그룹의 내용은 아예 행으로 내보내지 않는다.
pub(crate) fn refresh_list(vault: &Vault, model: &VecModel<ListRow>, collapsed: &HashSet<String>) {
    let (groups, memos) = match (vault.store().list_groups(), vault.store().list()) {
        (Ok(g), Ok(m)) => (g, m),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("목록 조회 실패: {e}");
            return;
        }
    };
    // 순환/유실 부모는 코어가 최상위로 끌어올려 준다.
    let children = ymemo_core::group_children(&groups);
    let valid: HashSet<&str> = groups.iter().map(|g| g.id.as_str()).collect();

    let mut rows = Vec::new();
    push_group_rows("", 0, &children, &memos, collapsed, &mut rows);
    // 그룹에 속하지 않은(또는 그룹이 사라진) 메모는 최상위에 둔다.
    for m in memos.iter().filter(|m| !valid.contains(m.group_id.as_str())) {
        rows.push(memo_row(m, 0));
    }
    model.set_vec(rows);
}

/// 드래그로 행을 옮긴다: `src` 행을 `dst` 행이 가리키는 그룹 안으로 넣는다.
///
/// - 그룹 행에 놓으면 그 그룹 안으로.
/// - 메모 행에 놓으면 그 메모와 같은 그룹으로 (옆에 두는 느낌).
/// - 목록 위/아래로 벗어나게 놓으면 최상위로 뺀다.
pub(crate) fn move_row(ctx: &Ctx, src: i32, dst: i32) {
    use slint::Model;
    let rows = &ctx.model;
    let Some(source) = usize::try_from(src).ok().and_then(|i| rows.row_data(i)) else {
        return;
    };

    let mut guard = ctx.vault.borrow_mut();
    let Some(v) = guard.as_mut() else { return };

    // 놓은 자리에서 새 부모 그룹 id 를 정한다 (범위 밖 = 최상위).
    let target_parent = match usize::try_from(dst).ok().and_then(|i| rows.row_data(i)) {
        Some(t) if t.is_group => t.id.to_string(),
        Some(t) => match v.store().get(t.id.as_str()) {
            Ok(Some(m)) => m.group_id,
            _ => String::new(),
        },
        None => String::new(), // 위/아래로 벗어남 → 최상위
    };

    let res = if source.is_group {
        let id = source.id.to_string();
        // 자기 자신이나 자손 밑으로는 못 넣는다 (넣으면 트리가 순환한다).
        let groups = match v.store().list_groups() {
            Ok(g) => g,
            Err(e) => {
                eprintln!("그룹 조회 실패: {e}");
                return;
            }
        };
        if ymemo_core::is_descendant(&groups, &target_parent, &id) {
            return; // 조용히 무시 — 드롭이 안 먹은 것처럼 보인다
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
        eprintln!("이동 실패: {e}");
        return;
    }
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
}

/// `parent` 밑의 그룹들을 재귀적으로 행에 담는다 (그룹 먼저, 그 다음 그 그룹의 메모).
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
            color: SharedString::new(),
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

    /// 트리가 깊이 우선으로 평탄화되고, 중첩 그룹이 들여쓰기를 갖는지.
    #[test]
    fn flattens_nested_groups_depth_first() {
        let groups = vec![group("outer", "바깥", ""), group("inner", "안쪽", "outer")];
        let memos = vec![memo_in("m-in", "inner"), memo_in("m-out", "outer")];
        let children = ymemo_core::group_children(&groups);

        let mut rows = Vec::new();
        push_group_rows("", 0, &children, &memos, &HashSet::new(), &mut rows);

        let got: Vec<(&str, i32, bool)> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.depth, r.is_group))
            .collect();
        // 그룹 먼저, 자식 그룹 재귀, 그 다음 그 그룹의 메모.
        assert_eq!(
            got,
            vec![
                ("outer", 0, true),
                ("inner", 1, true),
                ("m-in", 2, false),
                ("m-out", 1, false),
            ]
        );
        // 바깥 그룹의 자식 수 = 하위 그룹 1 + 메모 1
        assert_eq!(rows[0].child_count, 2);
    }

    /// 접은 그룹은 내용이 아예 행으로 나오지 않아야 한다.
    #[test]
    fn collapsed_group_hides_its_contents() {
        let groups = vec![group("outer", "바깥", ""), group("inner", "안쪽", "outer")];
        let memos = vec![memo_in("m-out", "outer")];
        let children = ymemo_core::group_children(&groups);
        let collapsed = HashSet::from(["outer".to_string()]);

        let mut rows = Vec::new();
        push_group_rows("", 0, &children, &memos, &collapsed, &mut rows);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id.as_str(), "outer");
        assert!(!rows[0].expanded);
        assert_eq!(rows[0].child_count, 2); // 접혀 있어도 개수는 보여준다
    }
}
