# egg_local

A fork of [egg](https://github.com/egraphs-good/egg) for **local equality saturation**.

The main purpose of this fork is to support saturation restricted to a selected set of e-classes, rather than saturating the entire e-graph. 

You can lone this repository to a local path:

```bash
git clone https://github.com/Jr-J1ANG/egg_local.git
```

Then use the local copy as a dependency in the project that needs it:

```toml
[dependencies]
egg = { path = "path_to_egraph_local" }
```

## RunnerLocal

This fork provides `RunnerLocal`, a runner designed similarly to egg's native `Runner`. The code modifications made to `RunnerLocal` are independent and do not affect the behavior of the original `Runner`.

## Installation

Import it with:

```rust
use egg::{Id, RunnerLocal};
```

It can be used in a similar way to the original `Runner`:

```rust
let runner = RunnerLocal::default()
    .with_egraph(egraph)
    .with_iter_limit(ITER_LIMIT)
    .with_node_limit(NODE_LIMIT)
    .with_time_limit(Duration::from_secs(TIME_LIMIT_SECS))
    .with_local_scope(local_scope)
    .run(&rules());
```

The `local_scope` is a `Vec<Id>` containing the IDs of the e-classes that define the local scope for sarturation.

For example:

```rust
let local_scope = vec![
    Id::from(id),
];
```

## Local Saturation Semantics

During local saturation, rewrite-rule matching and application are restricted to the e-classes contained in `local_scope`.

New e-classes created by rewrites during the saturation process are also added to `local_scope`, unless they are merged into an existing e-class outside the current local scope. (This restriction prevents the local scope from recursively expanding through existing e-graph structure outside the selected region.)

Thus when matching a pattern, if an e-node has no child e-class that is inside the current local scope, that e-node is not recursively expanded for pattern matching. It therefore behaves as a local leaf from the perspective of the current saturation scope.

