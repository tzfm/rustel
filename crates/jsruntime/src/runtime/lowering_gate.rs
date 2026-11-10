//! What may still change under a compiled callback.
//!
//! A lowered program stands for a JavaScript callback only while every slot
//! the callback reads when it runs holds what it held when the program was
//! compiled. The runtime cannot intercept a write to the Pattern prototype or
//! to the global object, but it reads every text before the text runs. The
//! gate records what each text can write, as the transpiler reports it, and
//! from then on refuses to compile against it. Host functions that write the
//! surface after the built-ins are sealed report to the gate too.
//!
//! The gate only closes. Every program holds the standing of each fact it
//! reads, and closing a fact marks its standing stale before the fact can
//! change. A stale program no longer stands for its callback, and the host
//! runs the callback instead. A pure graph built before the change still
//! applies the program and asks the host for the callback, so the runtime
//! queries every graph through its host while such a program is alive.

use super::*;
use rustel_core::callback_ir::Standing;
use std::collections::HashMap;
use std::sync::Arc;

/// Names whose replacement changes how every built-in method reads its
/// arguments: the intrinsics that argument sequencing and the composer
/// accessors read, and the names of the global object itself.
const SURFACE_NAMES: &[&str] = &[
    "Object",
    "Array",
    "isArray",
    "prototype",
    "globalThis",
    "window",
    "self",
];

/// The callee of every mini-notation literal.
const MINI_CALLEE: &str = "m";

/// The most method names the gate closes one by one. Past it, the gate
/// closes everything, so a long session of texts that write many names keeps
/// a bounded set.
const MAX_CLOSED_METHODS: usize = 4096;

/// The built-in surface one lowered program reads, as its JavaScript
/// callback reads it when it runs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LoweringDependencies {
    /// Pattern prototype methods the chain calls.
    pub(crate) methods: Vec<Box<str>>,
    /// The chain builds mini-notation through the global `m`.
    pub(crate) mini_callee: bool,
    /// The chain hands a string to a method, which reads it through the
    /// configurable string parser.
    pub(crate) raw_strings: bool,
}

/// The standings the gate issues to one kind of program, one per fact. A
/// fact keeps its standing until it closes, and a closed fact is never
/// issued again.
#[derive(Debug)]
struct Standings {
    /// Every program holds this one. It goes stale when the gate closes
    /// everything.
    everything: Arc<Standing>,
    mini_callee: Arc<Standing>,
    raw_strings: Arc<Standing>,
    methods: HashMap<Box<str>, Arc<Standing>>,
}

impl Default for Standings {
    fn default() -> Self {
        Self {
            everything: Standing::new(),
            mini_callee: Standing::new(),
            raw_strings: Standing::new(),
            methods: HashMap::new(),
        }
    }
}

impl Standings {
    /// The standings of every fact a program with these dependencies reads.
    fn issue(&mut self, dependencies: &LoweringDependencies) -> Vec<Arc<Standing>> {
        let mut standings = vec![self.everything.clone()];
        for method in &dependencies.methods {
            standings.push(self.methods.entry(method.clone()).or_default().clone());
        }
        if dependencies.mini_callee {
            standings.push(self.mini_callee.clone());
        }
        if dependencies.raw_strings {
            standings.push(self.raw_strings.clone());
        }
        standings
    }

    fn all(&self) -> impl Iterator<Item = &Arc<Standing>> {
        [&self.everything, &self.mini_callee, &self.raw_strings]
            .into_iter()
            .chain(self.methods.values())
    }
}

/// One fact the gate can close, marked in both kinds of standings.
#[derive(Clone, Copy)]
enum Fact<'a> {
    Everything,
    MiniCallee,
    RawStrings,
    Method(&'a str),
}

/// See the module documentation.
#[derive(Debug)]
pub(crate) struct LoweringGate {
    /// The built-in inventory is final. Before that, nothing is compiled and
    /// host writers report nothing: they are the engine installing itself.
    sealed: bool,
    unbounded: bool,
    methods: HashSet<Box<str>>,
    mini_callee: bool,
    raw_strings: bool,
    /// Standings for programs that run inside pure graphs, issued in Auto
    /// mode.
    lowered: Standings,
    /// Standings for programs that only the host runs, issued in DualRun
    /// mode.
    checked: Standings,
    /// A lowered standing went stale while a program held it, and no check
    /// since found every such program gone.
    stale_lowered_held: Cell<bool>,
    /// What the programs compiled so far read, and which unannounced changes
    /// to it have been found.
    used_methods: BTreeSet<Box<str>>,
    used_mini_callee: bool,
    issued_any: bool,
    found: BTreeSet<Box<str>>,
    found_intrinsics: bool,
}

impl Default for LoweringGate {
    fn default() -> Self {
        Self {
            sealed: false,
            unbounded: false,
            methods: HashSet::new(),
            mini_callee: false,
            raw_strings: false,
            lowered: Standings::default(),
            checked: Standings::default(),
            stale_lowered_held: Cell::new(false),
            used_methods: BTreeSet::new(),
            used_mini_callee: false,
            issued_any: false,
            found: BTreeSet::new(),
            found_intrinsics: false,
        }
    }
}

impl LoweringGate {
    /// Open the gate once the built-in inventory is final.
    pub(crate) fn seal(&mut self) {
        self.sealed = true;
    }

    /// Mark the standing of `fact` stale in both kinds of standings.
    fn mark(&mut self, fact: Fact<'_>) {
        for (standings, lowered) in [(&self.lowered, true), (&self.checked, false)] {
            let standing = match fact {
                Fact::Everything => Some(&standings.everything),
                Fact::MiniCallee => Some(&standings.mini_callee),
                Fact::RawStrings => Some(&standings.raw_strings),
                Fact::Method(name) => standings.methods.get(name),
            };
            if standing.is_some_and(Standing::mark_stale) && lowered {
                self.stale_lowered_held.set(true);
            }
        }
    }

    fn close_everything(&mut self) {
        if !self.unbounded {
            self.unbounded = true;
            self.mark(Fact::Everything);
        }
    }

    fn close_name(&mut self, name: &str) {
        if SURFACE_NAMES.contains(&name) {
            self.close_everything();
        } else if name == MINI_CALLEE {
            self.close_mini_callee_now();
        }
    }

    fn close_method_now(&mut self, name: &str) {
        if self.unbounded || self.methods.contains(name) {
            return;
        }
        if self.methods.len() >= MAX_CLOSED_METHODS {
            self.close_everything();
            self.methods.clear();
            return;
        }
        self.methods.insert(name.into());
        self.mark(Fact::Method(name));
    }

    fn close_mini_callee_now(&mut self) {
        if !self.mini_callee {
            self.mini_callee = true;
            self.mark(Fact::MiniCallee);
        }
    }

    /// Close everything a text can write, before the text runs.
    pub(crate) fn record_text(&mut self, writes: &rustel_transpiler::SurfaceWrites) {
        if writes.unbounded {
            self.close_everything();
        }
        if writes.declares_mini_callee {
            self.close_mini_callee_now();
        }
        for name in &writes.member_names {
            self.close_name(name);
            self.close_method_now(name);
        }
        for name in &writes.global_names {
            self.close_name(name);
        }
    }

    /// A host function is about to write the method slot `name`.
    pub(crate) fn close_method(&mut self, name: &str) {
        if self.sealed {
            self.close_name(name);
            self.close_method_now(name);
        }
    }

    /// A host function is about to write the global `name`.
    pub(crate) fn close_global(&mut self, name: &str) {
        if self.sealed {
            self.close_name(name);
        }
    }

    /// A host function is about to run code that no text announced.
    pub(crate) fn close_all(&mut self) {
        if self.sealed {
            self.close_everything();
        }
    }

    /// The configurable string parser is about to change.
    pub(crate) fn close_raw_strings(&mut self) {
        if self.sealed && !self.raw_strings {
            self.raw_strings = true;
            self.mark(Fact::RawStrings);
        }
    }

    /// Whether a program with these dependencies may stand for its callback.
    pub(crate) fn admits(&self, dependencies: &LoweringDependencies) -> bool {
        self.sealed
            && !self.unbounded
            && !(dependencies.mini_callee && self.mini_callee)
            && !(dependencies.raw_strings && self.raw_strings)
            && dependencies
                .methods
                .iter()
                .all(|method| !self.methods.contains(method))
    }

    /// The standings a newly compiled program holds, for a program that can
    /// run inside pure graphs or for one that only the host runs.
    pub(crate) fn issue(
        &mut self,
        dependencies: &LoweringDependencies,
        pure_graphs: bool,
    ) -> Vec<Arc<Standing>> {
        self.used_methods
            .extend(dependencies.methods.iter().cloned());
        self.used_mini_callee |= dependencies.mini_callee;
        self.issued_any = true;
        if pure_graphs {
            self.lowered.issue(dependencies)
        } else {
            self.checked.issue(dependencies)
        }
    }

    /// Whether a program that can run inside a pure graph is stale and still
    /// alive. A pure graph can then reach its JavaScript callback.
    pub(crate) fn stale_lowered_alive(&self) -> bool {
        if !self.stale_lowered_held.get() {
            return false;
        }
        // The gate holds one handle to each standing, and every other handle
        // belongs to a program.
        let alive = self
            .lowered
            .all()
            .any(|standing| standing.is_stale() && Arc::strong_count(standing) > 1);
        if !alive {
            self.stale_lowered_held.set(false);
        }
        alive
    }

    /// What issued programs read that the gate has not closed and no check
    /// has found changed: method names, the mini callee by its name, and
    /// whether the intrinsics are still to check.
    fn unchecked(&self) -> (Vec<Box<str>>, bool) {
        if self.unbounded {
            return (Vec::new(), false);
        }
        let mut names: Vec<Box<str>> = self
            .used_methods
            .iter()
            .filter(|name| !self.methods.contains(*name) && !self.found.contains(*name))
            .cloned()
            .collect();
        if self.used_mini_callee && !self.mini_callee {
            names.push(MINI_CALLEE.into());
        }
        (names, self.issued_any && !self.found_intrinsics)
    }
}

impl JsRuntime {
    /// Close what a text can write. Every text calls this before its first
    /// statement runs, whether or not it later fails.
    pub(super) fn record_surface_writes(&self, writes: &rustel_transpiler::SurfaceWrites) {
        self.callback_ir.gate.borrow_mut().record_text(writes);
    }

    /// Whether a compiled callback that no longer stands for its JavaScript
    /// is still alive. A pure graph can then reach the callback, so a query
    /// of any graph needs this runtime's host.
    pub fn compiled_callbacks_need_host(&self) -> bool {
        self.callback_ir.gate.borrow().stale_lowered_alive()
    }

    /// Compare every slot an issued program reads with the sealed built-in,
    /// after a text has run.
    ///
    /// Every change a text or a host function makes is announced to the gate
    /// before it happens. A change found here was not, so the gate closes
    /// against it now, and Auto mode reports it once: a compiled callback
    /// that ran between the change and this check used the built-in.
    pub(super) fn check_lowered_surface(&self) {
        let (names, intrinsics) = self.callback_ir.gate.borrow().unchecked();
        if names.is_empty() && !intrinsics {
            return;
        }
        let (changed, intrinsics_changed) = with_ctx(self, |ctx| {
            let changed: Vec<Box<str>> = names
                .into_iter()
                .filter(|name| {
                    if &**name == MINI_CALLEE {
                        !prove_mini_callee(&ctx)
                    } else {
                        prove_builtin_method(&ctx, name).is_none()
                    }
                })
                .collect();
            (changed, intrinsics && !prove_surface_intrinsics(&ctx))
        });
        if changed.is_empty() && !intrinsics_changed {
            return;
        }
        let report = self.callback_ir.mode.get() == PatternTransformIrMode::Auto;
        let mut gate = self.callback_ir.gate.borrow_mut();
        let mut logs = self.logs.borrow_mut();
        let mut warn = |what: &str| {
            if report && logs.len() < MAX_BUFFERED_LOGS {
                logs.push(format!(
                    "[warn] {what} was replaced in a way the runtime could not see in advance. A compiled callback that ran before the replacement was found used the built-in."
                ));
            }
        };
        if intrinsics_changed {
            gate.found_intrinsics = true;
            gate.close_everything();
            warn("Object or Array");
        }
        for name in changed {
            if &*name == MINI_CALLEE {
                gate.close_mini_callee_now();
            } else {
                gate.close_method_now(&name);
            }
            warn(&format!("'{name}'"));
            gate.found.insert(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependencies(
        methods: &[&str],
        mini_callee: bool,
        raw_strings: bool,
    ) -> LoweringDependencies {
        LoweringDependencies {
            methods: methods.iter().map(|name| (*name).into()).collect(),
            mini_callee,
            raw_strings,
        }
    }

    fn writes(member_names: &[&str], global_names: &[&str]) -> rustel_transpiler::SurfaceWrites {
        rustel_transpiler::SurfaceWrites {
            member_names: member_names.iter().map(|name| (*name).to_owned()).collect(),
            global_names: global_names.iter().map(|name| (*name).to_owned()).collect(),
            ..rustel_transpiler::SurfaceWrites::default()
        }
    }

    fn program(standings: Vec<Arc<Standing>>) -> rustel_core::callback_ir::PatternTransformProgram {
        rustel_core::callback_ir::PatternTransformProgram::new([])
            .expect("the identity program")
            .with_standings(standings)
    }

    #[test]
    fn nothing_is_admitted_before_the_seal() {
        let mut gate = LoweringGate::default();
        assert!(!gate.admits(&dependencies(&[], false, false)));
        gate.close_method("rev");
        gate.close_raw_strings();
        gate.seal();
        assert!(gate.admits(&dependencies(&["rev"], false, true)));
    }

    #[test]
    fn the_gate_only_closes_and_each_closed_fact_makes_its_programs_stale() {
        let mut gate = LoweringGate::default();
        gate.seal();
        let all = dependencies(&["rev", "fast"], true, true);
        assert!(gate.admits(&all));
        let both = program(gate.issue(&all, true));
        let fast = program(gate.issue(&dependencies(&["fast"], false, false), true));
        let checked = program(gate.issue(&all, false));
        assert!(both.is_current() && fast.is_current() && checked.is_current());

        gate.record_text(&writes(&["rev"], &[]));
        assert!(!gate.admits(&all));
        assert!(gate.admits(&dependencies(&["fast"], true, true)));
        assert!(!both.is_current());
        assert!(!checked.is_current());
        assert!(fast.is_current(), "a program that does not read rev");

        gate.record_text(&writes(&[], &["m"]));
        assert!(!gate.admits(&dependencies(&["fast"], true, false)));
        assert!(gate.admits(&dependencies(&["fast"], false, true)));

        gate.close_raw_strings();
        assert!(!gate.admits(&dependencies(&["fast"], false, true)));
        assert!(gate.admits(&dependencies(&["fast"], false, false)));
        assert!(fast.is_current());

        gate.record_text(&writes(&["isArray"], &[]));
        assert!(!gate.admits(&dependencies(&["fast"], false, false)));
        assert!(!fast.is_current());
    }

    #[test]
    fn surface_names_and_unbounded_texts_close_everything() {
        for text in [
            writes(&[], &["Array"]),
            writes(&["Object"], &[]),
            writes(&["prototype"], &[]),
            writes(&[], &["globalThis"]),
            rustel_transpiler::SurfaceWrites {
                unbounded: true,
                ..rustel_transpiler::SurfaceWrites::default()
            },
        ] {
            let mut gate = LoweringGate::default();
            gate.seal();
            let issued = program(gate.issue(&dependencies(&["fast"], false, false), true));
            gate.record_text(&text);
            assert!(
                !gate.admits(&dependencies(&["fast"], false, false)),
                "{text:?}"
            );
            assert!(!issued.is_current(), "{text:?}");
        }
        let mut gate = LoweringGate::default();
        gate.seal();
        gate.record_text(&rustel_transpiler::SurfaceWrites {
            declares_mini_callee: true,
            ..rustel_transpiler::SurfaceWrites::default()
        });
        assert!(gate.admits(&dependencies(&["fast"], false, true)));
        assert!(!gate.admits(&dependencies(&["fast"], true, false)));
    }

    #[test]
    fn closing_too_many_method_names_closes_everything() {
        let mut gate = LoweringGate::default();
        gate.seal();
        for index in 0..MAX_CLOSED_METHODS {
            gate.close_method(&format!("m{index}"));
        }
        assert!(gate.admits(&dependencies(&["fast"], false, false)));
        gate.close_method("one_more");
        assert!(!gate.admits(&dependencies(&["fast"], false, false)));
        assert!(gate.methods.is_empty());
    }

    #[test]
    fn a_stale_lowered_program_needs_the_host_while_it_is_alive() {
        let mut gate = LoweringGate::default();
        gate.seal();
        let lowered = program(gate.issue(&dependencies(&["rev"], false, false), true));
        let checked = program(gate.issue(&dependencies(&["fast"], false, false), false));
        gate.close_method("fast");
        assert!(!checked.is_current());
        assert!(
            !gate.stale_lowered_alive(),
            "a stale checked program runs only through the host"
        );
        gate.close_method("ply");
        assert!(!gate.stale_lowered_alive(), "no program reads ply");
        gate.close_method("rev");
        assert!(gate.stale_lowered_alive());
        drop(lowered);
        assert!(!gate.stale_lowered_alive());
    }

    #[test]
    fn only_unclosed_reads_are_checked_after_a_text() {
        let mut gate = LoweringGate::default();
        gate.seal();
        assert_eq!(gate.unchecked(), (Vec::new(), false));
        let _program = program(gate.issue(&dependencies(&["rev", "fast"], true, false), true));
        assert_eq!(
            gate.unchecked(),
            (
                vec![Box::from("fast"), Box::from("rev"), Box::from("m")],
                true
            )
        );
        gate.close_method("rev");
        gate.found.insert("fast".into());
        gate.found_intrinsics = true;
        assert_eq!(gate.unchecked(), (vec![Box::from("m")], false));
        gate.close_all();
        assert_eq!(gate.unchecked(), (Vec::new(), false));
    }
}
