use crate::{Id, Language};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct SelectedNode<L: Language> {
    pub node_index: usize,
    pub node: L,
}

#[derive(Clone, Debug)]
pub struct ExtractorInfo<L: Language> {
    pub root: Id,
    pub selected_nodes: HashMap<Id, SelectedNode<L>>,
}
