//! Load order.
//!
//! Plugins declare what they `require` and what they would `optional`ly like
//! to come after; both are edges of a graph over plugin ids and `provides`
//! aliases. The order is a topological sort in which ties go to the plugin
//! discovered first, so the same set of plugins always loads the same way.
//!
//! This module works on plain data and never touches the file system.

/// What the planner needs to know about a plugin.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub id: String,
    pub provides: Vec<String>,
    pub requires: Vec<String>,
    pub optional: Vec<String>,
}

/// A decision made before any plugin code runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Not loaded, but not an error: a dependency is not installed.
    Skip(String),
    /// Cannot be loaded: it sits on a dependency cycle.
    Fail(String),
}

#[derive(Debug, Default)]
pub struct Plan {
    /// Every node, dependencies first.
    pub order: Vec<usize>,
    /// Set for nodes that must not be loaded, by node index.
    pub verdicts: Vec<Option<Verdict>>,
    /// `(name, node)` for every id and every `provides` alias.
    names: Vec<(String, usize)>,
}

impl Plan {
    /// Nodes that satisfy `name`: the plugin with that id and every plugin
    /// that lists it under `provides`.
    pub fn providers(&self, name: &str) -> Vec<usize> {
        self.names
            .iter()
            .filter(|(candidate, _)| candidate == name)
            .map(|&(_, node)| node)
            .collect()
    }
}

// Plain vectors and linear scans on purpose: a handful of plugins does not
// need a map, and the loader pays for every collection type it instantiates
// (ADR 0004).

/// Orders `nodes`. Nodes keep their index; `order` lists those indexes.
pub fn plan(nodes: &[Node]) -> Plan {
    let mut names: Vec<(String, usize)> = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        for name in std::iter::once(&node.id).chain(&node.provides) {
            if !names
                .iter()
                .any(|(known, owner)| known == name && *owner == index)
            {
                names.push((name.clone(), index));
            }
        }
    }
    let mut plan = Plan {
        order: Vec::with_capacity(nodes.len()),
        verdicts: vec![None; nodes.len()],
        names,
    };

    // dependencies[i]: nodes that have to come before i.
    let mut dependencies: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        for (name, required) in node
            .requires
            .iter()
            .map(|name| (name, true))
            .chain(node.optional.iter().map(|name| (name, false)))
        {
            let found = plan.providers(name);
            if found.is_empty() && required && plan.verdicts[index].is_none() {
                plan.verdicts[index] = Some(Verdict::Skip(format!("needs {name}, not installed")));
            }
            for provider in found {
                if !dependencies[index].contains(&provider) {
                    dependencies[index].push(provider);
                }
            }
        }
    }

    let mut done = vec![false; nodes.len()];
    while plan.order.len() < nodes.len() {
        let ready = (0..nodes.len())
            .find(|&i| !done[i] && dependencies[i].iter().all(|&dependency| done[dependency]));
        if let Some(next) = ready {
            done[next] = true;
            plan.order.push(next);
            continue;
        }

        // Nothing is ready, so what is left holds at least one cycle (and
        // possibly plugins that merely wait for it). Every unfinished node
        // waits for another unfinished node: follow those links from the
        // first one until a node repeats.
        let start = (0..nodes.len()).find(|&i| !done[i]).expect("work left");
        let mut walk = vec![start];
        let cycle = loop {
            let current = walk[walk.len() - 1];
            let next = *dependencies[current]
                .iter()
                .find(|&&dependency| !done[dependency])
                .expect("a waiting node has an unfinished dependency");
            if let Some(at) = walk.iter().position(|&seen| seen == next) {
                break walk[at..].to_vec();
            }
            walk.push(next);
        };
        let mut names: Vec<&str> = cycle.iter().map(|&i| nodes[i].id.as_str()).collect();
        names.push(names[0]);
        let message = format!("dependency cycle: {}", names.join(" -> "));
        // Members are listed in discovery order like everything else.
        for member in 0..nodes.len() {
            if cycle.contains(&member) {
                plan.verdicts[member] = Some(Verdict::Fail(message.clone()));
                done[member] = true;
                plan.order.push(member);
            }
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, requires: &[&str], optional: &[&str], provides: &[&str]) -> Node {
        let owned = |list: &[&str]| list.iter().map(|item| item.to_string()).collect();
        Node {
            id: id.to_string(),
            requires: owned(requires),
            optional: owned(optional),
            provides: owned(provides),
        }
    }

    fn ids(nodes: &[Node], plan: &Plan) -> Vec<String> {
        plan.order.iter().map(|&i| nodes[i].id.clone()).collect()
    }

    #[test]
    fn independent_plugins_keep_discovery_order() {
        let nodes = [
            node("c", &[], &[], &[]),
            node("a", &[], &[], &[]),
            node("b", &[], &[], &[]),
        ];
        let plan = plan(&nodes);
        assert_eq!(ids(&nodes, &plan), ["c", "a", "b"]);
        assert!(plan.verdicts.iter().all(Option::is_none));
    }

    #[test]
    fn a_dependency_loads_before_its_dependent() {
        let nodes = [
            node("app", &["lib"], &[], &[]),
            node("other", &[], &[], &[]),
            node("lib", &[], &[], &[]),
        ];
        // `other` was discovered before `lib` and has no edges, so it stays
        // ahead of it.
        assert_eq!(ids(&nodes, &plan(&nodes)), ["other", "lib", "app"]);
    }

    #[test]
    fn optional_dependencies_only_order() {
        let nodes = [
            node("app", &[], &["lib", "absent"], &[]),
            node("lib", &[], &[], &[]),
        ];
        let plan = plan(&nodes);
        assert_eq!(ids(&nodes, &plan), ["lib", "app"]);
        assert!(plan.verdicts[0].is_none(), "a missing optional is ignored");
    }

    #[test]
    fn provides_lets_an_alias_satisfy_a_requirement() {
        let nodes = [
            node("app", &["js-runtime"], &[], &[]),
            node("boa", &[], &[], &["js-runtime"]),
        ];
        let plan = plan(&nodes);
        assert_eq!(ids(&nodes, &plan), ["boa", "app"]);
        assert_eq!(plan.providers("js-runtime"), [1]);
        assert_eq!(plan.providers("boa"), [1]);
        assert!(plan.providers("nobody").is_empty());
    }

    #[test]
    fn a_missing_requirement_is_a_skip_naming_it() {
        let nodes = [node("app", &["com.parsec-orbit.js-runtime"], &[], &[])];
        let plan = plan(&nodes);
        assert_eq!(
            plan.verdicts[0],
            Some(Verdict::Skip(
                "needs com.parsec-orbit.js-runtime, not installed".into()
            ))
        );
        assert_eq!(plan.order, [0], "it still has a place in the order");
    }

    #[test]
    fn a_cycle_fails_every_member_and_prints_it() {
        let nodes = [
            node("a", &["b"], &[], &[]),
            node("b", &["c"], &[], &[]),
            node("c", &["a"], &[], &[]),
            node("bystander", &[], &[], &[]),
        ];
        let plan = plan(&nodes);
        for member in 0..3 {
            match &plan.verdicts[member] {
                Some(Verdict::Fail(message)) => {
                    assert_eq!(message, "dependency cycle: a -> b -> c -> a")
                }
                other => panic!("{member}: {other:?}"),
            }
        }
        assert_eq!(plan.verdicts[3], None);
        assert_eq!(plan.order.len(), 4);
    }

    #[test]
    fn a_plugin_that_requires_itself_is_a_cycle() {
        let nodes = [node("a", &["a"], &[], &[])];
        match &plan(&nodes).verdicts[0] {
            Some(Verdict::Fail(message)) => assert_eq!(message, "dependency cycle: a -> a"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn plugins_waiting_on_a_cycle_are_still_ordered_after_it() {
        let nodes = [
            node("late", &["a"], &[], &[]),
            node("a", &["b"], &[], &[]),
            node("b", &["a"], &[], &[]),
        ];
        let plan = plan(&nodes);
        assert_eq!(plan.verdicts[0], None);
        assert!(matches!(plan.verdicts[1], Some(Verdict::Fail(_))));
        assert_eq!(ids(&nodes, &plan), ["a", "b", "late"]);
    }

    #[test]
    fn two_separate_cycles_are_both_found() {
        let nodes = [
            node("a", &["b"], &[], &[]),
            node("b", &["a"], &[], &[]),
            node("c", &["d"], &[], &[]),
            node("d", &["c"], &[], &[]),
        ];
        let plan = plan(&nodes);
        assert!(plan
            .verdicts
            .iter()
            .all(|verdict| matches!(verdict, Some(Verdict::Fail(_)))));
        assert_eq!(plan.order.len(), 4);
    }

    #[test]
    fn the_order_is_deterministic_for_a_diamond() {
        let nodes = [
            node("top", &["left", "right"], &[], &[]),
            node("right", &["base"], &[], &[]),
            node("left", &["base"], &[], &[]),
            node("base", &[], &[], &[]),
        ];
        let first = ids(&nodes, &plan(&nodes));
        assert_eq!(first, ["base", "right", "left", "top"]);
        assert_eq!(first, ids(&nodes, &plan(&nodes)));
    }

    #[test]
    fn alternative_providers_of_an_alias_all_come_first() {
        let nodes = [
            node("app", &["engine"], &[], &[]),
            node("engine-a", &[], &[], &["engine"]),
            node("engine-b", &[], &[], &["engine"]),
        ];
        assert_eq!(ids(&nodes, &plan(&nodes)), ["engine-a", "engine-b", "app"]);
    }
}
