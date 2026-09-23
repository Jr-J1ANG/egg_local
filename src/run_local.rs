use core::fmt::{self, Debug, Formatter};

use crate::no_std_prelude::*;
use log::*;

use crate::*;

/// A local equality-saturation runner.
///
/// `RunnerLocal` is intentionally separate from [`Runner`]. The normal
/// `Runner` is left untouched; this runner differs only in how rewrite
/// search is seeded: each rewrite is searched only from the e-class IDs
/// in `local_scope`. Pattern matching may still traverse child e-classes.
///
/// The implementation keeps the same iteration/apply/rebuild/stop
/// lifecycle as `Runner`. Its scheduler is local-aware and preserves the
/// exponential backoff behavior of egg's default `BackoffScheduler`.
///
/// This first version uses `()` as iteration data, matching the current
/// use of `Runner` in this project.
pub struct RunnerLocal<L: Language, N: Analysis<L>> {
    /// The [`EGraph`] used.
    pub egraph: EGraph<L, N>,
    /// Data accumulated over each [`Iteration`].
    pub iterations: Vec<Iteration<()>>,
    /// The roots of expressions added by [`with_expr`](RunnerLocal::with_expr).
    pub roots: Vec<Id>,
    /// Why the runner stopped.
    pub stop_reason: Option<StopReason>,
    /// Hooks added by [`with_hook`](RunnerLocal::with_hook).
    #[allow(clippy::type_complexity)]
    pub hooks: Vec<Box<dyn FnMut(&mut Self) -> Result<(), String>>>,

    limits: LocalRunnerLimits,
    scheduler: Box<dyn LocalRewriteScheduler<L, N>>,
    local_scope: Vec<Id>,
}

#[derive(Debug)]
struct LocalRunnerLimits {
    iter_limit: usize,
    node_limit: usize,
    time_limit: Duration,
    start_time: Option<Instant>,
}

impl LocalRunnerLimits {
    fn check_limits<L, N>(&self, iteration: usize, egraph: &EGraph<L, N>) -> RunnerResult<()>
    where
        L: Language,
        N: Analysis<L>,
    {
        let elapsed = self.start_time.unwrap().elapsed();
        if elapsed > self.time_limit {
            return Err(StopReason::TimeLimit(elapsed.as_secs_f64()));
        }
        let size = egraph.total_size();
        if size > self.node_limit {
            return Err(StopReason::NodeLimit(size));
        }
        if iteration >= self.iter_limit {
            return Err(StopReason::IterationLimit(iteration));
        }
        Ok(())
    }
}

impl<L, N> Default for RunnerLocal<L, N>
where
    L: Language,
    N: Analysis<L> + Default,
{
    fn default() -> Self {
        Self::new(N::default())
    }
}

impl<L, N> Debug for RunnerLocal<L, N>
where
    L: Language,
    N: Analysis<L>,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunnerLocal")
            .field("egraph", &self.egraph)
            .field("iterations", &self.iterations)
            .field("roots", &self.roots)
            .field("stop_reason", &self.stop_reason)
            .field("hooks", &vec![format_args!("<dyn FnMut ..>"); self.hooks.len()])
            .field("local_scope", &self.local_scope)
            .field("limits", &self.limits)
            .field("scheduler", &format_args!("<dyn LocalRewriteScheduler ..>"))
            .finish()
    }
}

impl<L, N> RunnerLocal<L, N>
where
    L: Language,
    N: Analysis<L>,
{
    /// Create a new local runner with egg-like default limits.
    pub fn new(analysis: N) -> Self {
        Self {
            limits: LocalRunnerLimits {
                iter_limit: 30,
                node_limit: 10_000,
                time_limit: Duration::from_secs(5),
                start_time: None,
            },
            egraph: EGraph::new(analysis),
            roots: vec![],
            iterations: vec![],
            stop_reason: None,
            hooks: vec![],
            scheduler: Box::new(LocalBackoffScheduler::default()),
            local_scope: vec![],
        }
    }

    /// Set the e-class IDs from which rewrite matching is started.
    pub fn with_local_scope(mut self, local_scope: Vec<Id>) -> Self {
        self.local_scope = local_scope;
        self
    }

    /// Replace the local scope.
    pub fn set_local_scope(&mut self, local_scope: Vec<Id>) {
        self.local_scope = local_scope;
    }

    /// Set the iteration limit. Default: 30.
    pub fn with_iter_limit(mut self, iter_limit: usize) -> Self {
        self.limits.iter_limit = iter_limit;
        self
    }

    /// Set the e-graph size limit. Default: 10,000 enodes.
    pub fn with_node_limit(mut self, node_limit: usize) -> Self {
        self.limits.node_limit = node_limit;
        self
    }

    /// Set the runner time limit. Default: 5 seconds.
    pub fn with_time_limit(mut self, time_limit: Duration) -> Self {
        self.limits.time_limit = time_limit;
        self
    }

    /// Add a hook run at the beginning of each iteration.
    pub fn with_hook<F>(mut self, hook: F) -> Self
    where
        F: FnMut(&mut Self) -> Result<(), String> + 'static,
    {
        self.hooks.push(Box::new(hook));
        self
    }

    /// Change the local scheduler.
    pub fn with_scheduler(self, scheduler: impl LocalRewriteScheduler<L, N> + 'static) -> Self {
        Self {
            scheduler: Box::new(scheduler),
            ..self
        }
    }

    /// Add an expression to the egraph and record its root.
    pub fn with_expr(mut self, expr: &RecExpr<L>) -> Self {
        let id = self.egraph.add_expr(expr);
        self.roots.push(id);
        self
    }

    /// Replace the egraph.
    pub fn with_egraph(self, egraph: EGraph<L, N>) -> Self {
        Self { egraph, ..self }
    }

    /// Run until a stop condition is reached.
    pub fn run<'a, R>(mut self, rules: R) -> Self
    where
        R: IntoIterator<Item = &'a Rewrite<L, N>>,
        L: 'a,
        N: 'a,
    {
        let rules: Vec<&Rewrite<L, N>> = rules.into_iter().collect();
        check_local_rules(&rules);
        self.egraph.rebuild();
        loop {
            let iter = self.run_one(&rules);
            self.iterations.push(iter);
            let stop_reason = self.iterations.last().unwrap().stop_reason.clone();
            if let Some(stop_reason) = stop_reason.or_else(|| self.check_limits().err()) {
                info!("Stopping local runner: {:?}", stop_reason);
                self.stop_reason = Some(stop_reason);
                break;
            }
        }
        assert!(!self.iterations.is_empty());
        assert!(self.stop_reason.is_some());
        self
    }

    pub fn with_explanations_enabled(mut self) -> Self {
        self.egraph = self.egraph.with_explanations_enabled();
        self
    }

    pub fn without_explanation_length_optimization(mut self) -> Self {
        self.egraph = self.egraph.without_explanation_length_optimization();
        self
    }

    pub fn with_explanation_length_optimization(mut self) -> Self {
        self.egraph = self.egraph.with_explanation_length_optimization();
        self
    }

    pub fn with_explanations_disabled(mut self) -> Self {
        self.egraph = self.egraph.with_explanations_disabled();
        self
    }

    pub fn explain_equivalence(&mut self, left: &RecExpr<L>, right: &RecExpr<L>) -> Explanation<L> {
        self.egraph.explain_equivalence(left, right)
    }

    pub fn explain_matches(
        &mut self,
        left: &RecExpr<L>,
        right: &PatternAst<L>,
        subst: &Subst,
    ) -> Explanation<L> {
        self.egraph.explain_matches(left, right, subst)
    }

    #[cfg(feature = "std")]
    pub fn print_report(&self) {
        println!("{}", self.report())
    }

    pub fn report(&self) -> Report {
        Report {
            stop_reason: self.stop_reason.clone().unwrap(),
            iterations: self.iterations.len(),
            egraph_nodes: self.egraph.total_number_of_nodes(),
            egraph_classes: self.egraph.number_of_classes(),
            memo_size: self.egraph.total_size(),
            rebuilds: self.iterations.iter().map(|i| i.n_rebuilds).sum(),
            search_time: self.iterations.iter().map(|i| i.search_time).sum(),
            apply_time: self.iterations.iter().map(|i| i.apply_time).sum(),
            rebuild_time: self.iterations.iter().map(|i| i.rebuild_time).sum(),
            total_time: self.iterations.iter().map(|i| i.total_time).sum(),
        }
    }

    fn run_one(&mut self, rules: &[&Rewrite<L, N>]) -> Iteration<()> {
        assert!(self.stop_reason.is_none());
        info!("\nLocal iteration {}", self.iterations.len());

        self.try_start();
        let mut result = self.check_limits();
        let egraph_nodes = self.egraph.total_size();
        let egraph_classes = self.egraph.number_of_classes();

        let hook_time = Instant::now();
        let mut hooks = core::mem::take(&mut self.hooks);
        result = result.and_then(|_| {
            hooks
                .iter_mut()
                .try_for_each(|hook| hook(self).map_err(StopReason::Other))
        });
        self.hooks = hooks;
        let hook_time = hook_time.elapsed().as_secs_f64();

        let egraph_nodes_after_hooks = self.egraph.total_size();
        let egraph_classes_after_hooks = self.egraph.number_of_classes();
        let i = self.iterations.len();
        trace!("Local EGraph {:?}", self.egraph.dump());

        let start_time = Instant::now();
        let mut matches = Vec::new();
        let mut applied = IndexMap::default();

        result = result.and_then(|_| {
            matches = self.scheduler.search_rewrites_local(
                i,
                &self.egraph,
                rules,
                &self.local_scope,
            )?;
            Ok(())
        });

        let search_time = start_time.elapsed().as_secs_f64();
        info!("Local search time: {}", search_time);

        // Snapshot the e-class IDs that existed at the beginning of this
        // iteration. New e-classes created by rewrite application must be
        // added to the local scope for the *next* iteration.
        //
        // We intentionally take this snapshot before applying rewrites and
        // inspect the e-graph again before rebuild(). This is important:
        // a newly-created e-class may be unioned with an existing e-class
        // during rewrite application. After rebuild(), that newly-created
        // ID may no longer appear in `classes()`, so waiting until after
        // rebuild would lose it.
        let scope_before_iteration: Vec<Id> =
            self.egraph.classes().map(|eclass| eclass.id).collect();
        
        let apply_time = Instant::now();
        result = result.and_then(|_| {
            rules.iter().zip(matches).try_for_each(|(rw, ms)| {
                let total_matches: usize = ms.iter().map(|m| m.substs.len()).sum();
                debug!("Applying {} {} times", rw.name, total_matches);
                let actually_matched = self.scheduler.apply_rewrite(i, &mut self.egraph, rw, ms);
                if actually_matched > 0 {
                    if let Some(count) = applied.get_mut(&rw.name) {
                        *count += actually_matched;
                    } else {
                        applied.insert(rw.name.to_owned(), actually_matched);
                    }
                    debug!("Applied {} {} times", rw.name, actually_matched);
                }
                self.check_limits()
            })
        });
        
        // Collect every e-class ID allocated during this iteration.
        //
        // Do this before rebuild(), because rebuild() canonicalizes unions
        // and can remove newly-created IDs from `classes()`. We keep the
        // original IDs (rather than only their post-rebuild canonical IDs)
        // so that the local scope records everything touched/generated by
        // this local saturation run.
        let mut new_local_ids = Vec::new();
        for eclass in self.egraph.classes() {
            let id = eclass.id;
            if !scope_before_iteration.contains(&id)
                && !new_local_ids.contains(&id)
            {
                new_local_ids.push(id);
            }
        }
        
        let apply_time = apply_time.elapsed().as_secs_f64();
        info!(
            "Local apply time: {}, new local e-classes: {}",
            apply_time,
            new_local_ids.len()
        );

        let rebuild_time = Instant::now();
        let n_rebuilds = self.egraph.rebuild();

        // Newly-created e-classes become searchable only in the next
        // iteration, preserving egg's normal search -> apply -> rebuild
        // iteration semantics.
        for id in new_local_ids {
            if !self.local_scope.contains(&id) {
                self.local_scope.push(id);
            }
        }
        if self.egraph.are_explanations_enabled() {
            debug_assert!(self.egraph.check_each_explain(rules));
        }
        let rebuild_time = rebuild_time.elapsed().as_secs_f64();
        info!("Local rebuild time: {}", rebuild_time);
        info!(
            "Local size: n={}, e={}",
            self.egraph.total_size(),
            self.egraph.number_of_classes()
        );

        let can_be_saturated = applied.is_empty()
            && self.scheduler.can_stop(i)
            && (egraph_nodes == egraph_nodes_after_hooks)
            && (egraph_classes == egraph_classes_after_hooks)
            && (egraph_nodes == self.egraph.total_size())
            && (egraph_classes == self.egraph.number_of_classes());

        if can_be_saturated {
            result = result.and(Err(StopReason::Saturated));
        }

        Iteration {
            applied,
            egraph_nodes,
            egraph_classes,
            hook_time,
            search_time,
            apply_time,
            rebuild_time,
            n_rebuilds,
            data: (),
            total_time: start_time.elapsed().as_secs_f64(),
            stop_reason: result.err(),
        }
    }

    fn try_start(&mut self) {
        self.limits.start_time.get_or_insert_with(Instant::now);
    }

    fn check_limits(&self) -> RunnerResult<()> {
        self.limits.check_limits(self.iterations.len(), &self.egraph)
    }
}

/// Local equivalent of egg's rewrite scheduler.
///
/// Unlike `RewriteScheduler`, its search entry point receives the local
/// scope. This keeps the original scheduler API in `run.rs` unchanged.
pub trait LocalRewriteScheduler<L, N>
where
    L: Language,
    N: Analysis<L>,
{
    fn can_stop(&mut self, iteration: usize) -> bool {
        true
    }

    fn search_rewrite_local<'a>(
        &mut self,
        iteration: usize,
        egraph: &EGraph<L, N>,
        rewrite: &'a Rewrite<L, N>,
        local_scope: &[Id],
    ) -> Vec<SearchMatches<'a, L>>;

    fn search_rewrites_local<'a>(
        &mut self,
        iteration: usize,
        egraph: &EGraph<L, N>,
        rewrites: &[&'a Rewrite<L, N>],
        local_scope: &[Id],
    ) -> RunnerResult<Vec<Vec<SearchMatches<'a, L>>>> {
        let mut matches = Vec::new();
        for rw in rewrites {
            let ms = self.search_rewrite_local(iteration, egraph, rw, local_scope);
            matches.push(ms);
        }
        Ok(matches)
    }

    fn apply_rewrite(
        &mut self,
        _iteration: usize,
        egraph: &mut EGraph<L, N>,
        rewrite: &Rewrite<L, N>,
        matches: Vec<SearchMatches<L>>,
    ) -> usize {
        rewrite.apply(egraph, &matches).len()
    }
}

#[derive(Debug)]
pub struct LocalSimpleScheduler;

impl<L, N> LocalRewriteScheduler<L, N> for LocalSimpleScheduler
where
    L: Language,
    N: Analysis<L>,
{
    fn search_rewrite_local<'a>(
        &mut self,
        _iteration: usize,
        egraph: &EGraph<L, N>,
        rewrite: &'a Rewrite<L, N>,
        local_scope: &[Id],
    ) -> Vec<SearchMatches<'a, L>> {
        rewrite.search_local_with_limit(egraph, local_scope, usize::MAX)
    }
}

#[derive(Debug)]
pub struct LocalBackoffScheduler {
    default_match_limit: usize,
    default_ban_length: usize,
    stats: IndexMap<Symbol, LocalRuleStats>,
}

#[derive(Debug)]
struct LocalRuleStats {
    times_applied: usize,
    banned_until: usize,
    times_banned: usize,
    match_limit: usize,
    ban_length: usize,
}

impl Default for LocalBackoffScheduler {
    fn default() -> Self {
        Self {
            stats: Default::default(),
            default_match_limit: 1_000,
            default_ban_length: 5,
        }
    }
}

impl LocalBackoffScheduler {
    pub fn with_initial_match_limit(mut self, limit: usize) -> Self {
        self.default_match_limit = limit;
        self
    }

    pub fn with_ban_length(mut self, ban_length: usize) -> Self {
        self.default_ban_length = ban_length;
        self
    }

    fn rule_stats(&mut self, name: Symbol) -> &mut LocalRuleStats {
        if self.stats.contains_key(&name) {
            &mut self.stats[&name]
        } else {
            self.stats.entry(name).or_insert(LocalRuleStats {
                times_applied: 0,
                banned_until: 0,
                times_banned: 0,
                match_limit: self.default_match_limit,
                ban_length: self.default_ban_length,
            })
        }
    }

    pub fn do_not_ban(mut self, name: impl Into<Symbol>) -> Self {
        self.rule_stats(name.into()).match_limit = usize::MAX;
        self
    }

    pub fn rule_match_limit(mut self, name: impl Into<Symbol>, limit: usize) -> Self {
        self.rule_stats(name.into()).match_limit = limit;
        self
    }

    pub fn rule_ban_length(mut self, name: impl Into<Symbol>, length: usize) -> Self {
        self.rule_stats(name.into()).ban_length = length;
        self
    }
}

impl<L, N> LocalRewriteScheduler<L, N> for LocalBackoffScheduler
where
    L: Language,
    N: Analysis<L>,
{
    fn can_stop(&mut self, iteration: usize) -> bool {
        let n_stats = self.stats.len();
        let mut banned: Vec<_> = self
            .stats
            .iter_mut()
            .filter(|(_, s)| s.banned_until > iteration)
            .collect();

        if banned.is_empty() {
            true
        } else {
            let min_ban = banned
                .iter()
                .map(|(_, s)| s.banned_until)
                .min()
                .expect("banned cannot be empty here");
            assert!(min_ban >= iteration);
            let delta = min_ban - iteration;
            let mut unbanned = vec![];
            for (name, s) in &mut banned {
                s.banned_until -= delta;
                if s.banned_until == iteration {
                    unbanned.push(name.as_str());
                }
            }
            assert!(!unbanned.is_empty());
            info!(
                "Banned {}/{}, fast-forwarded by {} to unban {}",
                banned.len(), n_stats, delta, unbanned.join(", ")
            );
            false
        }
    }

    fn search_rewrite_local<'a>(
        &mut self,
        iteration: usize,
        egraph: &EGraph<L, N>,
        rewrite: &'a Rewrite<L, N>,
        local_scope: &[Id],
    ) -> Vec<SearchMatches<'a, L>> {
        let stats = self.rule_stats(rewrite.name);
        if iteration < stats.banned_until {
            debug!(
                "Skipping {} ({}-{}), banned until {}...",
                rewrite.name, stats.times_applied, stats.times_banned, stats.banned_until
            );
            return vec![];
        }

        let threshold = stats
            .match_limit
            .checked_shl(stats.times_banned as u32)
            .unwrap();
        let matches = rewrite.search_local_with_limit(
            egraph,
            local_scope,
            threshold.saturating_add(1),
        );
        let total_len: usize = matches.iter().map(|m| m.substs.len()).sum();
        if total_len > threshold {
            let ban_length = stats.ban_length << stats.times_banned;
            stats.times_banned += 1;
            stats.banned_until = iteration + ban_length;
            info!(
                "Banning {} ({}-{}) for {} iters: {} < {}",
                rewrite.name,
                stats.times_applied,
                stats.times_banned,
                ban_length,
                threshold,
                total_len
            );
            vec![]
        } else {
            stats.times_applied += 1;
            matches
        }
    }
}

fn check_local_rules<L, N>(rules: &[&Rewrite<L, N>]) {
    let mut name_counts = IndexMap::default();
    for rw in rules {
        *name_counts.entry(rw.name).or_default() += 1;
    }
    name_counts.retain(|_, count: &mut usize| *count > 1);
    if !name_counts.is_empty() {
        #[cfg(feature = "std")]
        eprintln!("WARNING: Duplicated rule names may affect rule reporting and scheduling.");
        log::warn!("Duplicated rule names may affect rule reporting and scheduling.");
        for (name, &count) in name_counts.iter() {
            assert!(count > 1);
            #[cfg(feature = "std")]
            eprintln!("Rule '{}' appears {} times", name, count);
            log::warn!("Rule '{}' appears {} times", name, count);
        }
    }
}
