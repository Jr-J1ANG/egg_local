use crate::no_std_prelude::*;
use crate::util::{HashMap, HashSet};

use crate::{Analysis, ExtractorInfo, Id, Language};
use super::EGraph;

/// Rebuild a compact EGraph from the selected DAG in `ExtractorInfo`.
///
/// This version deliberately does NOT preserve the old eclass ID space.
/// The returned EGraph is completely rebuilt, so eclass IDs are compact and
/// may be completely different from the saturated EGraph.
///
/// The other purpose of this function is to maintain `unparticipated` across
/// the rebuild:
/// - `local_scope` is the set of eclasses that participated in this round;
/// - any old eclass in `unparticipated` that became equivalent to the local
///   scope is removed from `unparticipated`;
/// - surviving old `unparticipated` eclasses are remapped to their new IDs;
/// - eclasses created by this round's local saturation are never added to
///   `unparticipated`, because creating/selecting them is already part of the
///   current optimization.
///
/// Returns `(new_egraph, new_root, new_unparticipated)`.
pub fn prune<L, N>(
    saturated: &EGraph<L, N>,
    info: &ExtractorInfo<L>,
    local_scope: &[Id],
    unparticipated: &[Id],
) -> (EGraph<L, N>, Id, Vec<Id>)
where
    L: Language + Clone,
    N: Analysis<L> + Clone,
    N::Data: Clone,
{
    assert!(
        saturated.clean,
        "prune() requires a clean saturated EGraph"
    );

    // Canonical eclasses corresponding to the local scope after all unions
    // caused by this round's saturation have been processed.
    let local_canonical: HashSet<Id> = local_scope
        .iter()
        .map(|&id| saturated.find(id))
        .collect();

    // old canonical eclass ID -> new compact eclass ID.
    //
    // Only selected eclasses are inserted here, so this map is also the
    // provenance map needed to update `unparticipated`.
    let mut old_to_new: HashMap<Id, Id> = HashMap::default();

    let mut pruned = EGraph::new(saturated.analysis.clone());

    fn rebuild_selected<L, N>(
        old_id: Id,
        saturated: &EGraph<L, N>,
        info: &ExtractorInfo<L>,
        pruned: &mut EGraph<L, N>,
        old_to_new: &mut HashMap<Id, Id>,
    ) -> Id
    where
        L: Language + Clone,
        N: Analysis<L> + Clone,
        N::Data: Clone,
    {
        let canonical = saturated.find(old_id);

        if let Some(&new_id) = old_to_new.get(&canonical) {
            return new_id;
        }

        let selected = info
            .selected_nodes
            .get(&canonical)
            .unwrap_or_else(|| {
                panic!("selected DAG is missing eclass {}", canonical)
            });

        // Rebuild children first so the resulting EGraph remains a DAG and
        // preserves sharing between selected eclasses.
        let mut node = selected.node.clone();
        node.update_children(|child| {
            rebuild_selected(
                child,
                saturated,
                info,
                pruned,
                old_to_new,
            )
        });

        let new_id = pruned.add(node);

        let previous = old_to_new.insert(canonical, new_id);
        debug_assert!(
            previous.is_none(),
            "old eclass {} was inserted into the old_to_new map twice",
            canonical
        );

        new_id
    }

    let old_root = saturated.find(info.root);
    let new_root = rebuild_selected(
        old_root,
        saturated,
        info,
        &mut pruned,
        &mut old_to_new,
    );

    // `add()` may have populated the normal egg work queues, so rebuild once
    // after the whole selected DAG has been inserted.
    pruned.rebuild();

    // Normally this is an identity mapping because the selected DAG should not
    // contain duplicate enodes. Use find() anyway so the bookkeeping remains
    // correct even if a future language/analysis causes a union during rebuild.
    let rebuilt_root = pruned.find(new_root);

    // Maintain the history of eclasses that have NEVER participated in local
    // saturation.
    //
    // Importantly, we only iterate over the old `unparticipated` set. New
    // eclasses created by local saturation are therefore never introduced
    // into this history vector.
    let mut new_unparticipated = Vec::new();
    let mut seen = HashSet::default();

    for &old_id in unparticipated {
        let canonical = saturated.find(old_id);

        // This old eclass is now part of the current optimization region,
        // possibly because it was directly in local_scope or because it was
        // unioned with an eclass that was.
        if local_canonical.contains(&canonical) {
            continue;
        }

        // If the selected DAG did not keep this eclass, it no longer exists
        // in the new compact EGraph, so it must disappear from the history.
        let Some(&new_id) = old_to_new.get(&canonical) else {
            continue;
        };

        let new_id = pruned.find(new_id);

        // Two previously distinct unparticipated IDs may have become
        // equivalent during this round. Keep the new ID only once.
        if seen.insert(new_id) {
            new_unparticipated.push(new_id);
        }
    }

    // Deterministic order makes debugging and scope generation reproducible.
    new_unparticipated.sort_unstable();

    assert!(
        pruned.classes.contains_key(&rebuilt_root),
        "rebuilt root {} does not exist in the pruned EGraph",
        rebuilt_root
    );

    (pruned, rebuilt_root, new_unparticipated)
}
