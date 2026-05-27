//! `PluginSpecSet` composition and capability-contract load ordering.
//!
//! This module implements two pure-logic operations (no I/O, no network) that
//! are the core of Phase-3's management engine:
//!
//! 1. **[`compose`]** — merges one or more [`ConfigSpec`] layers (user
//!    `plugins.lua`, `managed.lua`, per-identity overlay) into a
//!    [`PluginSpecSet`], validating every Lua key as a [`PluginName`] and
//!    applying last-writer-wins per key.
//!
//! 2. **[`load_order`]** — given the [`mote_lua::Manifest`]s of the plugins in
//!    a resolved spec-set, produces a deterministic topological ordering where
//!    every capability fulfiller precedes its consumers. Surfaces
//!    dangling-consumer gaps and capability cycles as typed errors before any
//!    load is attempted.
//!
//! ## ADR references
//!
//! - ADR-0002: capabilities-only inter-plugin dependencies (no `requires`, no
//!   semver). `load_order` is therefore capability-contract ordering, not a
//!   semver resolver.
//! - ADR-0006: managed layer loaded last; per key last-writer-wins compose
//!   semantics.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use mote_lua::{ConfigSpec, Manifest};
use mote_types::{PluginName, PluginNameError};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single plugin spec as resolved from `plugins.lua` / `managed.lua`.
///
/// The `source` string is kept unparsed; parsing into a [`crate::source::Source`]
/// is `source.rs`'s responsibility, performed later by the manager façade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    /// The canonical, validated plugin name (the `plugins.lua` quoted key,
    /// validated per ADR-0006 / R4: keys are `PluginName`s).
    pub name: PluginName,
    /// The raw source string exactly as written in config (e.g.
    /// `"github:mote-browser/adblock"`, `"path:~/code/my-plugin"`,
    /// `"bundled"`).
    pub source: String,
    /// The optional version/tag/branch constraint.
    pub version: Option<String>,
}

/// The resolved plugin declaration set, keyed by [`PluginName`].
///
/// Produced by [`compose`] from one or more [`ConfigSpec`] layers.
/// Uses a [`BTreeMap`] so iteration order is deterministic (lexicographic by
/// [`PluginName`]) — required by `plugins.lock` key ordering and by
/// [`load_order`]'s tie-breaking rule.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginSpecSet {
    /// The resolved specs, keyed by canonical [`PluginName`].
    pub specs: BTreeMap<PluginName, PluginSpec>,
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by [`compose`] or [`load_order`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ResolveError {
    /// A `plugins.lua` entry key is not a valid [`PluginName`].
    ///
    /// Per ADR-0006 / R4, keys must be valid quoted hyphenated `PluginName`s
    /// (e.g. `["vim-mode"] = {…}`). An underscore or other invalid character
    /// is rejected here with a clear message before any further resolution.
    #[error("plugin key {key:?} in {source} is not a valid plugin name: {cause}")]
    InvalidKey {
        /// The offending Lua key string.
        key: String,
        /// A description of which config layer the entry came from (e.g.
        /// `"plugins.lua"`, `"managed.lua"`, `"per-identity overlay"`).
        source: String,
        /// The underlying validation failure.
        #[source]
        cause: PluginNameError,
    },

    /// One or more plugins consume a capability that no plugin in the set
    /// fulfills.
    ///
    /// Detected pre-flight by [`load_order`] before any load is attempted (the
    /// runtime raises the same error at load-step 1, but `pluginmgr` surfaces
    /// it earlier with the resolution hint). All dangling consumers are
    /// collected in a single error so the user can fix them in one pass.
    #[error(
        "dangling capability consumers: {}",
        .0.iter()
            .map(|(c, cap)| format!("{c} consumes {cap:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )]
    DanglingConsumers(
        /// All `(consumer PluginName, capability name)` pairs where no
        /// fulfiller exists, in deterministic (lexicographic) order.
        Vec<(PluginName, String)>,
    ),

    /// A cycle exists in the capability-consumes graph.
    ///
    /// Capability cycles are unsupported in v0.1 (documented in
    /// `docs/plans/03-risks.md` R6). The plugins involved in the cycle are
    /// collected and reported so the user can identify the mutual dependency.
    #[error(
        "capability dependency cycle detected among plugins: {}",
        .0.iter()
            .map(PluginName::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )]
    CapabilityCycle(
        /// The plugins involved in the cycle, in lexicographic order.
        Vec<PluginName>,
    ),
}

// ---------------------------------------------------------------------------
// compose
// ---------------------------------------------------------------------------

/// Merges `layers` of [`ConfigSpec`]s into a single [`PluginSpecSet`].
///
/// ## Composition semantics (ADR-0006)
///
/// Layers are applied **in order**. The expected caller order is:
///
/// ```text
/// [user plugins.lua, managed.lua, per-identity overlay]
/// ```
///
/// - Each layer is processed left-to-right, entry by entry.
/// - On a **key collision** (same [`PluginName`] appears in two layers), the
///   **later layer's entry wins** (last-writer-wins per key — not whole-set
///   replacement). This means `managed.lua` can override a key from
///   `plugins.lua`, and a per-identity overlay can override both.
/// - Plugins present in an earlier layer but absent in a later layer are
///   **preserved** (additive union with per-key override, not replacement).
/// - The output is keyed by [`PluginName`], sorted lexicographically.
///
/// ## Key validation (R4 / ADR-0006)
///
/// Every `PluginEntry.key` in every layer must be a valid [`PluginName`]
/// (lowercase ASCII, hyphens only, no underscores, non-empty). An invalid key
/// immediately returns [`ResolveError::InvalidKey`] with the layer index
/// rendered as `"layer N"` (1-based) so the caller can identify which file
/// contained the bad entry.
///
/// # Errors
///
/// Returns [`ResolveError::InvalidKey`] if any layer contains an entry whose
/// key is not a valid [`PluginName`].
pub fn compose(layers: &[&ConfigSpec]) -> Result<PluginSpecSet, ResolveError> {
    let mut specs: BTreeMap<PluginName, PluginSpec> = BTreeMap::new();

    for (layer_idx, layer) in layers.iter().enumerate() {
        let source_label = format!("layer {}", layer_idx + 1);
        for entry in &layer.plugins {
            let name =
                PluginName::new(entry.key.clone()).map_err(|cause| ResolveError::InvalidKey {
                    key: entry.key.clone(),
                    source: source_label.clone(),
                    cause,
                })?;
            // Last-writer-wins: insert unconditionally, overwriting any prior
            // entry with the same PluginName.
            specs.insert(
                name.clone(),
                PluginSpec {
                    name,
                    source: entry.source.clone(),
                    version: entry.version.clone(),
                },
            );
        }
    }

    Ok(PluginSpecSet { specs })
}

// ---------------------------------------------------------------------------
// load_order
// ---------------------------------------------------------------------------

/// Produces a deterministic topological load order for `manifests`.
///
/// Every plugin whose `capabilities` list fulfills a capability consumed by
/// another plugin must appear **before** that consumer in the returned
/// [`Vec`]. This mirrors the runtime's "fulfiller must already be loaded"
/// requirement at load-step 1 (DESIGN §Resolution at load time; ADR-0002).
///
/// ## Algorithm
///
/// 1. Build a `capability → [fulfillers]` map from all manifests.
/// 2. For each plugin, compute its **transitive dependencies**: the set of
///    plugins that must load before it (i.e. all fulfillers of each capability
///    it consumes).
/// 3. Run **Kahn's algorithm** (BFS topological sort) over the
///    dependency edges, breaking ties at each BFS frontier by
///    **[`PluginName`] lexicographic order**. This makes the output
///    deterministic regardless of input order (Lua table and map iteration
///    order are not stable — sorting is the determinism anchor).
/// 4. After Kahn's finishes, any plugin not yet emitted is part of a cycle
///    (its in-degree never reached zero because it depends on something that
///    also depends on it).
///
/// ## Multiple fulfillers
///
/// A capability fulfilled by more than one plugin is not an error here —
/// exclusivity is the runtime's check ([`crate::capability::CapabilityMap::claim`]).
/// All fulfillers are added as predecessors of the consumer, so they all load
/// before it.
///
/// ## Error collection
///
/// **Dangling consumers** (plugins that `consumes` a capability no plugin in
/// the set fulfills) are **all collected** before returning, so the user can
/// fix them in one pass rather than encountering them one at a time.
///
/// **Capability cycles** (e.g. plugin A fulfills cap X and consumes cap Y,
/// while plugin B fulfills cap Y and consumes cap X) are detected by Kahn's
/// residue: any plugin not emitted after the sort is part of a cycle. The
/// involved plugins are reported as a [`ResolveError::CapabilityCycle`].
/// Cycles take precedence over dangling-consumer errors (a cycle is reported
/// even if there are also dangling consumers in the non-cyclic portion, but
/// if both exist both errors could theoretically be present; in practice the
/// two are orthogonal and only one error variant is returned — dangling
/// consumers if no cycle, cycle if any cycle).
///
/// # Errors
///
/// - [`ResolveError::DanglingConsumers`] — one or more plugins consume an
///   unfulfilled capability (returned before cycle detection runs on the
///   non-dangling portion).
/// - [`ResolveError::CapabilityCycle`] — a cycle exists in the consumes→fulfills
///   graph.
pub fn load_order(manifests: &[Manifest]) -> Result<Vec<PluginName>, ResolveError> {
    // Step 1: build capability → [fulfillers] index.
    // BTreeMap for determinism.
    let mut cap_fulfillers: BTreeMap<String, Vec<PluginName>> = BTreeMap::new();
    for m in manifests {
        for cap in &m.capabilities {
            cap_fulfillers
                .entry(cap.clone())
                .or_default()
                .push(m.name.clone());
        }
    }

    // Step 2: pre-flight dangling-consumer check — collect ALL missing
    // (consumer, capability) pairs before attempting the sort.
    let mut dangling: Vec<(PluginName, String)> = Vec::new();
    for m in manifests {
        for cap in &m.consumes {
            if !cap_fulfillers.contains_key(cap.as_str()) {
                dangling.push((m.name.clone(), cap.clone()));
            }
        }
    }
    if !dangling.is_empty() {
        dangling.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        dangling.dedup();
        return Err(ResolveError::DanglingConsumers(dangling));
    }

    // Step 3: build adjacency and in-degree for Kahn's algorithm.
    // An edge  predecessor → successor  means "predecessor must load before
    // successor" (predecessor fulfills a capability successor consumes).
    //
    // Use a HashMap for O(1) lookups during edge building; we sort at output.
    let mut in_degree: HashMap<PluginName, usize> =
        manifests.iter().map(|m| (m.name.clone(), 0)).collect();

    // predecessors[X] = set of plugins that must load before X.
    let mut predecessors: HashMap<PluginName, BTreeSet<PluginName>> = manifests
        .iter()
        .map(|m| (m.name.clone(), BTreeSet::new()))
        .collect();

    // For each consumer, add its fulfillers as predecessors.
    for m in manifests {
        let consumer = &m.name;
        for cap in &m.consumes {
            // cap_fulfillers only contains fulfilled caps at this point
            // (dangling check passed).
            if let Some(fulfillers) = cap_fulfillers.get(cap.as_str()) {
                for fulfiller in fulfillers {
                    // A plugin cannot be its own predecessor (self-loop).
                    if fulfiller == consumer {
                        continue;
                    }
                    // Only add an edge if not already present.
                    if predecessors
                        .get_mut(consumer)
                        .is_some_and(|s| s.insert(fulfiller.clone()))
                    {
                        *in_degree.entry(consumer.clone()).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // Step 4: Kahn's BFS with tie-breaking by PluginName sort.
    //
    // Seed the queue with all nodes whose in-degree is 0, sorted for
    // determinism.
    let mut queue: VecDeque<PluginName> = {
        let mut zero: Vec<PluginName> = in_degree
            .iter()
            .filter(|&(_, &deg)| deg == 0)
            .map(|(name, _)| name.clone())
            .collect();
        zero.sort_unstable();
        zero.into()
    };

    // successors[X] = list of plugins that have X as a predecessor (i.e.
    // plugins that consume a capability X fulfills).
    // Build from predecessors map for Kahn's reverse traversal.
    let mut successors: HashMap<PluginName, Vec<PluginName>> = manifests
        .iter()
        .map(|m| (m.name.clone(), Vec::new()))
        .collect();
    for (consumer, preds) in &predecessors {
        for pred in preds {
            successors
                .entry(pred.clone())
                .or_default()
                .push(consumer.clone());
        }
    }

    let mut order: Vec<PluginName> = Vec::with_capacity(manifests.len());

    while let Some(current) = queue.pop_front() {
        order.push(current.clone());

        // Collect successors of `current`, decrement their in-degrees.
        let succs = successors.get(&current).cloned().unwrap_or_default();
        let mut newly_zero: Vec<PluginName> = Vec::new();
        for succ in succs {
            let deg = in_degree.entry(succ.clone()).or_insert(0);
            *deg -= 1;
            if *deg == 0 {
                newly_zero.push(succ);
            }
        }
        // Sort for determinism before extending the queue.
        newly_zero.sort_unstable();
        queue.extend(newly_zero);
    }

    // Step 5: cycle detection — any plugin not in `order` is in a cycle.
    if order.len() != manifests.len() {
        let ordered_set: BTreeSet<&PluginName> = order.iter().collect();
        let mut cycle_members: Vec<PluginName> = manifests
            .iter()
            .map(|m| &m.name)
            .filter(|n| !ordered_set.contains(n))
            .cloned()
            .collect();
        cycle_members.sort_unstable();
        return Err(ResolveError::CapabilityCycle(cycle_members));
    }

    Ok(order)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use mote_lua::{ConfigSpec, PluginEntry, eval_config, load_plugin};

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Build a minimal [`ConfigSpec`] inline (bypassing Lua eval) for tests
    /// that only need compose behaviour.
    fn make_spec(entries: &[(&str, &str)]) -> ConfigSpec {
        ConfigSpec {
            plugins: entries
                .iter()
                .map(|(key, src)| PluginEntry {
                    key: (*key).to_owned(),
                    source: (*src).to_owned(),
                    version: None,
                })
                .collect(),
            ..ConfigSpec::default()
        }
    }

    /// Build a [`Manifest`] from inline Lua source (same pattern as
    /// `mote-runtime/src/approval.rs` tests).
    fn manifest(src: &str) -> Manifest {
        load_plugin(src, "test").unwrap().manifest().clone()
    }

    /// Minimal plugin Lua with configurable name, capabilities, and consumes.
    fn plugin_lua(name: &str, capabilities: &[&str], consumes: &[&str]) -> String {
        let caps = lua_string_array(capabilities);
        let cons = lua_string_array(consumes);
        format!(
            r#"
            local M = {{}}
            M.manifest = {{
                schema = "v1",
                name = "{name}",
                version = "1",
                capabilities = {caps},
                consumes = {cons},
            }}
            return M
        "#
        )
    }

    fn lua_string_array(items: &[&str]) -> String {
        let inner: Vec<String> = items.iter().map(|s| format!(r#""{s}""#)).collect();
        format!("{{ {} }}", inner.join(", "))
    }

    // -----------------------------------------------------------------------
    // compose tests
    // -----------------------------------------------------------------------

    #[test]
    fn compose_single_layer_produces_all_entries() {
        let spec = make_spec(&[
            ("adblock", "github:mote-browser/adblock"),
            ("vim-mode", "bundled"),
        ]);
        let set = compose(&[&spec]).unwrap();
        assert_eq!(set.specs.len(), 2);
        assert!(set.specs.contains_key(&PluginName::new("adblock").unwrap()));
        assert!(
            set.specs
                .contains_key(&PluginName::new("vim-mode").unwrap())
        );
    }

    #[test]
    fn compose_managed_overrides_user_key() {
        // User declares "adblock" from bundled; managed overrides it to github.
        let user = make_spec(&[("adblock", "bundled")]);
        let managed = make_spec(&[("adblock", "github:mote-browser/adblock")]);

        let set = compose(&[&user, &managed]).unwrap();
        assert_eq!(set.specs.len(), 1);
        assert_eq!(
            set.specs[&PluginName::new("adblock").unwrap()].source,
            "github:mote-browser/adblock"
        );
    }

    #[test]
    fn compose_managed_adds_new_key() {
        let user = make_spec(&[("adblock", "bundled")]);
        let managed = make_spec(&[("vim-mode", "github:mote-browser/vim-mode")]);

        let set = compose(&[&user, &managed]).unwrap();
        assert_eq!(set.specs.len(), 2);
        assert!(set.specs.contains_key(&PluginName::new("adblock").unwrap()));
        assert!(
            set.specs
                .contains_key(&PluginName::new("vim-mode").unwrap())
        );
    }

    #[test]
    fn compose_disjoint_user_keys_preserved() {
        // User has A + B; managed has C; result has A + B + C.
        let user = make_spec(&[("plugin-a", "bundled"), ("plugin-b", "bundled")]);
        let managed = make_spec(&[("plugin-c", "path:~/plugins/c")]);

        let set = compose(&[&user, &managed]).unwrap();
        assert_eq!(set.specs.len(), 3);
        for n in ["plugin-a", "plugin-b", "plugin-c"] {
            assert!(
                set.specs.contains_key(&PluginName::new(n).unwrap()),
                "{n} missing from composed set"
            );
        }
    }

    #[test]
    fn compose_per_identity_overrides_both_earlier_layers() {
        let user = make_spec(&[("adblock", "bundled")]);
        let managed = make_spec(&[("adblock", "github:mote-browser/adblock")]);
        let identity = make_spec(&[("adblock", "path:~/my-adblock")]);

        let set = compose(&[&user, &managed, &identity]).unwrap();
        assert_eq!(
            set.specs[&PluginName::new("adblock").unwrap()].source,
            "path:~/my-adblock"
        );
    }

    #[test]
    fn compose_output_is_deterministic_same_layers_same_order() {
        let user = make_spec(&[("zebra", "bundled"), ("alpha", "bundled")]);
        let set1 = compose(&[&user]).unwrap();
        let set2 = compose(&[&user]).unwrap();
        assert_eq!(set1, set2);
        // BTreeMap iteration is in lexicographic key order.
        let names: Vec<&str> = set1.specs.keys().map(PluginName::as_str).collect();
        assert_eq!(names, vec!["alpha", "zebra"]);
    }

    #[test]
    fn compose_invalid_key_returns_error() {
        // "vim_mode" uses an underscore — not a valid PluginName.
        let bad = make_spec(&[("vim_mode", "bundled")]);
        let err = compose(&[&bad]).unwrap_err();
        match err {
            ResolveError::InvalidKey {
                ref key,
                ref source,
                ..
            } => {
                assert_eq!(key, "vim_mode");
                assert_eq!(source, "layer 1");
            }
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }

    #[test]
    fn compose_invalid_key_names_correct_layer() {
        let good = make_spec(&[("adblock", "bundled")]);
        let bad = make_spec(&[("vim_mode", "bundled")]); // layer 2
        let err = compose(&[&good, &bad]).unwrap_err();
        match err {
            ResolveError::InvalidKey { ref source, .. } => {
                assert_eq!(source, "layer 2");
            }
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }

    /// [`eval_config`] integration: compose works with real [`ConfigSpec`] from Lua
    #[test]
    fn compose_from_eval_config() {
        let lua = r#"
            mote.plugins({
                ["vim-mode"] = { source = "bundled" },
                ["adblock"]  = { source = "github:mote-browser/adblock" },
            })
        "#;
        let spec = eval_config(lua, "plugins.lua").unwrap();
        let set = compose(&[&spec]).unwrap();
        assert_eq!(set.specs.len(), 2);
        assert!(
            set.specs
                .contains_key(&PluginName::new("vim-mode").unwrap())
        );
        assert!(set.specs.contains_key(&PluginName::new("adblock").unwrap()));
    }

    // -----------------------------------------------------------------------
    // load_order tests
    // -----------------------------------------------------------------------

    #[test]
    fn load_order_single_plugin_no_deps() {
        let m = manifest(&plugin_lua("adblock", &[], &[]));
        let order = load_order(&[m]).unwrap();
        assert_eq!(order, vec![PluginName::new("adblock").unwrap()]);
    }

    #[test]
    fn load_order_linear_chain_fulfiller_before_consumer() {
        // B fulfills "x"; A consumes "x" => B must load before A.
        let a = manifest(&plugin_lua("consumer-a", &[], &["x"]));
        let b = manifest(&plugin_lua("fulfiller-b", &["x"], &[]));

        let order = load_order(&[a, b]).unwrap();
        let b_pos = order
            .iter()
            .position(|n| n.as_str() == "fulfiller-b")
            .unwrap();
        let a_pos = order
            .iter()
            .position(|n| n.as_str() == "consumer-a")
            .unwrap();
        assert!(b_pos < a_pos, "fulfiller-b must load before consumer-a");
    }

    #[test]
    fn load_order_diamond() {
        // D fulfills "d-cap"; B consumes "d-cap"; C consumes "d-cap";
        // A consumes "b-cap" and "c-cap"; B fulfills "b-cap"; C fulfills "c-cap".
        //          D
        //         / \
        //        B   C
        //         \ /
        //          A
        let d = manifest(&plugin_lua("d", &["d-cap"], &[]));
        let b = manifest(&plugin_lua("b", &["b-cap"], &["d-cap"]));
        let c = manifest(&plugin_lua("c", &["c-cap"], &["d-cap"]));
        let a = manifest(&plugin_lua("a", &[], &["b-cap", "c-cap"]));

        let order = load_order(&[a, b, c, d]).unwrap();
        let pos = |name: &str| order.iter().position(|n| n.as_str() == name).unwrap();
        assert!(pos("d") < pos("b"), "d before b");
        assert!(pos("d") < pos("c"), "d before c");
        assert!(pos("b") < pos("a"), "b before a");
        assert!(pos("c") < pos("a"), "c before a");
    }

    #[test]
    fn load_order_independent_plugins_sorted_by_name() {
        // No dependency edges => purely lexicographic order.
        let z = manifest(&plugin_lua("zebra", &[], &[]));
        let a = manifest(&plugin_lua("alpha", &[], &[]));
        let m = manifest(&plugin_lua("mid", &[], &[]));

        let order = load_order(&[z, a, m]).unwrap();
        assert_eq!(
            order.iter().map(PluginName::as_str).collect::<Vec<_>>(),
            vec!["alpha", "mid", "zebra"]
        );
    }

    #[test]
    fn load_order_dangling_consumer_single() {
        // A consumes "no-such-cap" which nobody fulfills.
        let a = manifest(&plugin_lua("plugin-a", &[], &["no-such-cap"]));
        let err = load_order(&[a]).unwrap_err();
        match err {
            ResolveError::DanglingConsumers(pairs) => {
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].0, PluginName::new("plugin-a").unwrap());
                assert_eq!(pairs[0].1, "no-such-cap");
            }
            other => panic!("expected DanglingConsumers, got {other:?}"),
        }
    }

    #[test]
    fn load_order_dangling_consumer_multiple_collected() {
        // A consumes "cap-x"; B consumes "cap-y"; neither is fulfilled.
        let a = manifest(&plugin_lua("plugin-a", &[], &["cap-x"]));
        let b = manifest(&plugin_lua("plugin-b", &[], &["cap-y"]));
        let err = load_order(&[a, b]).unwrap_err();
        match err {
            ResolveError::DanglingConsumers(pairs) => {
                assert_eq!(pairs.len(), 2, "both dangling consumers must be reported");
                // Deterministic order: (plugin-a, cap-x) before (plugin-b, cap-y).
                assert_eq!(pairs[0].0, PluginName::new("plugin-a").unwrap());
                assert_eq!(pairs[1].0, PluginName::new("plugin-b").unwrap());
            }
            other => panic!("expected DanglingConsumers, got {other:?}"),
        }
    }

    #[test]
    fn load_order_cycle_two_plugins() {
        // A fulfills "cap-a" and consumes "cap-b".
        // B fulfills "cap-b" and consumes "cap-a".
        // => mutual cycle.
        let a = manifest(&plugin_lua("plugin-a", &["cap-a"], &["cap-b"]));
        let b = manifest(&plugin_lua("plugin-b", &["cap-b"], &["cap-a"]));
        let err = load_order(&[a, b]).unwrap_err();
        match err {
            ResolveError::CapabilityCycle(members) => {
                let names: Vec<&str> = members.iter().map(PluginName::as_str).collect();
                assert!(names.contains(&"plugin-a"), "plugin-a in cycle");
                assert!(names.contains(&"plugin-b"), "plugin-b in cycle");
            }
            other => panic!("expected CapabilityCycle, got {other:?}"),
        }
    }

    #[test]
    fn load_order_library_plugin_fulfills_but_no_ui() {
        // "Library plugin": fulfills a capability but has no consumers itself.
        // Must load first (before the consumer) and must appear in the output.
        let lib = manifest(&plugin_lua(
            "password-manager-core",
            &["password-form-services"],
            &[],
        ));
        let ui = manifest(&plugin_lua(
            "password-manager-ui",
            &[],
            &["password-form-services"],
        ));

        let order = load_order(&[ui, lib]).unwrap();
        let lib_pos = order
            .iter()
            .position(|n| n.as_str() == "password-manager-core")
            .unwrap();
        let ui_pos = order
            .iter()
            .position(|n| n.as_str() == "password-manager-ui")
            .unwrap();
        assert!(
            lib_pos < ui_pos,
            "library plugin must load before UI consumer"
        );
    }

    #[test]
    fn load_order_multiple_fulfillers_all_before_consumer() {
        // Two plugins each fulfill "theme:provider"; a consumer needs it.
        // Both fulfillers must appear before the consumer.
        let f1 = manifest(&plugin_lua("theme-light", &["theme:provider"], &[]));
        let f2 = manifest(&plugin_lua("theme-dark", &["theme:provider"], &[]));
        let consumer = manifest(&plugin_lua("theme-consumer", &[], &["theme:provider"]));

        let order = load_order(&[consumer, f1, f2]).unwrap();
        let consumer_pos = order
            .iter()
            .position(|n| n.as_str() == "theme-consumer")
            .unwrap();
        let f1_pos = order
            .iter()
            .position(|n| n.as_str() == "theme-light")
            .unwrap();
        let f2_pos = order
            .iter()
            .position(|n| n.as_str() == "theme-dark")
            .unwrap();
        assert!(f1_pos < consumer_pos, "theme-light before theme-consumer");
        assert!(f2_pos < consumer_pos, "theme-dark before theme-consumer");
    }

    #[test]
    fn load_order_empty_set_returns_empty_vec() {
        let order = load_order(&[]).unwrap();
        assert!(order.is_empty());
    }

    #[test]
    fn load_order_is_deterministic_regardless_of_input_order() {
        // Same three independent plugins; order of manifests in the slice
        // must not affect the output.
        let a = manifest(&plugin_lua("aaa", &[], &[]));
        let b = manifest(&plugin_lua("bbb", &[], &[]));
        let c = manifest(&plugin_lua("ccc", &[], &[]));

        let order1 = load_order(&[a.clone(), b.clone(), c.clone()]).unwrap();
        let order2 = load_order(&[c.clone(), a.clone(), b.clone()]).unwrap();
        let order3 = load_order(&[b, c, a]).unwrap();

        assert_eq!(order1, order2, "input order must not affect output");
        assert_eq!(order1, order3, "input order must not affect output");
    }
}
