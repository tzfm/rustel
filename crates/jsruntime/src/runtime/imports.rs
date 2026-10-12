//! Script imports: the host fetches a script, and this realm runs it as
//! setup code.

use super::*;

/// The most `strudelMirror.repl.evaluate` calls that one import makes.
const MAX_REPL_EVALUATIONS: usize = 8;

/// The most code, in bytes, that one `strudelMirror.repl.evaluate` call
/// takes. The queue lives outside the QuickJS heap budget.
const MAX_REPL_CODE_BYTES: usize = 1024 * 1024;

impl JsRuntime {
    /// Whether this realm has run the script that `specifier` names.
    pub fn import_loaded(&self, specifier: &str) -> bool {
        self.loaded_imports.borrow().contains(specifier)
    }

    /// Run a fetched script as setup code. From then on `import(specifier)`
    /// resolves, to an empty module.
    ///
    /// The script is JavaScript as written: a double-quoted string is not
    /// mini-notation, as in a browser import. While the script runs,
    /// `strudelMirror.repl.evaluate(code)` is defined. Each `code` runs after
    /// the script as setup code with mini-notation. A script that carries
    /// its helpers as text compiles them with this call.
    ///
    /// Definitions made before a failure stay, as for any setup file.
    ///
    /// With [`ImportScope::Score`] the realm tracks the names the script
    /// sets, and [`Self::show_imports`] picks their meaning for each score.
    pub fn run_import(
        &self,
        specifier: &str,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
        scope: ImportScope,
    ) -> Result<Vec<ScoreEffects>, QueryError> {
        let queued = Rc::new(RefCell::new(Vec::<String>::new()));
        let setup = |source: &str| {
            self.evaluate_prelude_with_effects_cancellable(source, options, budget, cancellation)
                .map(|(_, effects)| effects)
        };
        let effects = with_ctx(&self.ctx, |ctx| {
            let globals = ctx.globals();
            let before = match scope {
                ImportScope::Score => Some(import_surface::snapshot(&ctx).map_err(script_error)?),
                ImportScope::Shared => None,
            };
            let outer: rquickjs::Value = globals.get("strudelMirror").map_err(script_error)?;
            globals
                .set("strudelMirror", repl_mirror(&ctx, queued.clone())?)
                .map_err(script_error)?;
            let run = || {
                // The comment keeps the script's first line on line 1.
                let mut effects = vec![setup(&format!("/* mini-off */{source}"))?];
                let mut evaluations = 0;
                loop {
                    let code = queued.take();
                    if code.is_empty() {
                        return Ok(effects);
                    }
                    for code in code {
                        evaluations += 1;
                        if evaluations > MAX_REPL_EVALUATIONS {
                            return Err(QueryError::Message(format!(
                                "the script calls strudelMirror.repl.evaluate more than {MAX_REPL_EVALUATIONS} times"
                            )));
                        }
                        effects.push(setup(&code)?);
                    }
                }
            };
            let effects = run();
            globals.set("strudelMirror", outer).map_err(script_error)?;
            if let (Ok(_), Some(before)) = (&effects, before) {
                self.import_surface
                    .borrow_mut()
                    .record(&ctx, specifier, &before)
                    .map_err(script_error)?;
            }
            effects
        })?;
        self.loaded_imports
            .borrow_mut()
            .insert(specifier.to_owned());
        Ok(effects)
    }

    /// Give a score the names of its own imports. `specifiers` are the
    /// imports in the score text, in order.
    ///
    /// Only a name with two meanings changes: one that two imports set, or
    /// one an import took from the surface. The call costs a list compare
    /// when the score before had the same imports.
    pub fn show_imports(&self, specifiers: &[String]) {
        let listed = self.import_surface.borrow().ids(specifiers);
        self.show_import_ids(listed);
    }

    /// Give the names back to the score of the last install. A score that
    /// failed leaves the playing score its own imports.
    pub fn show_last_good_imports(&self) {
        let listed = self.import_surface.borrow().last_good();
        self.show_import_ids(listed);
    }

    /// Let every score see the names of an import a score loaded. A prebake
    /// names the import now.
    pub fn share_import(&self, specifier: &str) {
        with_ctx(&self.ctx, |ctx| {
            self.import_surface.borrow_mut().share(&ctx, specifier);
        });
    }

    pub(super) fn keep_last_good_imports(&self) {
        self.import_surface.borrow_mut().keep_last_good();
    }

    fn show_import_ids(&self, listed: Vec<usize>) {
        let mut surface = self.import_surface.borrow_mut();
        if surface.shows(&listed) {
            return;
        }
        with_ctx(&self.ctx, |ctx| {
            // A name the last score registered goes back first. The swap
            // then replaces the value the engine or an import gave the name.
            if restore_registration_surface(&ctx).is_err() {
                let _ = ctx.catch();
            }
            surface.show(&ctx, listed);
        });
    }
}

fn script_error(error: rquickjs::Error) -> QueryError {
    QueryError::Message(error.to_string())
}

/// `{ repl: { evaluate(code) } }`, where `evaluate` queues `code`.
fn repl_mirror<'js>(
    ctx: &Ctx<'js>,
    queued: Rc<RefCell<Vec<String>>>,
) -> Result<rquickjs::Object<'js>, QueryError> {
    let build = || -> rquickjs::Result<rquickjs::Object<'js>> {
        let evaluate = Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, code: rquickjs::Value<'js>| -> rquickjs::Result<()> {
                let code = rquickjs::Coerced::<String>::from_js(&ctx, code)?.0;
                let mut queued = queued.borrow_mut();
                if queued.len() >= MAX_REPL_EVALUATIONS || code.len() > MAX_REPL_CODE_BYTES {
                    return Err(rquickjs::Exception::throw_range(
                        &ctx,
                        "strudelMirror.repl.evaluate takes 8 calls of 1 MiB of code at most",
                    ));
                }
                queued.push(code);
                Ok(())
            },
        )?;
        let repl = rquickjs::Object::new(ctx.clone())?;
        repl.set("evaluate", evaluate)?;
        let mirror = rquickjs::Object::new(ctx.clone())?;
        mirror.set("repl", repl)?;
        Ok(mirror)
    };
    build().map_err(script_error)
}
