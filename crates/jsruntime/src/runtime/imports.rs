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
    pub fn run_import(
        &self,
        specifier: &str,
        source: &str,
        options: &rustel_transpiler::TranspileOptions,
        budget: std::time::Duration,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<ScoreEffects>, QueryError> {
        let queued = Rc::new(RefCell::new(Vec::<String>::new()));
        let setup = |source: &str| {
            self.evaluate_prelude_with_effects_cancellable(source, options, budget, cancellation)
                .map(|(_, effects)| effects)
        };
        let effects = with_ctx(&self.ctx, |ctx| {
            let globals = ctx.globals();
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
            effects
        })?;
        self.loaded_imports
            .borrow_mut()
            .insert(specifier.to_owned());
        Ok(effects)
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
