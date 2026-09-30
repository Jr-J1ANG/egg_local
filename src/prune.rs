use crate::no_std_prelude::*;
use crate::util::HashSet;

use crate::{Analysis, ExtractorInfo, Id, Language};
use super::{EClass, EGraph};

/// Reconstruct a pruned EGraph from an extraction result.
///
/// The returned EGraph:
/// - keeps the original saturated EGraph's canonical eclass IDs;
/// - keeps only the enode selected by the extractor for each canonical eclass;
/// - discards all previous UnionFind equivalences and makes every surviving
///   ID canonical (`find(id) == id`);
/// - rebuilds `memo`, parent links, and `classes_by_op`;
/// - preserves the old ID space so that future eclass IDs can continue from
///   the old UnionFind size.
///
/// `info.selected_nodes` must contain at most one selected enode for each
/// canonical eclass.
pub fn prune<L, N>(egraph: &EGraph<L, N>, info: &ExtractorInfo<L>) -> EGraph<L, N>
where
    L: Language + Clone,
    N: Analysis<L> + Clone,
    N::Data: Clone,
{
    assert!(
        egraph.clean,
        "prune() requires a clean EGraph"
    );

    let id_space_size = egraph.unionfind.size();

    debug_assert_eq!(
        id_space_size,
        egraph.nodes.len(),
        "EGraph invariant violated: union-find size != nodes.len()"
    );

    // Canonicalize the selected nodes against the saturated EGraph first.
    // The new EGraph has no unions, so all child IDs must already be canonical.
    let mut selected: Vec<(Id, L)> = Vec::with_capacity(info.selected_nodes.len());

    for (&old_id, selected_node) in &info.selected_nodes {
        let canonical_id = egraph.find(old_id);

        let mut node = selected_node.node.clone();
        node.update_children(|child| egraph.find(child));

        selected.push((canonical_id, node));
    }

    // The current ExtractorInfo invariant is one selected enode per
    // canonical eclass.
    let mut seen = HashSet::with_capacity(selected.len());
    for &(id, _) in &selected {
        assert!(
            seen.insert(id),
            "multiple selected enodes for canonical eclass {}",
            id
        );
    }

    // Build a completely new EGraph. Its UnionFind will be initialized as
    // an identity relation over the original ID space.
    let mut pruned = EGraph::new(egraph.analysis.clone());

    for _ in 0..id_space_size {
        pruned.unionfind.make_set();
    }

    // Keep the old ID-indexed backing storage. Dead IDs are simply no longer
    // represented in `classes`, `memo`, `parents`, or `pending`.
    pruned.nodes = egraph.nodes.clone();

    // Recreate only the selected eclasses/enodes.
    for &(canonical_id, ref node) in &selected {
        let old_class = egraph
            .classes
            .get(&canonical_id)
            .unwrap_or_else(|| {
                panic!(
                    "selected canonical eclass {} does not exist in the saturated EGraph",
                    canonical_id
                )
            });

        let class = EClass {
            id: canonical_id,
            nodes: vec![node.clone()],
            data: old_class.data.clone(),
            parents: Vec::new(),
        };

        assert!(
            pruned.classes.insert(canonical_id, class).is_none(),
            "duplicate canonical eclass {} during pruning",
            canonical_id
        );

        // In the current egg implementation, the ID of an eclass is also
        // the index used by `nodes` for its current representative enode.
        pruned.nodes[usize::from(canonical_id)] = node.clone();

        assert!(
            pruned.memo.insert(node.clone(), canonical_id).is_none(),
            "duplicate selected enode {:?} in different pruned eclasses",
            node
        );
    }

    // Rebuild parent links from the selected DAG.
    // EClass::parents stores parent enode IDs. In the pruned graph each
    // surviving eclass has exactly one selected enode and its ID is the
    // canonical eclass ID.
    let mut parent_links: Vec<(Id, Id)> = Vec::new();

    for (&parent_id, class) in &pruned.classes {
        debug_assert_eq!(class.nodes.len(), 1);

        let node = &class.nodes[0];
        for &child_id in node.children() {
            parent_links.push((child_id, parent_id));
        }
    }

    for (child_id, parent_id) in parent_links {
        let child_class = pruned
            .classes
            .get_mut(&child_id)
            .unwrap_or_else(|| {
                panic!(
                    "selected enode in eclass {} refers to missing pruned child eclass {}",
                    parent_id, child_id
                )
            });

        child_class.parents.push(parent_id);
    }

    // Rebuild the operator index.
    for (&id, class) in &pruned.classes {
        for node in &class.nodes {
            pruned
                .classes_by_op
                .entry(node.discriminant())
                .or_default()
                .insert(id);
        }
    }

    // The graph has been rebuilt directly, so there is no rebuild work left.
    debug_assert!(pruned.pending.is_empty());
    debug_assert!(pruned.analysis_pending.is_empty());
    pruned.clean = true;

    // The extracted root must remain represented by the pruned graph.
    let root = egraph.find(info.root);
    assert!(
        pruned.classes.contains_key(&root),
        "extractor root {} was not preserved during pruning",
        root
    );

    pruned
}
